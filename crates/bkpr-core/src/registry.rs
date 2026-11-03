//! Backend-owned workspace intent. Persisted metadata is not PTY recovery.
//! Atomic tempfile/fsync/rename follows main's recovery.rs, without its session/KDL assumptions.
use crate::{Pane, SpawnSpec};
mod agents;
mod automatic;
mod capture;
mod context;
mod directories;
mod project;
mod recovery;
pub(crate) mod sessions;
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::*;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use uuid::Uuid;
const STORAGE_VERSION: u32 = 4;
const MAX_RECORD: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    registry: RegistrySnapshot,
    recoveries: BTreeMap<String, HarnessRecovery>,
    directories: BTreeMap<String, directories::Directory>,
    automatic: BTreeMap<String, HarnessRecovery>,
}
// Persistence is an explicit intent whitelist, not serialization of live facts.
// Borrow the record: a metadata write need not copy every foreground observation.
#[derive(Serialize)]
struct DurableRecord<'a> {
    version: u32,
    registry: DurableRegistry<'a>,
    recoveries: &'a BTreeMap<String, HarnessRecovery>,
    directories: &'a BTreeMap<String, directories::Directory>,
    automatic: &'a BTreeMap<String, HarnessRecovery>,
}
#[derive(Serialize)]
struct DurableRegistry<'a> {
    revision: u64,
    workspaces: &'a [Workspace],
    panes: DurablePanes<'a>,
}
struct DurablePanes<'a>(&'a [PaneInfo]);
#[derive(Serialize)]
struct DurablePane<'a> {
    id: &'a str,
    workspace_id: &'a Option<String>,
    window_id: &'a Option<String>,
    title: &'a str,
    status: PaneStatus,
    exit_code: Option<i32>,
    #[serde(with = "bkpr_protocol::path_wire")]
    cwd: &'a Path,
    #[serde(with = "bkpr_protocol::path_wire")]
    home: &'a Path,
    isolated_environment: bool,
}
impl Serialize for DurablePanes<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for pane in self.0 {
            sequence.serialize_element(&DurablePane {
                id: &pane.id,
                workspace_id: &pane.workspace_id,
                window_id: &pane.window_id,
                title: &pane.title,
                status: pane.status,
                exit_code: pane.exit_code,
                cwd: &pane.cwd,
                home: &pane.home,
                isolated_environment: pane.isolated_environment,
            })?;
        }
        sequence.end()
    }
}
struct State {
    record: Record,
    live: BTreeMap<String, Pane>,
    spawning: usize,
    spawn_workspaces: BTreeSet<String>,
    spawn_workspace_names: BTreeMap<String, String>,
    closing_workspaces: BTreeSet<String>,
    closing_panes: BTreeSet<String>,
    closing_document_panes: BTreeSet<String>,
    resuming: BTreeMap<String, recovery::ResumeAttempt>,
    bootstrap_started: bool,
    restore_batch: bool,
    pending_directories: BTreeMap<String, directories::Pending>,
    failed_restores: BTreeSet<String>,
    closing: bool,
    durability_error: Option<String>,
}
struct Inner {
    root: PathBuf,
    session: SessionInfo,
    clients: Mutex<BTreeMap<String, Arc<sessions::ClientLease>>>,
    state: Mutex<State>,
    // Serialize durable writers, never pane I/O. Lock order: transaction, then state.
    transaction: Mutex<()>,
    view_budget: Arc<crate::authority::ViewBudget>,
    captures: Mutex<crate::harness::Captures>,
    agents: Arc<agents::Agents>,
    spawn_environment: Mutex<Option<crate::harness::SpawnEnvironment>>,
    ssh_agent: Mutex<Option<Arc<crate::ssh_agent::Relay>>>,
    ssh_agent_sequence: std::sync::atomic::AtomicU64,
    ssh_agent_selected: Mutex<Option<String>>,
    observer_started: std::sync::atomic::AtomicBool,
    session_end: std::sync::atomic::AtomicBool,
    session_end_waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    status_wake: crossbeam_channel::Sender<()>,
    status_events: crossbeam_channel::Receiver<()>,
    project_wake: crossbeam_channel::Sender<()>,
    project_cancel: Arc<std::sync::atomic::AtomicBool>,
    project_worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    public_handles: std::sync::atomic::AtomicUsize,
    changed: Condvar,
    _lease: Lease,
    #[cfg(test)]
    spawn_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    persist_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    capture_admission_wait: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    #[cfg(test)]
    control_resume_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.project_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.project_wake.try_send(());
        // The retention worker owns only Agents, not the registry lease. Drain its
        // writer and close admission before releasing ownership of the durable root.
        let _ = self.agents.shutdown();
        if let Some(relay) = self.ssh_agent.get_mut().unwrap().take() {
            let _ = relay.shutdown();
        }
    }
}
#[cfg(test)]
struct SpawnBarrier {
    entered: std::sync::mpsc::SyncSender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
enum SpawnTarget {
    Append {
        workspace_id: Option<String>,
        window_id: Option<String>,
        expected_revision: Option<u64>,
    },
    Reopen {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        pane_id: String,
    },
    Restore {
        workspace_id: String,
        window_id: String,
        pane_id: String,
    },
    CreateWorkspace {
        workspace_id: String,
        window_id: String,
        name: Option<String>,
        bootstrap: bool,
    },
    CreateWindow {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        name: Option<String>,
    },
}
impl SpawnTarget {
    fn workspace_id(&self) -> Option<&str> {
        match self {
            Self::Append { workspace_id, .. } => workspace_id.as_deref(),
            Self::Reopen { workspace_id, .. }
            | Self::Restore { workspace_id, .. }
            | Self::CreateWorkspace { workspace_id, .. }
            | Self::CreateWindow { workspace_id, .. } => Some(workspace_id),
        }
    }
    fn window_id(&self) -> Option<&str> {
        match self {
            Self::Append { window_id, .. } => window_id.as_deref(),
            Self::Reopen { window_id, .. }
            | Self::Restore { window_id, .. }
            | Self::CreateWorkspace { window_id, .. }
            | Self::CreateWindow { window_id, .. } => Some(window_id),
        }
    }
    fn check(&self, state: &State) -> Result<()> {
        let r = &state.record.registry;
        match self {
            Self::Append {
                workspace_id,
                window_id,
                expected_revision,
            } => check_spawn_window(
                r,
                workspace_id.as_deref(),
                window_id.as_deref(),
                *expected_revision,
            ),
            Self::Reopen {
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
            } => {
                check_spawn_window(
                    r,
                    Some(workspace_id),
                    Some(window_id),
                    Some(*expected_revision),
                )?;
                let pane = r
                    .panes
                    .iter()
                    .find(|pane| &pane.id == pane_id)
                    .context("unknown pane")?;
                ensure!(
                    pane.workspace_id.as_deref() == Some(workspace_id)
                        && pane.window_id.as_deref() == Some(window_id),
                    "pane belongs to another window"
                );
                ensure!(
                    pane.status != PaneStatus::Running
                        && state.live.get(pane_id).is_none_or(|pane| pane
                            .shared
                            .finished
                            .load(std::sync::atomic::Ordering::Acquire)),
                    "cannot reopen a live pane; open a new pane instead"
                );
                let window = find_window(r, workspace_id, window_id)?;
                ensure!(
                    window
                        .layout
                        .as_ref()
                        .is_some_and(|layout| layout_references(layout, pane_id)),
                    "window layout does not reference pane"
                );
                Ok(())
            }
            Self::Restore {
                workspace_id,
                window_id,
                pane_id,
            } => {
                find_window(r, workspace_id, window_id)?;
                let pane = r
                    .panes
                    .iter()
                    .find(|pane| &pane.id == pane_id)
                    .context("unknown saved pane")?;
                ensure!(
                    pane.workspace_id.as_ref() == Some(workspace_id)
                        && pane.window_id.as_ref() == Some(window_id),
                    "saved pane membership changed"
                );
                ensure!(
                    pane.status != PaneStatus::Running
                        && state.live.get(pane_id).is_none_or(|pane| pane
                            .shared
                            .finished
                            .load(std::sync::atomic::Ordering::Acquire)),
                    "saved execution is still running"
                );
                Ok(())
            }
            Self::CreateWorkspace {
                workspace_id, name, ..
            } => {
                ensure!(
                    state.closing_workspaces.is_empty(),
                    "session busy: workspace close in progress"
                );
                let pending = state
                    .spawn_workspaces
                    .iter()
                    .filter(|id| *id != workspace_id && !r.workspaces.iter().any(|w| &w.id == *id))
                    .count();
                ensure!(
                    r.workspaces.len() + pending < MAX_WORKSPACES,
                    "workspace limit"
                );
                if let Some(name) = name.as_deref().filter(|name| !name.trim().is_empty()) {
                    valid_name(name)?;
                }
                Ok(())
            }
            Self::CreateWindow {
                workspace_id,
                expected_revision,
                name,
                ..
            } => {
                let w = r
                    .workspaces
                    .iter()
                    .find(|w| &w.id == workspace_id)
                    .context("unknown workspace")?;
                ensure!(
                    w.revision == *expected_revision,
                    "stale workspace revision: current {}",
                    w.revision
                );
                ensure!(w.windows.len() < MAX_PANE_RECORDS, "window limit");
                if let Some(name) = name.as_deref().filter(|name| !name.trim().is_empty()) {
                    valid_name(name)?;
                }
                Ok(())
            }
        }
    }
}
struct SpawnReservation {
    inner: Arc<Inner>,
    workspace_id: Option<String>,
    resume_pane: Option<String>,
    committed: bool,
}
impl Drop for SpawnReservation {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        state.spawning -= 1;
        if !self.committed
            && let Some(id) = &self.resume_pane
        {
            state.resuming.remove(id);
        }
        if let Some(workspace_id) = &self.workspace_id {
            state.spawn_workspaces.remove(workspace_id);
            state.spawn_workspace_names.remove(workspace_id);
        }
        self.inner.changed.notify_all();
        drop(state);
        let registry = Registry {
            inner: self.inner.clone(),
            fresh: false,
            lifecycle_owner: false,
        };
        let _ = registry.drain_completions();
    }
}
pub struct Registry {
    inner: Arc<Inner>,
    fresh: bool,
    lifecycle_owner: bool,
}
impl Clone for Registry {
    fn clone(&self) -> Self {
        if self.lifecycle_owner {
            self.inner
                .public_handles
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Self {
            inner: self.inner.clone(),
            fresh: self.fresh,
            lifecycle_owner: self.lifecycle_owner,
        }
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        if self.lifecycle_owner
            && self
                .inner
                .public_handles
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
                == 1
        {
            // A worker's temporary Inner reference is not an owner of the state
            // root. Join while this last public handle still retains the lease.
            self.stop_projects();
        }
    }
}

impl Registry {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_named(root, DEFAULT_SESSION_NAME)
    }
    pub fn open_named(root: &Path, name: &str) -> Result<Self> {
        valid_session_name(name)?;
        ensure!(root.is_absolute(), "state root must be absolute");
        private_directory(root)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("owner.lock"))?;
        check_file(&lease)?;
        lease
            .try_lock_exclusive()
            .context("state root already owned by another daemon")?;
        let lease = Lease(lease);
        let session = sessions::open_session(root, name)?;
        let stored = read_record(root)?;
        let fresh = stored.is_none();
        let mut record = stored.unwrap_or(Record {
            version: STORAGE_VERSION,
            recoveries: BTreeMap::new(),
            directories: BTreeMap::new(),
            automatic: BTreeMap::new(),
            registry: RegistrySnapshot {
                revision: 0,
                workspaces: Vec::new(),
                panes: Vec::new(),
            },
        });
        validate_record(&record)?;
        let mut stopped = false;
        for pane in &mut record.registry.panes {
            stopped |= pane.foreground_cwd.is_some()
                || pane.harness.is_some()
                || pane.harness_status.is_some()
                || pane.project.is_some();
            pane.foreground_cwd = None;
            pane.harness = None;
            pane.harness_status = None;
            pane.project = None;
            if pane.status != PaneStatus::Stopped {
                // No persisted pane has a terminal handle after daemon restart.
                // Retain a known historical exit code, but never advertise an attachable scene.
                if pane.status == PaneStatus::Running {
                    pane.exit_code = None;
                }
                pane.status = PaneStatus::Stopped;
                stopped = true;
            }
        }
        if stopped {
            record.registry.revision = next_revision(record.registry.revision)?;
        }
        if stopped || fresh {
            persist(root, &record)?;
        }
        let (status_wake, status_events) = crossbeam_channel::bounded(1);
        let (project_wake, project_events) = crossbeam_channel::bounded(1);
        let agents = Arc::new(agents::Agents::open(root)?);
        let registry = Self {
            inner: Arc::new(Inner {
                root: root.to_owned(),
                session,
                clients: Mutex::new(BTreeMap::new()),
                state: Mutex::new(State {
                    record,
                    live: BTreeMap::new(),
                    spawning: 0,
                    spawn_workspaces: BTreeSet::new(),
                    spawn_workspace_names: BTreeMap::new(),
                    closing_workspaces: BTreeSet::new(),
                    closing_panes: BTreeSet::new(),
                    closing_document_panes: BTreeSet::new(),
                    resuming: BTreeMap::new(),
                    bootstrap_started: false,
                    restore_batch: false,
                    pending_directories: BTreeMap::new(),
                    failed_restores: BTreeSet::new(),
                    closing: false,
                    durability_error: None,
                }),
                transaction: Mutex::new(()),
                view_budget: Arc::default(),
                captures: Mutex::new(crate::harness::Captures::default()),
                agents,
                spawn_environment: Mutex::new(None),
                ssh_agent: Mutex::new(None),
                ssh_agent_sequence: std::sync::atomic::AtomicU64::new(0),
                ssh_agent_selected: Mutex::new(None),
                observer_started: std::sync::atomic::AtomicBool::new(false),
                session_end: std::sync::atomic::AtomicBool::new(false),
                session_end_waker: Mutex::new(None),
                status_wake,
                status_events,
                project_wake,
                project_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                project_worker: Mutex::new(None),
                public_handles: std::sync::atomic::AtomicUsize::new(1),
                changed: Condvar::new(),
                _lease: lease,
                #[cfg(test)]
                spawn_barrier: Mutex::new(None),
                #[cfg(test)]
                persist_barrier: Mutex::new(None),
                #[cfg(test)]
                capture_admission_wait: Mutex::new(None),
                #[cfg(test)]
                control_resume_barrier: Mutex::new(None),
            }),
            fresh,
            lifecycle_owner: true,
        };
        registry.start_agent_maintenance()?;
        registry.start_projects(project_events)?;
        Ok(registry)
    }
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn test_pause_next_spawn(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
        *self.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
            entered,
            resume: Mutex::new(resume_rx),
        }));
        (entered_rx, resume)
    }
    pub fn session_end_requested(&self) -> bool {
        self.inner
            .session_end
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn set_session_end_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.inner.session_end_waker.lock().unwrap() = Some(waker.clone());
        if self.session_end_requested() {
            waker();
        }
    }
    fn notify_session_end(&self) {
        if self.session_end_requested() {
            let waker = self.inner.session_end_waker.lock().unwrap().clone();
            if let Some(waker) = waker {
                waker();
            }
        }
    }
    pub fn snapshot(&self) -> Result<RegistrySnapshot> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        Ok(state.record.registry.clone())
    }
    fn mutate(
        &self,
        edit: impl FnOnce(&mut RegistrySnapshot) -> Result<()>,
    ) -> Result<RegistrySnapshot> {
        self.mutate_inner(None, edit)
    }
    fn mutate_workspace(
        &self,
        workspace_id: &str,
        edit: impl FnOnce(&mut RegistrySnapshot) -> Result<()>,
    ) -> Result<RegistrySnapshot> {
        self.mutate_inner(Some(workspace_id), edit)
    }
    fn mutate_inner(
        &self,
        workspace_id: Option<&str>,
        edit: impl FnOnce(&mut RegistrySnapshot) -> Result<()>,
    ) -> Result<RegistrySnapshot> {
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, "daemon is shutting down");
            ensure!(!self.session_end_requested(), "session has ended");
            if let Some(workspace_id) = workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            state.record.clone()
        };
        edit(&mut next.registry)?;
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |_| {})
    }
    pub fn request(&self, request: Request) -> Result<Value> {
        self.request_for_client(request, None)
    }
    fn request_for_client(
        &self,
        request: Request,
        client: Option<Arc<sessions::ClientLease>>,
    ) -> Result<Value> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        if let Some(client) = &client {
            client.ensure_active()?;
        }
        match request.operation {
            Operation::SessionInfo => Ok(json!({"session":self.inner.session})),
            Operation::ClientRegister { .. }
            | Operation::Client { .. }
            | Operation::ClientActivate { .. }
            | Operation::ClientDetach => {
                bail!("client operations require authenticated attachment transport")
            }
            Operation::Inspect => Ok(json!({"registry":self.snapshot()?})),
            Operation::Context {
                pane_id: Some(pane_id),
            } => self.pane_context(&pane_id),
            Operation::Context { pane_id: None } => {
                bail!("automatic pane context requires OS-authenticated transport")
            }
            Operation::RecoveryList => self.recovery_list(),
            Operation::PaneResume { pane_id, expected } => self.resume(&pane_id, expected),
            Operation::HarnessBegin { .. }
            | Operation::HarnessEvent { .. }
            | Operation::HarnessContext { .. }
            | Operation::Agent { .. } => {
                bail!("capture operations require OS-authenticated transport")
            }
            Operation::Wait { after, wait_ms } => {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(
                    after <= state.record.registry.revision,
                    "future registry revision"
                );
                let (state, _) = self
                    .inner
                    .changed
                    .wait_timeout_while(state, Duration::from_millis(wait_ms.min(1000)), |s| {
                        s.record.registry.revision == after
                            && !s.closing
                            && s.durability_error.is_none()
                    })
                    .unwrap();
                healthy(&state)?;
                Ok(json!({"registry":state.record.registry}))
            }
            Operation::WorkspaceCreate { name, initial } => {
                let id = Uuid::new_v4().to_string();
                if let Some(spec) = initial {
                    return self.spawn(
                        SpawnTarget::CreateWorkspace {
                            workspace_id: id,
                            window_id: Uuid::new_v4().to_string(),
                            name,
                            bootstrap: false,
                        },
                        spec,
                    );
                }
                let registry = self.mutate(|r| {
                    let state = self.inner.state.lock().unwrap();
                    ensure!(
                        state.closing_workspaces.is_empty(),
                        "session busy: workspace close in progress"
                    );
                    let pending = state
                        .spawn_workspaces
                        .iter()
                        .filter(|id| !r.workspaces.iter().any(|w| &w.id == *id))
                        .count();
                    ensure!(
                        r.workspaces.len() + pending < MAX_WORKSPACES,
                        "workspace limit"
                    );
                    let name = available_name(
                        name.as_deref(),
                        "Workspace",
                        r.workspaces
                            .iter()
                            .map(|w| w.name.as_str())
                            .chain(state.spawn_workspace_names.values().map(String::as_str)),
                    )?;
                    drop(state);
                    r.workspaces.push(Workspace {
                        id: id.clone(),
                        name,
                        revision: 1,
                        windows: Vec::new(),
                    });
                    Ok(())
                })?;
                Ok(json!({"registry":registry,"workspace_id":id}))
            }
            Operation::WorkspaceRename {
                workspace_id,
                expected_revision,
                name,
            } => {
                valid_name(&name)?;
                let registry = self.mutate_workspace(&workspace_id, |r| {
                    let w = workspace(r, &workspace_id, expected_revision)?;
                    w.name = name;
                    w.revision = next_revision(w.revision)?;
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::WorkspaceClose {
                workspace_id,
                expected_revision,
            } => self.close_documents(&workspace_id, None, expected_revision),
            Operation::WindowCreate {
                workspace_id,
                expected_revision,
                name,
                spec,
            } => self.spawn(
                SpawnTarget::CreateWindow {
                    workspace_id,
                    window_id: Uuid::new_v4().to_string(),
                    expected_revision,
                    name,
                },
                spec,
            ),
            Operation::WindowRename {
                workspace_id,
                window_id,
                expected_revision,
                name,
            } => {
                valid_name(&name)?;
                let registry = self.mutate_workspace(&workspace_id, |r| {
                    let w = r
                        .workspaces
                        .iter_mut()
                        .find(|w| w.id == workspace_id)
                        .context("unknown workspace")?;
                    let window = window(w, &window_id, expected_revision)?;
                    window.name = name;
                    window.revision = next_revision(window.revision)?;
                    w.revision = next_revision(w.revision)?;
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::WindowClose {
                workspace_id,
                window_id,
                expected_revision,
            } => self.close_documents(&workspace_id, Some(&window_id), expected_revision),
            Operation::LayoutUpdate {
                workspace_id,
                window_id,
                expected_revision,
                layout,
            } => {
                let registry = self.mutate_workspace(&workspace_id, |r| {
                    if let Some(node) = &layout {
                        validate_owned_layout(node, &r.panes, &workspace_id, &window_id)?;
                    }
                    let w = r
                        .workspaces
                        .iter_mut()
                        .find(|w| w.id == workspace_id)
                        .context("unknown workspace")?;
                    let window = window(w, &window_id, expected_revision)?;
                    window.layout = layout;
                    window.revision = next_revision(window.revision)?;
                    w.revision = next_revision(w.revision)?;
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::PaneSpawn {
                workspace_id,
                window_id,
                expected_revision,
                spec,
            } => self.spawn(
                SpawnTarget::Append {
                    workspace_id,
                    window_id,
                    expected_revision,
                },
                spec,
            ),
            Operation::PaneReopen {
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
                spec,
            } => self.spawn(
                SpawnTarget::Reopen {
                    workspace_id,
                    window_id,
                    expected_revision,
                    pane_id,
                },
                spec,
            ),
            Operation::PaneForget { pane_id } => self.forget(&pane_id),
            Operation::PaneRename { pane_id, title } => {
                valid_name(&title)?;
                let workspace_id = self
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|p| p.id == pane_id)
                    .context("unknown pane")?
                    .workspace_id
                    .clone();
                let registry = self.mutate_inner(workspace_id.as_deref(), |r| {
                    let pane = r
                        .panes
                        .iter_mut()
                        .find(|p| p.id == pane_id)
                        .context("unknown pane")?;
                    pane.title = title;
                    if let (Some(workspace_id), Some(window_id)) =
                        (&pane.workspace_id, &pane.window_id)
                    {
                        let w = r
                            .workspaces
                            .iter_mut()
                            .find(|w| &w.id == workspace_id)
                            .context("unknown workspace")?;
                        let window = w
                            .windows
                            .iter_mut()
                            .find(|w| &w.id == window_id)
                            .context("unknown window")?;
                        window.revision = next_revision(window.revision)?;
                        w.revision = next_revision(w.revision)?;
                    }
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::PaneTerminate { pane_id } => {
                let pane = {
                    let state = self.inner.state.lock().unwrap();
                    healthy(&state)?;
                    ensure!(
                        state.record.registry.panes.iter().any(|p| p.id == pane_id),
                        "unknown pane"
                    );
                    state.live.get(&pane_id).cloned().context("pane execution unavailable; reopen it or explicitly close its window/workspace")?
                };
                pane.request(PaneRequest::Shutdown)?;
                self.record_completion(&pane_id)?;
                Ok(json!({"registry":self.snapshot()?,"pane_id":pane_id}))
            }
            Operation::Pane { pane_id, request } => {
                ensure!(
                    !matches!(request, PaneRequest::Shutdown),
                    "use pane_terminate for lifecycle control"
                );
                if matches!(&request, PaneRequest::Attach { .. }) {
                    self.expire_retired_views()?;
                }
                let pane = {
                    let state = self.inner.state.lock().unwrap();
                    healthy(&state)?;
                    state
                        .live
                        .get(&pane_id)
                        .cloned()
                        .context("pane is stopped; terminal contents were not recovered")?
                };
                let terminating = matches!(request, PaneRequest::Terminate { .. });
                let inspection = matches!(
                    request,
                    PaneRequest::Inspect
                        | PaneRequest::Poll { .. }
                        | PaneRequest::Select { .. }
                        | PaneRequest::CopySelection { .. }
                        | PaneRequest::Search { .. }
                );
                let result = pane.request_for_client(request, client.clone())?;
                if inspection && let Some(client) = &client {
                    client.ensure_active()?;
                }
                if terminating {
                    self.record_completion(&pane_id)?;
                }
                Ok(result)
            }
            Operation::Shutdown => {
                self.shutdown()?;
                Ok(json!({"shutdown":true}))
            }
        }
    }
    fn spawn(&self, target: SpawnTarget, spec: PaneSpawnSpec) -> Result<Value> {
        self.spawn_inner(target, spec, None, None)
    }
    fn spawn_inner(
        &self,
        mut target: SpawnTarget,
        spec: PaneSpawnSpec,
        resume: Option<recovery::ResumeAttempt>,
        origin: Option<(&crate::harness::Launch, &crate::harness::Peer)>,
    ) -> Result<Value> {
        valid_size(spec.size)?;
        ensure!(
            !spec.argv.is_empty()
                && spec.argv.len() <= 256
                && spec.argv.iter().map(String::len).sum::<usize>() <= 128 * 1024,
            "invalid or excessive argv"
        );
        let title = spec.title.clone().unwrap_or_else(|| {
            Path::new(&spec.argv[0])
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        valid_name(&title)?;
        let workspace_id = target.workspace_id().map(str::to_owned);
        let id = match &target {
            SpawnTarget::Restore { pane_id, .. } => pane_id.clone(),
            _ => Uuid::new_v4().to_string(),
        };
        let info = PaneInfo {
            id: id.clone(),
            workspace_id: workspace_id.clone(),
            window_id: target.window_id().map(str::to_owned),
            title,
            status: PaneStatus::Running,
            exit_code: None,
            cwd: spec.cwd.clone(),
            home: spec.home.clone(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: Some(project::loading(&spec.cwd)),
            isolated_environment: spec.isolated_environment,
        };
        if let Some((launch, peer)) = origin {
            crate::harness::validate_callback(peer, launch)?;
        }
        let workspace = {
            let _transaction = self.inner.transaction.lock().unwrap();
            if let Some((launch, _)) = origin {
                self.authorize_resume_origin(launch)?;
            }
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, "daemon is shutting down");
            ensure!(!self.session_end_requested(), "session has ended");
            if matches!(
                target,
                SpawnTarget::CreateWorkspace {
                    bootstrap: true,
                    ..
                }
            ) && state.record.registry.panes.iter().any(|pane| {
                pane.status == PaneStatus::Running
                    && state.live.get(&pane.id).is_some_and(|pane| {
                        !pane
                            .shared
                            .finished
                            .load(std::sync::atomic::Ordering::Acquire)
                    })
            }) {
                return Ok(json!({"registry":state.record.registry}));
            }
            let running = state
                .record
                .registry
                .panes
                .iter()
                .filter(|p| p.status == PaneStatus::Running)
                .count();
            ensure!(
                (matches!(target, SpawnTarget::Restore { .. })
                    || state.record.registry.panes.len() + state.spawning < MAX_PANE_RECORDS)
                    && running + state.spawning < MAX_LIVE_PANES,
                "pane limit"
            );
            target.check(&state)?;
            if let Some(resume) = &resume {
                recovery::check_resume(&state, &self.inner.captures.lock().unwrap(), resume)?;
            }
            if let Some(workspace_id) = &workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            // Resolve automatic labels once, including in-flight workspaces.
            // Only this short reservation transaction owns name allocation.
            let pending_name = if let SpawnTarget::CreateWorkspace {
                name, bootstrap, ..
            } = &mut target
            {
                let resolved = available_name(
                    name.as_deref(),
                    if *bootstrap { "Main" } else { "Workspace" },
                    state
                        .record
                        .registry
                        .workspaces
                        .iter()
                        .map(|w| w.name.as_str())
                        .chain(state.spawn_workspace_names.values().map(String::as_str)),
                )?;
                *name = Some(resolved.clone());
                Some(resolved)
            } else {
                None
            };
            let workspace = workspace_id
                .as_ref()
                .map(|id| -> Result<_> {
                    let name = match &pending_name {
                        Some(name) => name.clone(),
                        None => state
                            .record
                            .registry
                            .workspaces
                            .iter()
                            .find(|w| &w.id == id)
                            .context("unknown workspace")?
                            .name
                            .clone(),
                    };
                    Ok((id.clone(), name))
                })
                .transpose()?;
            // Validate the exact layout and revisions before argv can execute.
            spawn_record(state.record.clone(), info.clone(), &target)?;
            if let Some(workspace_id) = &workspace_id {
                state.spawn_workspaces.insert(workspace_id.clone());
                if let Some(name) = pending_name {
                    state
                        .spawn_workspace_names
                        .insert(workspace_id.clone(), name);
                }
            }
            if let Some(resume) = &resume {
                state.resuming.insert(id.clone(), resume.clone());
            }
            state.spawning += 1;
            workspace
        };
        let mut reservation = SpawnReservation {
            inner: self.inner.clone(),
            workspace_id: workspace_id.clone(),
            resume_pane: resume.as_ref().map(|_| id.clone()),
            committed: false,
        };
        #[cfg(test)]
        {
            let barrier = self.inner.spawn_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                barrier
                    .entered
                    .send(())
                    .context("spawn test barrier receiver closed")?;
                barrier
                    .resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .context("spawn test barrier was not released")?;
            }
        }
        self.start_observer()?;
        let environment = self.inner.spawn_environment.lock().unwrap().clone();
        let ssh_agent_socket = self
            .inner
            .ssh_agent
            .lock()
            .unwrap()
            .as_ref()
            .map(|relay| relay.socket().to_owned());
        // PTY startup, child exec and initial snapshot never run under the registry lock.
        let spawned = Pane::spawn_with_id(
            SpawnSpec {
                argv: spec.argv,
                home: spec.home,
                cwd: spec.cwd,
                size: spec.size,
                isolated_process: false,
                isolated_environment: spec.isolated_environment,
            },
            id.clone(),
            self.inner.view_budget.clone(),
            environment,
            Some(self.inner.status_wake.clone()),
            Some(crate::SpawnContext {
                session: (
                    self.inner.session.session_id.clone(),
                    self.inner.session.name.clone(),
                ),
                workspace,
            }),
            ssh_agent_socket,
        );
        let pane = spawned?;
        let weak = Arc::downgrade(&self.inner);
        let terminating_id = id.clone();
        *pane.shared.before_terminate.lock().unwrap() = Some(Arc::new(move || {
            let Some(inner) = weak.upgrade() else {
                return Ok(());
            };
            Registry {
                inner,
                fresh: false,
                lifecycle_owner: false,
            }
            .revoke_recovery(&terminating_id)
        }));
        let result = (|| -> Result<RegistrySnapshot> {
            let _transaction = self.inner.transaction.lock().unwrap();
            let current = {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(!state.closing, "daemon is shutting down");
                ensure!(!self.session_end_requested(), "session has ended");
                target.check(&state)?;
                if let Some(resume) = &resume {
                    recovery::check_resume_reserved(&state, resume, &id)?;
                }
                state.record.clone()
            };
            let next = spawn_record(current, info, &target)?;
            commit(&self.inner, next, |state| {
                state.live.insert(id.clone(), pane.clone());
            })
        })();
        if result.is_ok() {
            reservation.committed = true;
        }
        if result.is_err() {
            let _ = pane.request(PaneRequest::Shutdown);
        }
        let registry = result?;
        let weak = Arc::downgrade(&self.inner);
        let notify_id = id.clone();
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                let registry = Registry {
                    inner,
                    fresh: false,
                    lifecycle_owner: false,
                };
                // A persistence failure poisons further operations; never report durable success.
                let _ = registry.record_completion(&notify_id);
            }
        });
        *pane.shared.on_finish.lock().unwrap() = Some(callback.clone());
        if pane
            .shared
            .finished
            .load(std::sync::atomic::Ordering::Acquire)
        {
            callback();
        }
        drop(reservation);
        let registry = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            if state.record.registry.revision == registry.revision {
                registry
            } else {
                state.record.registry.clone()
            }
        };
        Ok(
            json!({"registry":registry,"pane_id":id,"workspace_id":workspace_id,"window_id":target.window_id()}),
        )
    }
    fn drain_completions(&self) -> Result<bool> {
        let completed = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            state
                .record
                .registry
                .panes
                .iter()
                .filter(|p| p.workspace_id.is_some() && p.status == PaneStatus::Exited)
                .map(|p| p.id.clone())
                .collect::<Vec<_>>()
        };
        let pending = !completed.is_empty();
        for id in completed {
            self.record_completion(&id)?;
        }
        Ok(pending)
    }
    fn expire_retired_views(&self) -> Result<()> {
        // Live workers expire their own leases. Completed workers no longer run a
        // timer, so reclaim their expired slots lazily before new admission.
        let panes = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            state
                .live
                .values()
                .filter(|pane| {
                    pane.shared
                        .finished
                        .load(std::sync::atomic::Ordering::Acquire)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let now = std::time::Instant::now();
        for pane in panes {
            let expired = pane.shared.authority.lock().unwrap().expire(now);
            if !expired.is_empty() {
                let mut baselines = pane.shared.delta_frames.lock().unwrap();
                for id in expired {
                    baselines.remove(&id);
                }
                pane.shared.changes.notify();
            }
        }
        Ok(())
    }
    fn forget(&self, id: &str) -> Result<Value> {
        let pane = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            let info = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == id)
                .context("unknown pane")?;
            ensure!(
                info.status != PaneStatus::Running,
                "cannot forget a running pane; terminate it explicitly first"
            );
            state.live.get(id).cloned()
        };
        // Serialize only this retired pane's authority changes. A request that already
        // cloned the handle checks forgotten again after this gate before attaching.
        let _view_policy = pane
            .as_ref()
            .map(|pane| pane.shared.view_policy.lock().unwrap());
        if let Some(pane) = &pane {
            let lifecycle = pane.shared.published.lock().unwrap().lifecycle.clone();
            ensure!(
                matches!(lifecycle.as_str(), "exited" | "terminated"),
                "pane execution has not completed; cannot forget"
            );
            let mut authority = pane.shared.authority.lock().unwrap();
            authority.expire(std::time::Instant::now());
            ensure!(
                authority.views.is_empty(),
                "pane still has active views; detach them before forgetting"
            );
        }
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, "daemon is shutting down");
            ensure!(!self.session_end_requested(), "session has ended");
            ensure!(
                !state.resuming.values().any(|attempt| attempt.source == id),
                "pane busy: native resume in progress"
            );
            let info = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == id)
                .context("unknown pane")?;
            if let Some(workspace_id) = &info.workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            ensure!(
                info.status != PaneStatus::Running,
                "cannot forget a running pane"
            );
            for workspace in &state.record.registry.workspaces {
                for window in &workspace.windows {
                    ensure!(
                        !window
                            .layout
                            .as_ref()
                            .is_some_and(|layout| layout_references(layout, id)),
                        "pane is still referenced by window {}; remove its layout references first",
                        window.id
                    );
                }
            }
            state.record.clone()
        };
        if let Some(info) = next.registry.panes.iter().find(|p| p.id == id)
            && let (Some(workspace_id), Some(window_id)) = (&info.workspace_id, &info.window_id)
        {
            let w = next
                .registry
                .workspaces
                .iter_mut()
                .find(|w| &w.id == workspace_id)
                .context("unknown workspace")?;
            let window = w
                .windows
                .iter_mut()
                .find(|w| &w.id == window_id)
                .context("unknown window")?;
            window.revision = next_revision(window.revision)?;
            w.revision = next_revision(w.revision)?;
        }
        next.registry.panes.retain(|pane| pane.id != id);
        next.recoveries.remove(id);
        next.directories.remove(id);
        next.automatic.remove(id);
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        let registry = commit(&self.inner, next, |state| {
            if let Some(pane) = &pane {
                pane.shared
                    .forgotten
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            state.live.remove(id);
            state.closing_panes.remove(id);
        })?;
        Ok(json!({"registry":registry,"pane_id":id}))
    }
    fn record_completion(&self, id: &str) -> Result<()> {
        let pane = { self.inner.state.lock().unwrap().live.get(id).cloned() };
        let Some(pane) = pane else {
            return Ok(());
        };
        let (status, exit_code) = {
            let p = pane.shared.published.lock().unwrap();
            let status = match p.lifecycle.as_str() {
                "exited" | "terminated" => PaneStatus::Exited,
                // Faults keep their projections visible for inspection.
                "fault" => PaneStatus::Stopped,
                _ => return Ok(()),
            };
            (status, p.exit_code.map(|code| code as i32))
        };
        self.inner.captures.lock().unwrap().retire_pane(id);
        let unbound_restore = {
            let mut state = self.inner.state.lock().unwrap();
            let unbound = state
                .resuming
                .remove(id)
                .is_some_and(|attempt| attempt.source == id);
            if unbound {
                state.failed_restores.insert(id.to_owned());
            }
            unbound || state.failed_restores.contains(id)
        };
        let agent_retirement = self.inner.agents.retire_pane(id);
        {
            let mut published = pane.shared.published.lock().unwrap();
            published.status.clear();
            published.program_identity = None;
            pane.shared
                .status_reset
                .store(true, std::sync::atomic::Ordering::Release);
        }
        pane.shared.changes.notify();
        let _transaction = self.inner.transaction.lock().unwrap();
        let (mut next, closing, reserved) = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            let reserved = state.restore_batch
                || unbound_restore
                || state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.workspace_id.as_ref())
                    .is_some_and(|id| {
                        state.closing_workspaces.contains(id) || state.spawn_workspaces.contains(id)
                    })
                || state
                    .spawn_workspaces
                    .iter()
                    .any(|id| !state.record.registry.workspaces.iter().any(|w| &w.id == id));
            (state.record.clone(), state.closing, reserved)
        };
        let Some(info) = next.registry.panes.iter_mut().find(|pane| pane.id == id) else {
            // A concurrent successful PaneForget may retire an already-completed handle.
            return Ok(());
        };
        if info.status == status
            && info.exit_code == exit_code
            && (status != PaneStatus::Exited || info.workspace_id.is_none() || closing || reserved)
        {
            return Ok(());
        }
        info.status = if unbound_restore {
            PaneStatus::Stopped
        } else {
            status
        };
        info.exit_code = exit_code;
        info.foreground_cwd = None;
        info.harness = None;
        info.harness_status = None;
        info.project = None;
        if status == PaneStatus::Exited && !closing && !reserved {
            close_membership(&mut next.registry, id)?;
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |_| {})?;
        drop(_transaction);
        self.notify_session_end();
        // Keep the final in-memory scene readable until daemon shutdown; no pixels go to disk.
        agent_retirement?;
        Ok(())
    }
    fn close_documents(
        &self,
        workspace_id: &str,
        window_id: Option<&str>,
        revision: u64,
    ) -> Result<Value> {
        let (panes, closing_documents) = {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(
                !state.closing && !self.session_end_requested(),
                "session is closing"
            );
            workspace_available(&state, workspace_id)?;
            ensure!(
                !state.spawn_workspaces.iter().any(|id| !state
                    .record
                    .registry
                    .workspaces
                    .iter()
                    .any(|w| &w.id == id)),
                "session busy: workspace creation in progress"
            );
            let w = state
                .record
                .registry
                .workspaces
                .iter()
                .find(|w| w.id == workspace_id)
                .context("unknown workspace")?;
            if let Some(id) = window_id {
                let window = w
                    .windows
                    .iter()
                    .find(|w| w.id == id)
                    .context("unknown window")?;
                ensure!(
                    window.revision == revision,
                    "stale window revision: current {}",
                    window.revision
                );
            } else {
                ensure!(
                    w.revision == revision,
                    "stale workspace revision: current {}",
                    w.revision
                );
            }
            let panes = state
                .record
                .registry
                .panes
                .iter()
                .filter(|p| {
                    p.workspace_id.as_deref() == Some(workspace_id)
                        && window_id.is_none_or(|id| p.window_id.as_deref() == Some(id))
                })
                .filter_map(|p| state.live.get(&p.id).cloned())
                .collect::<Vec<_>>();
            let removed = state
                .record
                .registry
                .panes
                .iter()
                .filter(|pane| {
                    pane.workspace_id.as_deref() == Some(workspace_id)
                        && window_id.is_none_or(|id| pane.window_id.as_deref() == Some(id))
                })
                .map(|pane| pane.id.clone())
                .collect::<BTreeSet<_>>();
            ensure!(
                !state.resuming.iter().any(|(id, attempt)| removed.contains(&attempt.source) && !removed.contains(id)),
                "workspace busy: native resume in progress"
            );
            let mut next = state.record.clone();
            next.recoveries.retain(|id, _| !removed.contains(id));
            next.automatic.retain(|id, _| !removed.contains(id));
            for (id, attempt) in &state.resuming {
                if removed.contains(id) {
                    next.recoveries.remove(&attempt.source);
                    next.automatic.remove(&attempt.source);
                }
            }
            if next.recoveries != state.record.recoveries {
                next.registry.revision = next_revision(next.registry.revision)?;
                drop(state);
                commit(&self.inner, next, |_| {})?;
                state = self.inner.state.lock().unwrap();
            }
            state.closing_workspaces.insert(workspace_id.to_owned());
            state.closing_document_panes.extend(removed.iter().cloned());
            (panes, removed)
        };
        // Native waits must not hold the durable writer or global state locks.
        let mut failures = Vec::new();
        for pane in panes {
            if let Err(error) = pane.request(PaneRequest::Shutdown) {
                failures.push(format!("{}: {error:#}", pane.shared.pane_id));
            }
            if let Err(error) = self.record_completion(&pane.shared.pane_id) {
                failures.push(format!("{} completion: {error:#}", pane.shared.pane_id));
            }
        }
        let result = (|| -> Result<RegistrySnapshot> {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut next = {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(
                    failures.is_empty(),
                    "partial close; executions may already have terminated: {}",
                    failures.join("; ")
                );
                state.record.clone()
            };
            for pane in &mut next.registry.panes {
                if pane.workspace_id.as_deref() == Some(workspace_id)
                    && window_id.is_none_or(|id| pane.window_id.as_deref() == Some(id))
                {
                    pane.workspace_id = None;
                    pane.window_id = None;
                }
            }
            if let Some(window_id) = window_id {
                let w = next
                    .registry
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace_id)
                    .context("workspace disappeared during close")?;
                w.windows.retain(|w| w.id != window_id);
                w.revision = next_revision(w.revision)?;
                if w.windows.is_empty()
                    && !next
                        .registry
                        .panes
                        .iter()
                        .any(|p| p.workspace_id.as_deref() == Some(workspace_id))
                {
                    next.registry.workspaces.retain(|w| w.id != workspace_id);
                }
            } else {
                next.registry.workspaces.retain(|w| w.id != workspace_id);
            }
            next.registry.revision = next_revision(next.registry.revision)?;
            validate_record(&next)?;
            commit(&self.inner, next, |_| {}).context(
                "close executions completed but document persistence failed; restart required",
            )
        })();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.closing_workspaces.remove(workspace_id);
            state
                .closing_document_panes
                .retain(|id| !closing_documents.contains(id));
        }
        let drained = self.drain_completions();
        self.notify_session_end();
        let registry = result.context("close may already have terminated executions; inspect durable registry before another action")?;
        let registry = if drained.context(
            "close committed but deferred completion failed; inspect before another action",
        )? {
            self.snapshot()?
        } else {
            registry
        };
        Ok(json!({"registry":registry}))
    }
    pub fn shutdown(&self) -> Result<()> {
        // Observe while the process tree still exists; never derive intent from teardown.
        self.observe_processes();
        let directory_capture = self.flush_directories();
        let panes = {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            state.closing = true;
            self.inner.captures.lock().unwrap().launches.clear();
            self.inner.changed.notify_all();
            state.live.values().cloned().collect::<Vec<_>>()
        };
        self.detach_all_clients("shutdown");
        let agent_shutdown = self.inner.agents.shutdown();
        let forwarding = self.inner.ssh_agent.lock().unwrap().clone();
        let forwarding_shutdown = forwarding.map(|relay| relay.shutdown()).transpose();
        self.stop_projects();
        let mut first_error = directory_capture
            .err()
            .or_else(|| agent_shutdown.err())
            .or_else(|| forwarding_shutdown.err());
        for pane in panes {
            if let Err(error) = pane.request(PaneRequest::Shutdown)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            if let Err(error) = self.record_completion(&pane.shared.pane_id)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        // Spawns already accepted must finish/abort before ownership lease can be released.
        let state = self.inner.state.lock().unwrap();
        let _state = self
            .inner
            .changed
            .wait_while(state, |s| s.spawning > 0)
            .unwrap();
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}
fn healthy(state: &State) -> Result<()> {
    if let Some(error) = &state.durability_error {
        bail!("registry persistence failed; restart required: {error}");
    }
    Ok(())
}
fn workspace_available(state: &State, workspace_id: &str) -> Result<()> {
    ensure!(
        !state.closing_workspaces.contains(workspace_id),
        "workspace busy: close in progress"
    );
    ensure!(
        !state.spawn_workspaces.contains(workspace_id),
        "workspace busy: pane spawn in progress"
    );
    Ok(())
}
fn next_revision(revision: u64) -> Result<u64> {
    revision.checked_add(1).context("revision exhausted")
}
fn layout_references(layout: &LayoutNode, pane_id: &str) -> bool {
    match layout {
        LayoutNode::Leaf { pane_id: leaf } => leaf == pane_id,
        LayoutNode::Split { first, second, .. } => {
            layout_references(first, pane_id) || layout_references(second, pane_id)
        }
    }
}
fn remove_layout_pane(layout: LayoutNode, pane_id: &str) -> Option<LayoutNode> {
    match layout {
        LayoutNode::Leaf { pane_id: leaf } => {
            (leaf != pane_id).then_some(LayoutNode::Leaf { pane_id: leaf })
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            remove_layout_pane(*first, pane_id),
            remove_layout_pane(*second, pane_id),
        ) {
            (Some(first), Some(second)) => Some(LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (first, second) => first.or(second),
        },
    }
}
fn spawn_record(mut next: Record, info: PaneInfo, target: &SpawnTarget) -> Result<Record> {
    if let SpawnTarget::Restore { pane_id, .. } = target {
        let old = next
            .registry
            .panes
            .iter_mut()
            .find(|pane| &pane.id == pane_id)
            .context("unknown saved pane")?;
        // Execution identity remains in its exact saved position. Initial CWD is
        // historical launch metadata; current directory intent has its own whitelist.
        let initial_cwd = old.cwd.clone();
        *old = info;
        old.cwd = initial_cwd;
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        return Ok(next);
    }
    if let SpawnTarget::CreateWorkspace {
        workspace_id, name, ..
    } = target
    {
        let name = name.clone().context("workspace name was not reserved")?;
        next.registry.workspaces.push(Workspace {
            id: workspace_id.clone(),
            name,
            revision: 1,
            windows: Vec::new(),
        });
    }
    if let (Some(workspace_id), Some(window_id)) = (&info.workspace_id, &info.window_id) {
        let workspace = next
            .registry
            .workspaces
            .iter_mut()
            .find(|w| &w.id == workspace_id)
            .context("unknown workspace")?;
        if matches!(
            target,
            SpawnTarget::CreateWorkspace { .. } | SpawnTarget::CreateWindow { .. }
        ) {
            let requested = match target {
                SpawnTarget::CreateWindow { name, .. } => name.as_deref(),
                _ => None,
            };
            let name = available_name(
                requested,
                "Window",
                workspace.windows.iter().map(|w| w.name.as_str()),
            )?;
            workspace.windows.push(Window {
                id: window_id.clone(),
                name,
                revision: 1,
                layout: None,
            });
        }
        let window = workspace
            .windows
            .iter_mut()
            .find(|w| &w.id == window_id)
            .context("unknown window")?;
        if let SpawnTarget::Reopen { pane_id, .. } = target {
            let layout = window
                .layout
                .as_mut()
                .context("window layout does not reference pane")?;
            replace_layout_pane(layout, pane_id, &info.id);
            let old = next
                .registry
                .panes
                .iter_mut()
                .find(|p| &p.id == pane_id)
                .context("unknown pane")?;
            old.workspace_id = None;
            old.window_id = None;
        } else {
            let leaf = LayoutNode::Leaf {
                pane_id: info.id.clone(),
            };
            window.layout = Some(match window.layout.take() {
                None => leaf,
                Some(first) => LayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 500,
                    first: Box::new(first),
                    second: Box::new(leaf),
                },
            });
        }
        window.revision = next_revision(window.revision)?;
        workspace.revision = next_revision(workspace.revision)?;
    }
    next.registry.panes.push(info);
    next.registry.revision = next_revision(next.registry.revision)?;
    validate_record(&next)?;
    Ok(next)
}
fn replace_layout_pane(layout: &mut LayoutNode, old: &str, new: &str) {
    match layout {
        LayoutNode::Leaf { pane_id } => {
            if pane_id == old {
                *pane_id = new.to_owned();
            }
        }
        LayoutNode::Split { first, second, .. } => {
            replace_layout_pane(first, old, new);
            replace_layout_pane(second, old, new);
        }
    }
}
fn workspace<'a>(
    r: &'a mut RegistrySnapshot,
    id: &str,
    revision: u64,
) -> Result<&'a mut Workspace> {
    let w = r
        .workspaces
        .iter_mut()
        .find(|w| w.id == id)
        .context("unknown workspace")?;
    ensure!(
        w.revision == revision,
        "stale workspace revision: current {}",
        w.revision
    );
    Ok(w)
}
fn find_window<'a>(
    r: &'a RegistrySnapshot,
    workspace_id: &str,
    window_id: &str,
) -> Result<&'a Window> {
    r.workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .context("unknown workspace")?
        .windows
        .iter()
        .find(|w| w.id == window_id)
        .context("unknown window")
}
fn window<'a>(workspace: &'a mut Workspace, id: &str, revision: u64) -> Result<&'a mut Window> {
    let w = workspace
        .windows
        .iter_mut()
        .find(|w| w.id == id)
        .context("unknown window")?;
    ensure!(
        w.revision == revision,
        "stale window revision: current {}",
        w.revision
    );
    Ok(w)
}
fn check_spawn_window(
    r: &RegistrySnapshot,
    workspace_id: Option<&str>,
    window_id: Option<&str>,
    revision: Option<u64>,
) -> Result<()> {
    match (workspace_id, window_id, revision) {
        (Some(workspace_id), Some(window_id), Some(revision)) => {
            let w = find_window(r, workspace_id, window_id)?;
            ensure!(
                w.revision == revision,
                "stale window revision: current {}",
                w.revision
            );
        }
        (None, None, None) => {}
        _ => bail!("bound spawn requires workspace_id, window_id and expected_revision together"),
    }
    Ok(())
}
fn available_name<'a>(
    requested: Option<&str>,
    base: &str,
    existing: impl Iterator<Item = &'a str>,
) -> Result<String> {
    if let Some(name) = requested.filter(|name| !name.trim().is_empty()) {
        valid_name(name)?;
        return Ok(name.to_owned());
    }
    let existing: BTreeSet<_> = existing.collect();
    for suffix in 1..=MAX_PANE_RECORDS + MAX_WORKSPACES + 1 {
        let name = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base} {suffix}")
        };
        if !existing.contains(name.as_str()) {
            return Ok(name);
        }
    }
    bail!("name space exhausted")
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
        "invalid name"
    );
    Ok(())
}
fn validate_layout(node: &LayoutNode, panes: &[PaneInfo]) -> Result<()> {
    fn walk(
        node: &LayoutNode,
        panes: &[PaneInfo],
        seen: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth <= MAX_LAYOUT_DEPTH, "layout depth limit");
        match node {
            LayoutNode::Leaf { pane_id } => {
                ensure!(
                    panes.iter().any(|p| &p.id == pane_id),
                    "layout refers to unknown pane"
                );
                ensure!(
                    seen.insert(pane_id.clone()),
                    "duplicate pane in workspace layout"
                );
            }
            LayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ensure!(
                    (1..1000).contains(ratio),
                    "split ratio must be 1..999 thousandths"
                );
                walk(first, panes, seen, depth + 1)?;
                walk(second, panes, seen, depth + 1)?;
            }
        }
        Ok(())
    }
    walk(node, panes, &mut BTreeSet::new(), 0)
}
fn validate_owned_layout(
    node: &LayoutNode,
    panes: &[PaneInfo],
    workspace_id: &str,
    window_id: &str,
) -> Result<()> {
    validate_layout(node, panes)?;
    fn walk(
        node: &LayoutNode,
        panes: &[PaneInfo],
        workspace_id: &str,
        window_id: &str,
    ) -> Result<()> {
        match node {
            LayoutNode::Leaf { pane_id } => {
                let pane = panes
                    .iter()
                    .find(|p| &p.id == pane_id)
                    .context("unknown pane")?;
                ensure!(
                    pane.workspace_id.as_deref() == Some(workspace_id)
                        && pane.window_id.as_deref() == Some(window_id),
                    "layout pane belongs to another workspace/window"
                );
            }
            LayoutNode::Split { first, second, .. } => {
                walk(first, panes, workspace_id, window_id)?;
                walk(second, panes, workspace_id, window_id)?;
            }
        }
        Ok(())
    }
    walk(node, panes, workspace_id, window_id)
}
fn close_membership(r: &mut RegistrySnapshot, pane_id: &str) -> Result<()> {
    let Some(pane) = r.panes.iter_mut().find(|p| p.id == pane_id) else {
        return Ok(());
    };
    let (Some(workspace_id), Some(window_id)) = (pane.workspace_id.take(), pane.window_id.take())
    else {
        return Ok(());
    };
    let w = r
        .workspaces
        .iter_mut()
        .find(|w| w.id == workspace_id)
        .context("unknown owning workspace")?;
    let window = w
        .windows
        .iter_mut()
        .find(|w| w.id == window_id)
        .context("unknown owning window")?;
    window.layout = window
        .layout
        .take()
        .and_then(|layout| remove_layout_pane(layout, pane_id));
    window.revision = next_revision(window.revision)?;
    if !r.panes.iter().any(|p| {
        p.workspace_id.as_deref() == Some(&workspace_id)
            && p.window_id.as_deref() == Some(&window_id)
    }) {
        w.windows.retain(|w| w.id != window_id);
    }
    w.revision = next_revision(w.revision)?;
    if w.windows.is_empty()
        && !r
            .panes
            .iter()
            .any(|p| p.workspace_id.as_deref() == Some(&workspace_id))
    {
        r.workspaces.retain(|w| w.id != workspace_id);
    }
    Ok(())
}
fn validate_record(record: &Record) -> Result<()> {
    ensure!(
        record.version == STORAGE_VERSION,
        "unsupported registry storage version"
    );
    let r = &record.registry;
    ensure!(
        r.panes.len() <= MAX_PANE_RECORDS && r.workspaces.len() <= MAX_WORKSPACES,
        "registry resource limit"
    );
    let mut ids = BTreeSet::new();
    for pane in &r.panes {
        ensure!(
            Uuid::parse_str(&pane.id).is_ok() && ids.insert(&pane.id),
            "invalid or duplicate pane id"
        );
        valid_name(&pane.title)?;
        ensure!(
            pane.cwd.is_absolute() && pane.home.is_absolute(),
            "invalid persisted pane paths"
        );
    }
    ensure!(
        record.recoveries.len() <= r.panes.len(),
        "recovery descriptor limit"
    );
    for (id, descriptor) in &record.recoveries {
        ensure!(
            r.panes.iter().any(|pane| &pane.id == id),
            "recovery refers to unknown pane"
        );
        recovery::validate(descriptor)?;
    }
    for (id, directory) in &record.directories {
        ensure!(
            r.panes.iter().any(|pane| &pane.id == id),
            "directory refers to unknown pane"
        );
        directories::validate(&directory.cwd)?;
    }
    for (id, descriptor) in &record.automatic {
        ensure!(
            record.recoveries.get(id) == Some(descriptor),
            "automatic native intent requires a verified recovery descriptor"
        );
        recovery::validate(descriptor)?;
    }
    let mut ids = BTreeSet::new();
    for w in &r.workspaces {
        ensure!(
            Uuid::parse_str(&w.id).is_ok() && ids.insert(&w.id) && w.revision > 0,
            "invalid workspace identity/revision"
        );
        valid_name(&w.name)?;
        let mut window_ids = BTreeSet::new();
        for window in &w.windows {
            ensure!(
                Uuid::parse_str(&window.id).is_ok()
                    && window_ids.insert(&window.id)
                    && window.revision > 0,
                "invalid window identity/revision"
            );
            valid_name(&window.name)?;
            if let Some(node) = &window.layout {
                validate_owned_layout(node, &r.panes, &w.id, &window.id)?;
            }
        }
    }
    for pane in &r.panes {
        match (&pane.workspace_id, &pane.window_id) {
            (Some(workspace_id), Some(window_id)) => {
                find_window(r, workspace_id, window_id)?;
            }
            (None, None) => {}
            _ => bail!("pane requires paired workspace/window membership"),
        }
    }
    Ok(())
}
fn private_directory(root: &Path) -> Result<()> {
    if !root.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
    }
    let m = fs::symlink_metadata(root)?;
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o077 == 0,
        "state root must be private, current-user-owned directory"
    );
    Ok(())
}
fn check_file(file: &File) -> Result<()> {
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o077 == 0,
        "registry file must be private and current-user-owned"
    );
    Ok(())
}
fn read_record(root: &Path) -> Result<Option<Record>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("registry.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_RECORD, "registry file limit");
    let mut value: Value = serde_json::from_slice(&bytes)
        .context("decode registry (never discard unreadable metadata)")?;
    let legacy_binding = matches!(value["version"].as_u64(), Some(1 | 2));
    let upgraded = matches!(value["version"].as_u64(), Some(1..=3));
    // LEGACY_COMPAT(next-major): one-shot v1 metadata upgrade, not a wire/runtime shim.
    // Window IDs reuse their workspace UUID in a distinct typed identity domain.
    if value["version"] == 1 {
        let registry = value["registry"]
            .as_object_mut()
            .context("invalid legacy registry")?;
        let mut panes = match registry.remove("panes") {
            Some(Value::Array(panes)) => panes,
            _ => bail!("invalid legacy panes"),
        };
        let mut referenced = BTreeSet::new();
        let workspaces = registry
            .get_mut("workspaces")
            .and_then(Value::as_array_mut)
            .context("invalid legacy workspaces")?;
        for workspace in workspaces {
            let id = workspace["id"]
                .as_str()
                .context("invalid legacy workspace id")?
                .to_owned();
            let layout = workspace
                .as_object_mut()
                .context("invalid legacy workspace")?
                .remove("layout")
                .unwrap_or(Value::Null);
            if !layout.is_null() {
                let node = LayoutNode::deserialize(&layout)?;
                fn owned(
                    node: &LayoutNode,
                    panes: &[Value],
                    id: &str,
                    referenced: &mut BTreeSet<String>,
                ) -> bool {
                    match node {
                        LayoutNode::Leaf { pane_id } => {
                            if !panes
                                .iter()
                                .any(|p| p["id"] == *pane_id && p["workspace_id"] == id)
                            {
                                return false;
                            }
                            referenced.insert(pane_id.clone());
                            true
                        }
                        LayoutNode::Split { first, second, .. } => {
                            owned(first, panes, id, referenced)
                                && owned(second, panes, id, referenced)
                        }
                    }
                }
                ensure!(
                    owned(&node, &panes, &id, &mut referenced),
                    "legacy registry has ambiguous foreign-workspace layout ownership; export and repair registry.json before retrying"
                );
            }
            let has_panes = panes
                .iter()
                .any(|p| p["workspace_id"] == id && p["status"] != "exited");
            workspace["windows"] = if has_panes || !layout.is_null() {
                json!([{"id":id,"name":"Window","revision":1,"layout":layout}])
            } else {
                json!([])
            };
        }
        for pane in &mut panes {
            // v1 kept the workspace label on retired, unreferenced receipts.
            if pane["status"] == "exited"
                && pane["id"]
                    .as_str()
                    .is_some_and(|id| !referenced.contains(id))
            {
                pane["workspace_id"] = Value::Null;
            }
            pane["window_id"] = pane["workspace_id"].clone();
        }
        registry.insert("panes".into(), Value::Array(panes));
        value["version"] = json!(STORAGE_VERSION);
    }
    // LEGACY_COMPAT(next-major): schema 2 had no durable authenticated binding.
    // Never promote its optional live harness facts into recovery authority.
    if legacy_binding {
        value["version"] = json!(STORAGE_VERSION);
        value["recoveries"] = json!({});
    }
    // LEGACY_COMPAT(next-major): v3 had initial CWD only. Preserve verified
    // descriptors, but never promote transient foreground observations.
    if upgraded {
        value["version"] = json!(STORAGE_VERSION);
        value["directories"] = json!({});
        value["automatic"] = json!({});
    }
    let record: Record = serde_json::from_value(value).context("decode registry metadata")?;
    validate_record(&record)?;
    if upgraded {
        persist(root, &record)?;
    }
    Ok(Some(record))
}
fn persist(root: &Path, record: &Record) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    let durable = DurableRecord {
        version: record.version,
        recoveries: &record.recoveries,
        directories: &record.directories,
        automatic: &record.automatic,
        registry: DurableRegistry {
            revision: record.registry.revision,
            workspaces: &record.registry.workspaces,
            panes: DurablePanes(&record.registry.panes),
        },
    };
    serde_json::to_writer(temporary.as_file_mut(), &durable)?;
    temporary.as_file_mut().flush()?;
    ensure!(
        temporary.as_file().metadata()?.len() <= MAX_RECORD,
        "registry file limit"
    );
    temporary.as_file().sync_all()?;
    temporary.persist(root.join("registry.json"))?;
    File::open(root)?.sync_all()?;
    Ok(())
}
// Caller owns transaction; no state guard may cross this durability boundary.
fn commit(
    inner: &Inner,
    next: Record,
    publish: impl FnOnce(&mut State),
) -> Result<RegistrySnapshot> {
    #[cfg(test)]
    {
        let barrier = inner.persist_barrier.lock().unwrap().take();
        if let Some(barrier) = barrier {
            barrier
                .entered
                .send(())
                .context("storage test barrier receiver closed")?;
            barrier
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .context("storage test barrier was not released")?;
        }
    }
    if let Err(error) = persist(&inner.root, &next) {
        let mut state = inner.state.lock().unwrap();
        state.durability_error = Some(format!("{error:#}"));
        inner.changed.notify_all();
        return Err(error);
    }
    let mut state = inner.state.lock().unwrap();
    if !state.record.registry.workspaces.is_empty()
        && next.registry.workspaces.is_empty()
        && !state.closing
    {
        inner
            .session_end
            .store(true, std::sync::atomic::Ordering::Release);
    }
    state.record = next;
    publish(&mut state);
    inner.changed.notify_all();
    let _ = inner.project_wake.try_send(());
    Ok(state.record.registry.clone())
}

#[cfg(test)]
mod tests;
