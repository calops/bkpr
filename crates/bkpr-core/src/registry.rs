//! Backend-owned workspace intent, pane membership, and live execution state.
//! Reopening restores persisted metadata, not PTYs or running processes.
use crate::{Pane, SpawnSpec};
mod agents;
mod automatic;
mod capture;
mod ci;
mod context;
mod control;
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

#[derive(Debug)]
pub(crate) struct SessionClosing(pub(crate) &'static str);
impl std::fmt::Display for SessionClosing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for SessionClosing {}

/// A mutation conflicts with an in-progress workspace or window close.
///
/// Returned through [`anyhow::Error`] when the target is reserved for closing;
/// other busy conditions, such as an in-progress spawn, use separate errors.
#[derive(Debug)]
pub struct RegistryBusy {
    /// Workspace or window ID whose close reservation blocks the mutation.
    pub target_id: String,
}
impl std::fmt::Display for RegistryBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "target busy: close in progress ({})",
            self.target_id
        )
    }
}
impl std::error::Error for RegistryBusy {}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    registry: RegistrySnapshot,
    recoveries: BTreeMap<String, HarnessRecovery>,
    directories: BTreeMap<String, directories::Directory>,
    automatic: BTreeMap<String, HarnessRecovery>,
    // LEGACY_COMPAT(next-major): old records lack label origin; absence keeps
    // their saved labels fixed rather than guessing which names were explicit.
    #[serde(default)]
    automatic_titles: BTreeSet<String>,
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
    automatic_titles: &'a BTreeSet<String>,
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
    closing_windows: BTreeSet<String>,
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
    spawn_defaults: std::sync::OnceLock<SpawnDefaults>,
    clients: Mutex<BTreeMap<String, Arc<sessions::ClientLease>>>,
    state: Mutex<State>,
    // Serialize durable writers, never pane I/O. Lock order: transaction, then state.
    transaction: Mutex<()>,
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
    ci_wake: crossbeam_channel::Sender<()>,
    ci_cancel: Arc<std::sync::atomic::AtomicBool>,
    ci_worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    ci_refresh: Mutex<Vec<CiRefreshTarget>>,
    ci_inputs: Mutex<BTreeMap<PathBuf, u64>>,
    ci_input_generation: std::sync::atomic::AtomicU64,
    public_handles: std::sync::atomic::AtomicUsize,
    changed: Condvar,
    _lease: Lease,
    #[cfg(test)]
    spawn_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    spawn_started: Mutex<Option<SpawnStarted>>,
    #[cfg(test)]
    persist_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    capture_admission_wait: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    #[cfg(test)]
    control_resume_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    control_observe_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    control_cleanup_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    request_wait_entered: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.project_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.project_wake.try_send(());
        self.ci_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.ci_wake.try_send(());
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
#[cfg(test)]
type SpawnStarted = Arc<dyn Fn(&Pane) + Send + Sync>;
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
enum SpawnTarget {
    Place {
        workspace_id: Option<String>,
        window_id: Option<String>,
        expected_revision: Option<u64>,
        layout: Option<SpawnLayoutNode>,
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
            Self::Place { workspace_id, .. } => workspace_id.as_deref(),
            Self::Reopen { workspace_id, .. }
            | Self::Restore { workspace_id, .. }
            | Self::CreateWorkspace { workspace_id, .. }
            | Self::CreateWindow { workspace_id, .. } => Some(workspace_id),
        }
    }
    fn window_id(&self) -> Option<&str> {
        match self {
            Self::Place { window_id, .. } => window_id.as_deref(),
            Self::Reopen { window_id, .. }
            | Self::Restore { window_id, .. }
            | Self::CreateWorkspace { window_id, .. }
            | Self::CreateWindow { window_id, .. } => Some(window_id),
        }
    }
    fn check(&self, state: &State) -> Result<()> {
        if let Some(window_id) = self.window_id() {
            window_available(state, window_id)?;
        }
        let r = &state.record.registry;
        match self {
            Self::Place {
                workspace_id,
                window_id,
                expected_revision,
                layout,
            } => {
                check_spawn_window(
                    r,
                    workspace_id.as_deref(),
                    window_id.as_deref(),
                    *expected_revision,
                )?;
                ensure!(
                    layout.is_some() == workspace_id.is_some(),
                    "bound spawn requires a complete layout; unowned spawn requires no layout"
                );
                Ok(())
            }
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
            Self::CreateWorkspace { name, .. } => {
                workspace_creation_available(state)?;
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
                    "stale workspace revision for {workspace_id}: current {}",
                    w.revision
                );
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
    count: usize,
}
impl Drop for SpawnReservation {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        state.spawning -= self.count;
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
// Unpublished children never carry registry recovery authority.
struct SpawnChildren {
    panes: Vec<Pane>,
    committed: bool,
}
impl Drop for SpawnChildren {
    fn drop(&mut self) {
        if !self.committed {
            for pane in &self.panes {
                let _ = pane.request(PaneRequest::Shutdown);
            }
        }
    }
}
/// Cloneable handle to one exclusively owned state root and its live panes.
///
/// Clones share registry state, pane executions, and the state-root ownership
/// lease. Opening restores workspace and recovery metadata, not running
/// processes or terminal contents. Use [`Self::shutdown`] for coordinated
/// teardown before releasing the handles.
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
            self.stop_ci();
        }
    }
}

impl Registry {
    /// Returns the current session ID for correlating backend diagnostics.
    pub fn diagnostic_session_id(&self) -> &str {
        &self.inner.session.session_id
    }
    /// Opens the state root using [`DEFAULT_SESSION_NAME`].
    ///
    /// Has the same ownership, recovery, and error contracts as [`Self::open_named`].
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_named(root, DEFAULT_SESSION_NAME)
    }
    /// Exclusively opens a private state root and starts its background services.
    ///
    /// Creates a missing root and initial registry. Existing pane records reopen
    /// as stopped, with transient process and harness observations cleared.
    /// `name` identifies the session associated with this root.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid session name, a nonabsolute or nonprivate
    /// root, an existing owner, invalid stored metadata, or a storage or
    /// background-service startup failure.
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
            automatic_titles: BTreeSet::new(),
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
            pane.harness_status_changed_at_ms = None;
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
        let (ci_wake, ci_events) = crossbeam_channel::bounded(1);
        let agents = Arc::new(agents::Agents::open(root)?);
        let registry = Self {
            inner: Arc::new(Inner {
                root: root.to_owned(),
                session,
                spawn_defaults: std::sync::OnceLock::new(),
                clients: Mutex::new(BTreeMap::new()),
                state: Mutex::new(State {
                    record,
                    live: BTreeMap::new(),
                    spawning: 0,
                    spawn_workspaces: BTreeSet::new(),
                    spawn_workspace_names: BTreeMap::new(),
                    closing_workspaces: BTreeSet::new(),
                    closing_windows: BTreeSet::new(),
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
                ci_wake,
                ci_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ci_worker: Mutex::new(None),
                ci_refresh: Mutex::new(Vec::new()),
                ci_inputs: Mutex::new(BTreeMap::new()),
                ci_input_generation: std::sync::atomic::AtomicU64::new(0),
                public_handles: std::sync::atomic::AtomicUsize::new(1),
                changed: Condvar::new(),
                _lease: lease,
                #[cfg(test)]
                spawn_barrier: Mutex::new(None),
                #[cfg(test)]
                spawn_started: Mutex::new(None),
                #[cfg(test)]
                persist_barrier: Mutex::new(None),
                #[cfg(test)]
                capture_admission_wait: Mutex::new(None),
                #[cfg(test)]
                control_resume_barrier: Mutex::new(None),
                #[cfg(test)]
                control_observe_barrier: Mutex::new(None),
                #[cfg(test)]
                control_cleanup_barrier: Mutex::new(None),
                #[cfg(test)]
                request_wait_entered: Mutex::new(None),
            }),
            fresh,
            lifecycle_owner: true,
        };
        registry.start_agent_maintenance()?;
        registry.start_projects(project_events)?;
        registry.start_ci(ci_events)?;
        Ok(registry)
    }
    /// Whether no persisted registry record existed when this handle was opened.
    ///
    /// This opening-time flag does not change after mutations and does not
    /// indicate whether the registry currently contains workspaces or panes.
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
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn test_next_request_wait(&self) -> std::sync::mpsc::Receiver<()> {
        let (entered, receiver) = std::sync::mpsc::sync_channel(1);
        *self.inner.request_wait_entered.lock().unwrap() = Some(entered);
        receiver
    }
    #[cfg(test)]
    pub(crate) fn test_notify_request_wait(&self) {
        if let Some(entered) = self.inner.request_wait_entered.lock().unwrap().take() {
            let _ = entered.try_send(());
        }
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn test_wait_for_closing(&self) -> bool {
        let state = self.inner.state.lock().unwrap();
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.closing)
            .unwrap();
        state.closing
    }
    /// Whether removing the last workspace has requested session termination.
    ///
    /// Once set, the flag remains set. It is distinct from explicit daemon
    /// shutdown and does not itself perform teardown.
    pub fn session_end_requested(&self) -> bool {
        self.inner
            .session_end
            .load(std::sync::atomic::Ordering::Acquire)
    }
    /// Replaces the callback used to wake the owner when the session ends.
    ///
    /// Invokes the callback synchronously if termination was already requested.
    /// Notifications may repeat; the callback must tolerate repeated calls.
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
    pub(crate) fn request_admission(&self) -> Result<()> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(
            !state.closing && !self.session_end_requested(),
            SessionClosing("session is shutting down")
        );
        Ok(())
    }
    pub(crate) fn request_health(&self) -> Result<()> {
        healthy(&self.inner.state.lock().unwrap())
    }
    /// Returns registry metadata with effective titles from live panes.
    ///
    /// Does not refresh process observations or include terminal contents.
    ///
    /// # Errors
    ///
    /// Returns an error if a persistence failure has made this registry require
    /// a restart.
    pub fn snapshot(&self) -> Result<RegistrySnapshot> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        Ok(registry_snapshot(&state))
    }
    #[cfg(test)]
    fn mutate(
        &self,
        edit: impl FnOnce(&mut RegistrySnapshot) -> Result<()>,
    ) -> Result<RegistrySnapshot> {
        Ok(serde_json::from_value(
            self.mutate_inner(None, None, edit)?,
        )?)
    }
    fn mutate_inner(
        &self,
        workspace_id: Option<&str>,
        control: Option<&control::ControlOrigin<'_>>,
        edit: impl FnOnce(&mut RegistrySnapshot) -> Result<()>,
    ) -> Result<Value> {
        self.mutate_record_inner(workspace_id, control, |record| edit(&mut record.registry))
    }
    fn mutate_record_inner(
        &self,
        workspace_id: Option<&str>,
        control: Option<&control::ControlOrigin<'_>>,
        edit: impl FnOnce(&mut Record) -> Result<()>,
    ) -> Result<Value> {
        self.observe_control(control)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        self.authorize_control(control)?;
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            ensure!(
                !self.session_end_requested(),
                SessionClosing("session has ended")
            );
            if let Some(workspace_id) = workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            state.record.clone()
        };
        edit(&mut next)?;
        {
            let state = self.inner.state.lock().unwrap();
            // Workspace-level metadata can advance independently, but a reserved
            // window's layout/revision/membership cannot change under its close.
            for workspace in &state.record.registry.workspaces {
                for window in &workspace.windows {
                    if state.closing_windows.contains(&window.id) {
                        let unchanged = next
                            .registry
                            .workspaces
                            .iter()
                            .flat_map(|workspace| &workspace.windows)
                            .any(|next| next.id == window.id && next.revision == window.revision);
                        ensure!(
                            unchanged,
                            RegistryBusy {
                                target_id: window.id.clone()
                            }
                        );
                    }
                }
            }
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit_projected(
            &self.inner,
            next,
            |_| {},
            |state| response_snapshot(state, control),
        )
    }
    /// Publishes validated startup spawn defaults once, without changing the
    /// process environment or writing the defaults to disk.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or excessive arguments, nonabsolute or
    /// NUL-containing paths, or defaults that have already been configured.
    pub fn configure_spawn_defaults(&self, defaults: SpawnDefaults) -> Result<()> {
        validate_spawn_arguments(&defaults.argv, &defaults.cwd, &defaults.home)?;
        self.inner
            .spawn_defaults
            .set(defaults)
            .map_err(|_| anyhow::anyhow!("spawn defaults already configured"))
    }
    /// Dispatches a native protocol request without an authenticated client or
    /// harness origin.
    ///
    /// This is a trusted backend entry point, not a transport authentication
    /// boundary. Attachment registration, automatic pane context, and harness
    /// capture or agent operations require their authenticated transport paths.
    /// Pane operations still enforce the requested view's authority.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported protocol version, an operation that
    /// requires authenticated transport, or an operation-specific validation,
    /// revision, authority, lifecycle, process, or persistence failure.
    /// Close conflicts may carry [`RegistryBusy`]. A failed lifecycle operation
    /// may already have changed executions; an error does not imply rollback.
    pub fn request(&self, request: Request) -> Result<Payload> {
        self.request_for_client(request, None)
    }
    fn request_for_client(
        &self,
        request: Request,
        client: Option<Arc<sessions::ClientLease>>,
    ) -> Result<Payload> {
        self.request_for_origin(request, client, None)
    }
    fn request_for_origin(
        &self,
        request: Request,
        client: Option<Arc<sessions::ClientLease>>,
        control: Option<&control::ControlOrigin<'_>>,
    ) -> Result<Payload> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        if let Some(client) = &client
            && !matches!(&request.operation, Operation::SessionInfo)
        {
            client.ensure_active()?;
        }
        if matches!(
            &request.operation,
            Operation::ControlInspect { .. } | Operation::ControlPaneRead { .. }
        ) {
            bkpr_protocol::control::validate_native(&request.operation)?;
        }
        match request.operation {
            Operation::SessionInfo => Ok(json!({
                "session": self.inner.session,
                "spawn_defaults": self.inner.spawn_defaults.get(),
            })),
            Operation::ClientRegister { .. }
            | Operation::Client { .. }
            | Operation::ClientActivate { .. }
            | Operation::ClientDetach => {
                bail!("client operations require authenticated attachment transport")
            }
            Operation::Inspect => Ok(json!({"registry":self.snapshot()?})),
            Operation::ControlInspect { query } => {
                self.control_inspect(&query, control)
            }
            Operation::ControlPaneRead { pane_id, query } => {
                let pane = {
                    let state = self.inner.state.lock().unwrap();
                    healthy(&state)?;
                    state.live.get(&pane_id).cloned()
                        .context("pane is stopped; terminal contents were not recovered")?
                };
                return self.control_pane_read(&pane, &query, control, client);
            }
            Operation::CiRefresh { target } => {
                self.observe_control(control)?;
                let _transaction = self.inner.transaction.lock().unwrap();
                self.authorize_control(control)?;
                Ok(json!({"registry":self.refresh_ci(&target)?}))
            }
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
                #[cfg(test)]
                self.test_notify_request_wait();
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
                Ok(json!({"registry":registry_snapshot(&state)}))
            }
            Operation::WorkspaceCreate { name, initial } => {
                let id = Uuid::new_v4().to_string();
                if let Some(spec) = initial {
                    return self.spawn_control(
                        SpawnTarget::CreateWorkspace {
                            workspace_id: id,
                            window_id: Uuid::new_v4().to_string(),
                            name,
                            bootstrap: false,
                        },
                        spec,
                        control,
                    ).map(Payload::Control);
                }
                let registry = self.mutate_inner(None, control, |r| {
                    let state = self.inner.state.lock().unwrap();
                    workspace_creation_available(&state)?;
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
                let registry = self.mutate_inner(Some(&workspace_id), control, |r| {
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
            } => self.close_documents(&workspace_id, None, expected_revision, control),
            Operation::WindowCreate {
                workspace_id,
                expected_revision,
                name,
                spec,
            } => self.spawn_control(
                SpawnTarget::CreateWindow {
                    workspace_id,
                    window_id: Uuid::new_v4().to_string(),
                    expected_revision,
                    name,
                },
                spec,
                control,
            ),
            Operation::WindowRename {
                workspace_id,
                window_id,
                expected_revision,
                name,
            } => {
                valid_name(&name)?;
                let registry = self.mutate_inner(Some(&workspace_id), control, |r| {
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
            } => self.close_documents(&workspace_id, Some(&window_id), expected_revision, control),
            Operation::LayoutUpdate {
                workspace_id,
                window_id,
                expected_revision,
                layout,
            } => {
                let registry = self.mutate_inner(Some(&workspace_id), control, |r| {
                    if let Some(node) = &layout {
                        let panes = r.panes.iter().map(|pane| (pane.id.as_str(), pane)).collect();
                        validate_owned_layout(node, &panes, &workspace_id, &window_id)?;
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
                panes,
                layout,
            } => self.spawn_batch(
                SpawnTarget::Place {
                    workspace_id,
                    window_id,
                    expected_revision,
                    layout,
                },
                panes,
                None,
                None,
                true,
                control,
            ),
            Operation::PaneReopen {
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
                spec,
            } => self.spawn_control(
                SpawnTarget::Reopen {
                    workspace_id,
                    window_id,
                    expected_revision,
                    pane_id,
                },
                spec,
                control,
            ),
            Operation::PaneForget { pane_id } => self.forget(&pane_id, control),
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
                let registry = self.mutate_record_inner(workspace_id.as_deref(), control, |record| {
                    record.automatic_titles.remove(&pane_id);
                    let r = &mut record.registry;
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
                self.observe_control(control)?;
                {
                    let _transaction = self.inner.transaction.lock().unwrap();
                    self.authorize_control(control)?;
                }
                let (pane, workspace_id, window_id) = {
                    let state = self.inner.state.lock().unwrap();
                    healthy(&state)?;
                    let info = state.record.registry.panes.iter().find(|pane| pane.id == pane_id).context("unknown pane")?;
                    let pane = state.live.get(&pane_id).cloned().context("pane execution unavailable; reopen it or explicitly close its window/workspace")?;
                    (pane, info.workspace_id.clone(), info.window_id.clone())
                };
                pane.request(PaneRequest::Shutdown)?;
                self.record_completion(&pane_id)?;
                let state = self.inner.state.lock().unwrap();
                Ok(json!({
                    "registry":response_snapshot_target(&state, control, workspace_id.as_deref().map(|id| (id, window_id.as_deref()))),
                    "pane_id":pane_id,"workspace_id":workspace_id,"window_id":window_id,
                }))
            }
            Operation::Pane { pane_id, request } => {
                ensure!(
                    !matches!(request, PaneRequest::Shutdown),
                    "use pane_terminate for lifecycle control"
                );
                let pane = {
                    let state = self.inner.state.lock().unwrap();
                    healthy(&state)?;
                    state
                        .live
                        .get(&pane_id)
                        .cloned()
                        .context("pane is stopped; terminal contents were not recovered")?
                };
                if matches!(&request, PaneRequest::Attach { .. })
                    && pane
                        .shared
                        .finished
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    // Completed workers no longer expire leases on a timer.
                    // Reclaim this pane's stale authority before attaching a new view.
                    let expired = pane
                        .shared
                        .authority
                        .lock()
                        .unwrap()
                        .expire(std::time::Instant::now());
                    if !expired.is_empty() {
                        {
                            let mut baselines = pane.shared.delta_frames.lock().unwrap();
                            for id in expired {
                                baselines.remove(&id);
                            }
                        }
                        pane.shared.changes.notify();
                    }
                }
                let headless = match &request {
                    PaneRequest::Poll { view_id, .. } | PaneRequest::Input { view_id, .. }
                    | PaneRequest::Paste { view_id, .. } | PaneRequest::Key { view_id, .. } => view_id.is_empty(),
                    PaneRequest::Inspect => control.is_some(),
                    _ => false,
                };
                if headless {
                    return self.control_pane_request(&pane, request, control, client);
                }
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
                return Ok(result);
            }
            Operation::Shutdown => {
                self.shutdown()?;
                Ok(json!({"shutdown":true}))
            }
        }.map(Payload::Control)
    }
    fn spawn(&self, target: SpawnTarget, spec: PaneSpawnSpec) -> Result<Value> {
        self.spawn_inner(target, spec, None, None)
    }
    fn spawn_control(
        &self,
        target: SpawnTarget,
        spec: PaneSpawnSpec,
        control: Option<&control::ControlOrigin<'_>>,
    ) -> Result<Value> {
        self.spawn_batch(
            target,
            vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec,
            }],
            None,
            None,
            false,
            control,
        )
    }
    fn spawn_inner(
        &self,
        target: SpawnTarget,
        spec: PaneSpawnSpec,
        resume: Option<recovery::ResumeAttempt>,
        origin: Option<(&crate::harness::Launch, &crate::harness::Peer)>,
    ) -> Result<Value> {
        self.spawn_batch(
            target,
            vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec,
            }],
            resume,
            origin,
            false,
            None,
        )
    }
    fn spawn_batch(
        &self,
        mut target: SpawnTarget,
        entries: Vec<PaneSpawnEntry>,
        resume: Option<recovery::ResumeAttempt>,
        origin: Option<(&crate::harness::Launch, &crate::harness::Peer)>,
        report_created: bool,
        control: Option<&control::ControlOrigin<'_>>,
    ) -> Result<Value> {
        ensure!(!entries.is_empty(), "invalid pane batch size");
        ensure!(
            matches!(target, SpawnTarget::Place { .. }) || entries.len() == 1,
            "internal spawn requires one pane"
        );
        let count = entries.len();
        let automatic_titles: Vec<bool> = entries
            .iter()
            .map(|entry| entry.spec.title.is_none())
            .collect();
        let workspace_id = target.workspace_id().map(str::to_owned);
        let mut created_panes = BTreeMap::new();
        let mut infos = Vec::with_capacity(count);
        for entry in &entries {
            ensure!(
                !entry.reference.is_empty() && entry.reference.len() <= 256,
                "invalid pane reference"
            );
            let title = validate_spawn_spec(&entry.spec)?;
            let id = match &target {
                SpawnTarget::Restore { pane_id, .. } => pane_id.clone(),
                _ => Uuid::new_v4().to_string(),
            };
            ensure!(
                created_panes
                    .insert(entry.reference.clone(), id.clone())
                    .is_none(),
                "duplicate pane reference"
            );
            infos.push(PaneInfo {
                id,
                workspace_id: workspace_id.clone(),
                window_id: target.window_id().map(str::to_owned),
                title,
                status: PaneStatus::Running,
                exit_code: None,
                cwd: entry.spec.cwd.clone(),
                home: entry.spec.home.clone(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                harness_status_changed_at_ms: None,
                project: Some(project::loading(&entry.spec.cwd)),
                isolated_environment: entry.spec.isolated_environment,
            });
        }
        let id = infos[0].id.clone();
        let layout = match &target {
            SpawnTarget::Place {
                layout: Some(layout),
                ..
            } => Some(resolve_spawn_layout(layout, &created_panes)?),
            _ => None,
        };
        if let Some((launch, peer)) = origin {
            crate::harness::validate_callback(peer, launch)?;
        }
        self.observe_control(control)?;
        let (workspace, candidate) = {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(control)?;
            if let Some((launch, _)) = origin {
                self.authorize_resume_origin(launch)?;
            }
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            ensure!(
                !self.session_end_requested(),
                SessionClosing("session has ended")
            );
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
                return Ok(json!({"registry":response_snapshot(&state, control)}));
            }
            target.check(&state)?;
            if let SpawnTarget::Place {
                layout: Some(layout),
                ..
            } = &target
            {
                validate_spawn_existing(layout, &state.record.registry.panes)?;
            }
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
            let candidate = spawn_record(
                state.record.clone(),
                &infos,
                &automatic_titles,
                &target,
                layout.as_ref(),
            )?;
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
            state.spawning += count;
            (workspace, candidate)
        };
        let mut reservation = SpawnReservation {
            inner: self.inner.clone(),
            workspace_id: workspace_id.clone(),
            resume_pane: resume.as_ref().map(|_| id.clone()),
            committed: false,
            count,
        };
        // Size validation uses a bounded sink, outside registry and writer locks.
        serde_json::to_writer(RecordSize(0), &durable_record(&candidate))?;
        drop(candidate);
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
        // Rollback owns only unpublished handles: no recovery callbacks are installed
        // until durable publication. Its shutdown/reaping runs outside registry locks.
        let mut children = SpawnChildren {
            panes: Vec::with_capacity(count),
            committed: false,
        };
        // Prepare every HOME/CWD before the first exec. Isolated setup may create
        // directories, but a later invalid filesystem prerequisite cannot run an
        // earlier command. Filesystem races after this phase remain startup errors.
        let prepared = entries
            .into_iter()
            .map(|entry| {
                let spec = entry.spec;
                crate::PreparedSpawnSpec::new(SpawnSpec {
                    argv: spec.argv,
                    home: spec.home,
                    cwd: spec.cwd,
                    size: spec.size,
                    isolated_process: false,
                    isolated_environment: spec.isolated_environment,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        for (spec, info) in prepared.into_iter().zip(&infos) {
            self.observe_control(control)?;
            if control.is_some() {
                let _transaction = self.inner.transaction.lock().unwrap();
                self.authorize_control(control)?;
            }
            let pane = Pane::spawn_with_id(
                spec,
                info.id.clone(),
                environment.clone(),
                Some(self.inner.status_wake.clone()),
                Some(crate::SpawnContext {
                    session: (
                        self.inner.session.session_id.clone(),
                        self.inner.session.name.clone(),
                    ),
                    workspace: workspace.clone(),
                }),
                ssh_agent_socket.clone(),
            )?;
            children.panes.push(pane);
            #[cfg(test)]
            {
                let hook = self.inner.spawn_started.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook(children.panes.last().unwrap());
                }
            }
        }
        let result = (|| -> Result<Value> {
            self.observe_control(control)?;
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(control)?;
            let current = {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(!state.closing, SessionClosing("daemon is shutting down"));
                ensure!(
                    !self.session_end_requested(),
                    SessionClosing("session has ended")
                );
                target.check(&state)?;
                if let Some(resume) = &resume {
                    recovery::check_resume_reserved(&state, resume, &id)?;
                }
                state.record.clone()
            };
            // Rebuild against current metadata, merging unrelated workspace commits.
            let next = spawn_record(current, &infos, &automatic_titles, &target, layout.as_ref())?;
            commit_projected(
                &self.inner,
                next,
                |state| {
                    for (info, pane) in infos.iter().zip(&children.panes) {
                        self.install_spawn_callbacks(&info.id, pane);
                        state.live.insert(info.id.clone(), pane.clone());
                    }
                },
                |state| {
                    response_snapshot_target(
                        state,
                        control,
                        target.workspace_id().map(|id| (id, target.window_id())),
                    )
                },
            )
        })();
        let registry = result?;
        children.committed = true;
        reservation.committed = true;
        // A process can exit before callbacks are attached. Process every such
        // completion while the batch reservation still protects final membership.
        for (info, pane) in infos.iter().zip(&children.panes) {
            if pane
                .shared
                .finished
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.record_completion(&info.id)?;
            }
        }
        drop(reservation);
        let registry = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            if Some(state.record.registry.revision) == registry["revision"].as_u64() {
                registry
            } else {
                response_snapshot_target(
                    &state,
                    control,
                    target.workspace_id().map(|id| (id, target.window_id())),
                )
            }
        };
        let mut result = json!({
            "registry": registry,
            "pane_id": if count == 1 { Some(id) } else { None },
            "workspace_id": workspace_id,
            "window_id": target.window_id(),
        });
        if report_created {
            result["created_panes"] = json!(created_panes);
        }
        Ok(result)
    }
    fn install_spawn_callbacks(&self, id: &str, pane: &Pane) {
        let weak = Arc::downgrade(&self.inner);
        let terminating_id = id.to_owned();
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
        let weak = Arc::downgrade(&self.inner);
        let notify_id = id.to_owned();
        *pane.shared.on_finish.lock().unwrap() = Some(Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                let _ = Registry {
                    inner,
                    fresh: false,
                    lifecycle_owner: false,
                }
                .record_completion(&notify_id);
            }
        }));
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
    fn forget(&self, id: &str, control: Option<&control::ControlOrigin<'_>>) -> Result<Value> {
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
        self.observe_control(control)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        self.authorize_control(control)?;
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, SessionClosing("daemon is shutting down"));
            ensure!(
                !self.session_end_requested(),
                SessionClosing("session has ended")
            );
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
            if let Some(window_id) = &info.window_id {
                window_available(&state, window_id)?;
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
        let (workspace_id, window_id) = next
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .map(|pane| (pane.workspace_id.clone(), pane.window_id.clone()))
            .unwrap_or_default();
        next.registry.panes.retain(|pane| pane.id != id);
        next.recoveries.remove(id);
        next.directories.remove(id);
        next.automatic.remove(id);
        next.automatic_titles.remove(id);
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        let registry = commit_projected(
            &self.inner,
            next,
            |state| {
                if let Some(pane) = &pane {
                    pane.shared
                        .forgotten
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                state.live.remove(id);
                state.closing_panes.remove(id);
            },
            |state| {
                response_snapshot_target(
                    state,
                    control,
                    workspace_id.as_deref().map(|id| (id, window_id.as_deref())),
                )
            },
        )?;
        Ok(
            json!({"registry":registry,"pane_id":id,"workspace_id":workspace_id,"window_id":window_id}),
        )
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
            published.status_changed_at_ms.clear();
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
                || state.closing_document_panes.contains(id)
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
        info.harness_status_changed_at_ms = None;
        info.project = None;
        if status == PaneStatus::Exited && !closing && !reserved {
            close_membership(&mut next.registry, id)?;
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit_projected(&self.inner, next, |_| (), |_| ())?;
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
        control: Option<&control::ControlOrigin<'_>>,
    ) -> Result<Value> {
        self.observe_control(control)?;
        let (panes, closing_documents, reserves_workspace) = {
            let _transaction = self.inner.transaction.lock().unwrap();
            self.authorize_control(control)?;
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(
                !state.closing && !self.session_end_requested(),
                SessionClosing("session is closing")
            );
            workspace_available(&state, workspace_id)?;
            let w = state
                .record
                .registry
                .workspaces
                .iter()
                .find(|w| w.id == workspace_id)
                .context("unknown workspace")?;
            if window_id.is_none() {
                if let Some(window) = w
                    .windows
                    .iter()
                    .find(|window| state.closing_windows.contains(&window.id))
                {
                    bail!(RegistryBusy {
                        target_id: window.id.clone()
                    });
                }
            } else if let Some(window_id) = window_id {
                window_available(&state, window_id)?;
            }
            // The last available window owns the workspace/session transition.
            // Other window closes reserve only their own window and pane documents.
            let reserves_workspace = window_id.is_none()
                || w.windows
                    .iter()
                    .filter(|window| !state.closing_windows.contains(&window.id))
                    .count()
                    <= 1;
            if reserves_workspace {
                ensure!(
                    !state.spawn_workspaces.iter().any(|id| !state
                        .record
                        .registry
                        .workspaces
                        .iter()
                        .any(|w| &w.id == id)),
                    "session busy: workspace creation in progress"
                );
            }
            if let Some(id) = window_id {
                let window = w
                    .windows
                    .iter()
                    .find(|w| w.id == id)
                    .context("unknown window")?;
                ensure!(
                    window.revision == revision,
                    "stale window revision for {id}: current {}",
                    window.revision
                );
            } else {
                ensure!(
                    w.revision == revision,
                    "stale workspace revision for {workspace_id}: current {}",
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
                commit_projected(&self.inner, next, |_| {}, |_| ())?;
                state = self.inner.state.lock().unwrap();
            }
            if reserves_workspace {
                state.closing_workspaces.insert(workspace_id.to_owned());
            }
            if let Some(window_id) = window_id {
                state.closing_windows.insert(window_id.to_owned());
            }
            state.closing_document_panes.extend(removed.iter().cloned());
            (panes, removed, reserves_workspace)
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
        let result = (|| -> Result<Value> {
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
            commit_projected(
                &self.inner,
                next,
                |_| {},
                |state| response_snapshot(state, control),
            )
            .context("close executions completed but document persistence failed; restart required")
        })();
        {
            let mut state = self.inner.state.lock().unwrap();
            if reserves_workspace {
                state.closing_workspaces.remove(workspace_id);
            }
            if let Some(window_id) = window_id {
                state.closing_windows.remove(window_id);
            }
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
            response_snapshot(&self.inner.state.lock().unwrap(), control)
        } else {
            registry
        };
        let workspace_closed = !registry["workspaces"].as_array().is_some_and(|workspaces| {
            workspaces
                .iter()
                .any(|workspace| workspace["id"] == workspace_id)
        });
        crate::diagnostics::record(
            "document.close",
            Some(window_id.unwrap_or(workspace_id)),
            "completed",
        );
        Ok(json!({
            "registry":registry,"workspace_id":workspace_id,"window_id":window_id,
            "workspace_closed":workspace_closed,"window_closed":window_id.is_some(),
            "closed_pane_ids":closing_documents,
        }))
    }
    /// Shuts down live executions and backend services while retaining durable
    /// workspace and recovery intent.
    ///
    /// Flushes directory observations before teardown, closes mutation admission,
    /// revokes harness control and client attachments, and waits for accepted
    /// spawns to finish or abort. The state-root lease remains held by the
    /// registry handles.
    ///
    /// # Errors
    ///
    /// Continues teardown after individual failures and returns the first
    /// recorded directory, service, pane-shutdown, or completion-persistence
    /// error. An error does not mean teardown was rolled back.
    pub fn shutdown(&self) -> Result<()> {
        // Observe while the process tree still exists; never derive intent from teardown.
        self.observe_processes();
        let directory_capture = self.flush_directories();
        let panes = {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            state.closing = true;
            let mut captures = self.inner.captures.lock().unwrap();
            for launch in captures.launches.values() {
                launch.control.revoke();
            }
            captures.launches.clear();
            self.inner.changed.notify_all();
            state.live.values().cloned().collect::<Vec<_>>()
        };
        self.detach_all_clients("shutdown");
        let agent_shutdown = self.inner.agents.shutdown();
        let forwarding = self.inner.ssh_agent.lock().unwrap().clone();
        let forwarding_shutdown = forwarding.map(|relay| relay.shutdown()).transpose();
        self.stop_projects();
        self.stop_ci();
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
        RegistryBusy {
            target_id: workspace_id.to_owned()
        }
    );
    ensure!(
        !state.spawn_workspaces.contains(workspace_id),
        "workspace busy: pane spawn in progress"
    );
    Ok(())
}
fn window_available(state: &State, window_id: &str) -> Result<()> {
    ensure!(
        !state.closing_windows.contains(window_id),
        RegistryBusy {
            target_id: window_id.to_owned()
        }
    );
    Ok(())
}
fn workspace_creation_available(state: &State) -> Result<()> {
    if !state.record.registry.workspaces.is_empty()
        && state
            .record
            .registry
            .workspaces
            .iter()
            .all(|workspace| state.closing_workspaces.contains(&workspace.id))
    {
        bail!(RegistryBusy {
            target_id: state.record.registry.workspaces[0].id.clone()
        });
    }
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
fn spawn_record(
    mut next: Record,
    infos: &[PaneInfo],
    automatic_titles: &[bool],
    target: &SpawnTarget,
    layout: Option<&LayoutNode>,
) -> Result<Record> {
    let info = &infos[0];
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
        *old = info.clone();
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
        } else if matches!(target, SpawnTarget::Place { .. }) {
            window.layout = Some(layout.context("bound spawn requires layout")?.clone());
        } else {
            window.layout = Some(LayoutNode::Leaf {
                pane_id: info.id.clone(),
            });
        }
        window.revision = next_revision(window.revision)?;
        workspace.revision = next_revision(workspace.revision)?;
    }
    next.registry.panes.extend_from_slice(infos);
    let first_new = next.registry.panes.len() - infos.len();
    for (index, (info, automatic)) in infos.iter().zip(automatic_titles).enumerate() {
        if *automatic {
            let inherited = match target {
                SpawnTarget::Reopen { pane_id, .. } if !next.automatic_titles.contains(pane_id) => {
                    next.registry
                        .panes
                        .iter()
                        .find(|pane| &pane.id == pane_id)
                        .map(|pane| pane.title.clone())
                }
                _ => None,
            };
            if let Some(title) = inherited {
                next.registry.panes[first_new + index].title = title;
            } else {
                next.automatic_titles.insert(info.id.clone());
            }
        }
    }
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
        "stale workspace revision for {id}: current {}",
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
        "stale window revision for {id}: current {}",
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
                "stale window revision for {window_id}: current {}",
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
    // Among n existing names, at least one of n + 1 candidates is unused.
    for suffix in 1..=existing.len() + 1 {
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
fn existing_spawn_layout(node: &LayoutNode) -> SpawnLayoutNode {
    match node {
        LayoutNode::Leaf { pane_id } => SpawnLayoutNode::Existing {
            pane_id: pane_id.clone(),
        },
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => SpawnLayoutNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(existing_spawn_layout(first)),
            second: Box::new(existing_spawn_layout(second)),
        },
    }
}
fn validate_spawn_arguments(argv: &[String], cwd: &Path, home: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    ensure!(
        !argv.is_empty()
            && !argv[0].is_empty()
            && argv.len() <= 256
            && argv.iter().map(String::len).sum::<usize>() <= 128 * 1024
            && argv.iter().all(|arg| !arg.contains('\0')),
        "invalid or excessive argv"
    );
    ensure!(
        cwd.is_absolute()
            && home.is_absolute()
            && !cwd.as_os_str().as_bytes().contains(&0)
            && !home.as_os_str().as_bytes().contains(&0),
        "invalid spawn paths"
    );
    Ok(())
}
fn validate_spawn_spec(spec: &PaneSpawnSpec) -> Result<String> {
    valid_size(spec.size)?;
    validate_spawn_arguments(&spec.argv, &spec.cwd, &spec.home)?;
    let title = spec.title.clone().unwrap_or_else(|| {
        Path::new(&spec.argv[0])
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    valid_name(&title)?;
    Ok(title)
}
fn resolve_spawn_layout(
    node: &SpawnLayoutNode,
    refs: &BTreeMap<String, String>,
) -> Result<LayoutNode> {
    fn walk(
        node: &SpawnLayoutNode,
        refs: &BTreeMap<String, String>,
        seen: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<LayoutNode> {
        ensure!(depth <= MAX_LAYOUT_DEPTH, "layout depth limit");
        Ok(match node {
            SpawnLayoutNode::Existing { pane_id } => LayoutNode::Leaf {
                pane_id: pane_id.clone(),
            },
            SpawnLayoutNode::New { reference } => {
                let pane_id = refs.get(reference).context("unknown pane reference")?;
                ensure!(
                    seen.insert(reference.clone()),
                    "duplicate pane reference in layout"
                );
                LayoutNode::Leaf {
                    pane_id: pane_id.clone(),
                }
            }
            SpawnLayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                ensure!(
                    (1..1000).contains(ratio),
                    "split ratio must be 1..999 thousandths"
                );
                LayoutNode::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(walk(first, refs, seen, depth + 1)?),
                    second: Box::new(walk(second, refs, seen, depth + 1)?),
                }
            }
        })
    }
    let mut seen = BTreeSet::new();
    let layout = walk(node, refs, &mut seen, 0)?;
    ensure!(
        seen.len() == refs.len(),
        "layout must include every new pane exactly once"
    );
    Ok(layout)
}
fn validate_spawn_existing(node: &SpawnLayoutNode, panes: &[PaneInfo]) -> Result<()> {
    match node {
        SpawnLayoutNode::Existing { pane_id } => ensure!(
            panes.iter().any(|pane| &pane.id == pane_id),
            "layout refers to unknown existing pane"
        ),
        SpawnLayoutNode::New { .. } => {}
        SpawnLayoutNode::Split { first, second, .. } => {
            validate_spawn_existing(first, panes)?;
            validate_spawn_existing(second, panes)?;
        }
    }
    Ok(())
}
fn validate_owned_layout(
    node: &LayoutNode,
    panes: &BTreeMap<&str, &PaneInfo>,
    workspace_id: &str,
    window_id: &str,
) -> Result<()> {
    fn walk<'a>(
        node: &'a LayoutNode,
        panes: &BTreeMap<&str, &PaneInfo>,
        workspace_id: &str,
        window_id: &str,
        seen: &mut BTreeSet<&'a str>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth <= MAX_LAYOUT_DEPTH, "layout depth limit");
        match node {
            LayoutNode::Leaf { pane_id } => {
                let pane = panes
                    .get(pane_id.as_str())
                    .context("layout refers to unknown pane")?;
                ensure!(
                    seen.insert(pane_id.as_str()),
                    "duplicate pane in workspace layout"
                );
                ensure!(
                    pane.workspace_id.as_deref() == Some(workspace_id)
                        && pane.window_id.as_deref() == Some(window_id),
                    "layout pane belongs to another workspace/window"
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
                walk(first, panes, workspace_id, window_id, seen, depth + 1)?;
                walk(second, panes, workspace_id, window_id, seen, depth + 1)?;
            }
        }
        Ok(())
    }
    walk(
        node,
        panes,
        workspace_id,
        window_id,
        &mut BTreeSet::new(),
        0,
    )
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
    let mut panes = BTreeMap::new();
    for pane in &r.panes {
        ensure!(
            Uuid::parse_str(&pane.id).is_ok() && panes.insert(pane.id.as_str(), pane).is_none(),
            "invalid or duplicate pane id"
        );
        valid_name(&pane.title)?;
        ensure!(
            pane.cwd.is_absolute() && pane.home.is_absolute(),
            "invalid persisted pane paths"
        );
    }
    ensure!(
        record
            .automatic_titles
            .iter()
            .all(|id| panes.contains_key(id.as_str())),
        "automatic title refers to unknown pane"
    );
    ensure!(
        record.recoveries.len() <= r.panes.len(),
        "recovery descriptor limit"
    );
    for (id, descriptor) in &record.recoveries {
        ensure!(
            panes.contains_key(id.as_str()),
            "recovery refers to unknown pane"
        );
        recovery::validate(descriptor)?;
    }
    for (id, directory) in &record.directories {
        ensure!(
            panes.contains_key(id.as_str()),
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
    let mut workspaces = BTreeMap::new();
    for w in &r.workspaces {
        ensure!(
            Uuid::parse_str(&w.id).is_ok()
                && !workspaces.contains_key(w.id.as_str())
                && w.revision > 0,
            "invalid workspace identity/revision"
        );
        valid_name(&w.name)?;
        let mut window_ids = BTreeSet::new();
        for window in &w.windows {
            ensure!(
                Uuid::parse_str(&window.id).is_ok()
                    && window_ids.insert(window.id.as_str())
                    && window.revision > 0,
                "invalid window identity/revision"
            );
            valid_name(&window.name)?;
            if let Some(node) = &window.layout {
                validate_owned_layout(node, &panes, &w.id, &window.id)?;
            }
        }
        workspaces.insert(w.id.as_str(), window_ids);
    }
    for pane in &r.panes {
        match (&pane.workspace_id, &pane.window_id) {
            (Some(workspace_id), Some(window_id)) => {
                let windows = workspaces
                    .get(workspace_id.as_str())
                    .context("unknown workspace")?;
                ensure!(windows.contains(window_id.as_str()), "unknown window");
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
fn durable_record(record: &Record) -> DurableRecord<'_> {
    DurableRecord {
        version: record.version,
        recoveries: &record.recoveries,
        directories: &record.directories,
        automatic: &record.automatic,
        automatic_titles: &record.automatic_titles,
        registry: DurableRegistry {
            revision: record.registry.revision,
            workspaces: &record.registry.workspaces,
            panes: DurablePanes(&record.registry.panes),
        },
    }
}
struct RecordSize(u64);
impl Write for RecordSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let remaining = MAX_RECORD.saturating_sub(self.0);
        if bytes.len() as u64 > remaining {
            return Err(std::io::Error::other("registry file limit"));
        }
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn persist(root: &Path, record: &Record) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(temporary.as_file_mut(), &durable_record(record))?;
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
fn project_title(state: &State, id: &str, title: &mut String) {
    if state.record.automatic_titles.contains(id)
        && let Some(pane) = state.live.get(id)
        && let Some(native) = &pane.shared.published.lock().unwrap().title
    {
        title.clone_from(native);
    }
}
fn registry_snapshot(state: &State) -> RegistrySnapshot {
    let mut registry = state.record.registry.clone();
    for pane in &mut registry.panes {
        project_title(state, &pane.id, &mut pane.title);
    }
    registry
}
/// Build only elementary target revision metadata for a compact control receipt.
/// The native/TUI and explicit full paths still receive the canonical snapshot.
fn response_snapshot(state: &State, origin: Option<&control::ControlOrigin<'_>>) -> Value {
    response_snapshot_target(state, origin, None)
}
fn response_snapshot_target(
    state: &State,
    origin: Option<&control::ControlOrigin<'_>>,
    target: Option<(&str, Option<&str>)>,
) -> Value {
    let Some(origin) =
        origin.filter(|origin| origin.reply == bkpr_protocol::control::ControlReply::Receipt)
    else {
        return json!(registry_snapshot(state));
    };
    let registry = &state.record.registry;
    let (workspace_id, window_id, pane_id) = match &origin.operation {
        Operation::WorkspaceCreate { .. } => (
            registry
                .workspaces
                .last()
                .map(|workspace| workspace.id.as_str()),
            registry
                .workspaces
                .last()
                .and_then(|workspace| workspace.windows.last())
                .map(|window| window.id.as_str()),
            None,
        ),
        Operation::WorkspaceRename { workspace_id, .. }
        | Operation::WorkspaceClose { workspace_id, .. } => {
            (Some(workspace_id.as_str()), None, None)
        }
        Operation::WindowCreate { workspace_id, .. } => (
            Some(workspace_id.as_str()),
            registry
                .workspaces
                .iter()
                .find(|w| &w.id == workspace_id)
                .and_then(|w| w.windows.last())
                .map(|w| w.id.as_str()),
            None,
        ),
        Operation::WindowRename {
            workspace_id,
            window_id,
            ..
        }
        | Operation::WindowClose {
            workspace_id,
            window_id,
            ..
        }
        | Operation::LayoutUpdate {
            workspace_id,
            window_id,
            ..
        }
        | Operation::PaneReopen {
            workspace_id,
            window_id,
            ..
        } => (Some(workspace_id.as_str()), Some(window_id.as_str()), None),
        Operation::PaneSpawn {
            workspace_id,
            window_id,
            ..
        } => (workspace_id.as_deref(), window_id.as_deref(), None),
        Operation::PaneRename { pane_id, .. }
        | Operation::PaneTerminate { pane_id }
        | Operation::PaneForget { pane_id }
        | Operation::PaneResume { pane_id, .. } => (None, None, Some(pane_id.as_str())),
        _ => (None, None, None),
    };
    let (workspace_id, window_id) = target
        .map_or((workspace_id, window_id), |(workspace, window)| {
            (Some(workspace), window)
        });
    let pane = pane_id.and_then(|id| registry.panes.iter().find(|pane| pane.id == id));
    let workspace_id = workspace_id.or_else(|| pane.and_then(|pane| pane.workspace_id.as_deref()));
    let window_id = window_id.or_else(|| pane.and_then(|pane| pane.window_id.as_deref()));
    let workspaces = registry.workspaces.iter().filter(|workspace| Some(workspace.id.as_str()) == workspace_id)
        .map(|workspace| json!({
            "id":workspace.id, "revision":workspace.revision,
            "windows":workspace.windows.iter().filter(|window| Some(window.id.as_str()) == window_id)
                .map(|window| json!({"id":window.id,"revision":window.revision})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>();
    let panes = pane.map(|pane| json!({"id":pane.id,"workspace_id":pane.workspace_id,"window_id":pane.window_id})).into_iter().collect::<Vec<_>>();
    json!({"revision":registry.revision,"workspaces":workspaces,"panes":panes})
}
// Caller owns transaction; no state guard may cross this durability boundary.
fn commit(
    inner: &Inner,
    next: Record,
    publish: impl FnOnce(&mut State),
) -> Result<RegistrySnapshot> {
    commit_projected(inner, next, publish, registry_snapshot)
}
fn commit_projected<T>(
    inner: &Inner,
    next: Record,
    publish: impl FnOnce(&mut State),
    response: impl FnOnce(&State) -> T,
) -> Result<T> {
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
    inner
        .ci_input_generation
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let _ = inner.ci_wake.try_send(());
    Ok(response(&state))
}

#[cfg(test)]
mod tests;
