//! Backend-owned workspace intent. Persisted metadata is not PTY recovery.
//! Atomic tempfile/fsync/rename follows main's recovery.rs, without its session/KDL assumptions.
use crate::{Pane, SpawnSpec};
mod agents;
mod capture;
mod project;
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
const STORAGE_VERSION: u32 = 1;
const MAX_RECORD: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u32,
    registry: RegistrySnapshot,
}
// Persistence is an explicit intent whitelist, not serialization of live facts.
// Borrow the record: a metadata write need not copy every foreground observation.
#[derive(Serialize)]
struct DurableRecord<'a> {
    version: u32,
    registry: DurableRegistry<'a>,
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
    observer_started: std::sync::atomic::AtomicBool,
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
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.project_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.project_wake.try_send(());
        // The retention worker owns only Agents, not the registry lease. Drain its
        // writer and close admission before releasing ownership of the durable root.
        let _ = self.agents.shutdown();
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
        expected_revision: Option<u64>,
    },
    Reopen {
        workspace_id: String,
        expected_revision: u64,
        pane_id: String,
    },
    Bootstrap {
        workspace_id: String,
    },
}
impl SpawnTarget {
    fn workspace_id(&self) -> Option<&str> {
        match self {
            Self::Append { workspace_id, .. } => workspace_id.as_deref(),
            Self::Reopen { workspace_id, .. } | Self::Bootstrap { workspace_id } => {
                Some(workspace_id)
            }
        }
    }
    fn check(&self, state: &State) -> Result<()> {
        match self {
            Self::Append {
                workspace_id,
                expected_revision,
            } => check_spawn_workspace(
                &state.record.registry,
                workspace_id.as_deref(),
                *expected_revision,
            ),
            Self::Reopen {
                workspace_id,
                expected_revision,
                pane_id,
            } => {
                check_spawn_workspace(
                    &state.record.registry,
                    Some(workspace_id),
                    Some(*expected_revision),
                )?;
                let pane = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|pane| &pane.id == pane_id)
                    .context("unknown pane")?;
                ensure!(
                    pane.status != PaneStatus::Running
                        && state.live.get(pane_id).is_none_or(|pane| pane
                            .shared
                            .finished
                            .load(std::sync::atomic::Ordering::Acquire)),
                    "cannot reopen a live pane; open a new pane instead"
                );
                ensure!(
                    state
                        .record
                        .registry
                        .workspaces
                        .iter()
                        .find(|workspace| &workspace.id == workspace_id)
                        .and_then(|workspace| workspace.layout.as_ref())
                        .is_some_and(|layout| layout_references(layout, pane_id)),
                    "workspace layout does not reference pane"
                );
                Ok(())
            }
            Self::Bootstrap { .. } => {
                ensure!(
                    state.record.registry.workspaces.len() < MAX_WORKSPACES,
                    "workspace limit"
                );
                Ok(())
            }
        }
    }
}
struct SpawnReservation {
    inner: Arc<Inner>,
    workspace_id: Option<String>,
}
impl Drop for SpawnReservation {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        state.spawning -= 1;
        if let Some(workspace_id) = &self.workspace_id {
            state.spawn_workspaces.remove(workspace_id);
        }
        self.inner.changed.notify_all();
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
                    closing: false,
                    durability_error: None,
                }),
                transaction: Mutex::new(()),
                view_budget: Arc::default(),
                captures: Mutex::new(crate::harness::Captures::default()),
                agents,
                spawn_environment: Mutex::new(None),
                observer_started: std::sync::atomic::AtomicBool::new(false),
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
            Operation::WorkspaceCreate { name } => {
                valid_name(&name)?;
                let id = Uuid::new_v4().to_string();
                let registry = self.mutate(|r| {
                    ensure!(r.workspaces.len() < MAX_WORKSPACES, "workspace limit");
                    r.workspaces.push(Workspace {
                        id: id.clone(),
                        name,
                        revision: 1,
                        layout: None,
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
            Operation::WorkspaceRemove {
                workspace_id,
                expected_revision,
            } => {
                let registry = self.mutate_workspace(&workspace_id, |r| {
                    workspace(r, &workspace_id, expected_revision)?;
                    r.workspaces.retain(|w| w.id != workspace_id);
                    for pane in &mut r.panes {
                        if pane.workspace_id.as_deref() == Some(workspace_id.as_str()) {
                            pane.workspace_id = None;
                        }
                    }
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::LayoutUpdate {
                workspace_id,
                expected_revision,
                layout,
            } => {
                let registry = self.mutate_workspace(&workspace_id, |r| {
                    if let Some(node) = &layout {
                        validate_layout(node, &r.panes)?;
                    }
                    let w = workspace(r, &workspace_id, expected_revision)?;
                    w.layout = layout;
                    w.revision = next_revision(w.revision)?;
                    Ok(())
                })?;
                Ok(json!({"registry":registry}))
            }
            Operation::PaneSpawn {
                workspace_id,
                expected_revision,
                spec,
            } => self.spawn(
                SpawnTarget::Append {
                    workspace_id,
                    expected_revision,
                },
                spec,
            ),
            Operation::PaneReopen {
                workspace_id,
                expected_revision,
                pane_id,
                spec,
            } => self.spawn(
                SpawnTarget::Reopen {
                    workspace_id,
                    expected_revision,
                    pane_id,
                },
                spec,
            ),
            Operation::PaneForget { pane_id } => self.forget(&pane_id),
            Operation::PaneRename { pane_id, title } => {
                valid_name(&title)?;
                let registry = self.mutate(|r| {
                    r.panes
                        .iter_mut()
                        .find(|p| p.id == pane_id)
                        .context("unknown pane")?
                        .title = title;
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
                    state.live.get(&pane_id).cloned()
                };
                if let Some(pane) = pane {
                    pane.request(PaneRequest::Shutdown)?;
                }
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
    /// Cold-daemon bootstrap only: supplied current argv, never persisted commands.
    /// A running execution makes this a no-op; metadata-only state is preserved.
    pub fn bootstrap(&self, spec: PaneSpawnSpec) -> Result<Value> {
        self.spawn(
            SpawnTarget::Bootstrap {
                workspace_id: Uuid::new_v4().to_string(),
            },
            spec,
        )
    }
    fn spawn(&self, target: SpawnTarget, spec: PaneSpawnSpec) -> Result<Value> {
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
        let id = Uuid::new_v4().to_string();
        let info = PaneInfo {
            id: id.clone(),
            workspace_id: workspace_id.clone(),
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
        {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, "daemon is shutting down");
            if matches!(target, SpawnTarget::Bootstrap { .. })
                && state.record.registry.panes.iter().any(|pane| {
                    pane.status == PaneStatus::Running
                        && state.live.get(&pane.id).is_some_and(|pane| {
                            !pane
                                .shared
                                .finished
                                .load(std::sync::atomic::Ordering::Acquire)
                        })
                })
            {
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
                state.record.registry.panes.len() + state.spawning < MAX_PANE_RECORDS
                    && running + state.spawning < MAX_LIVE_PANES,
                "pane limit"
            );
            target.check(&state)?;
            if let Some(workspace_id) = &workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            // Validate the exact layout and revisions before argv can execute.
            spawn_record(state.record.clone(), info.clone(), &target)?;
            if let Some(workspace_id) = &workspace_id {
                state.spawn_workspaces.insert(workspace_id.clone());
            }
            state.spawning += 1;
        }
        let _reservation = SpawnReservation {
            inner: self.inner.clone(),
            workspace_id: workspace_id.clone(),
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
            Some((
                self.inner.session.session_id.clone(),
                self.inner.session.name.clone(),
            )),
        );
        let pane = spawned?;
        let result = (|| -> Result<RegistrySnapshot> {
            let _transaction = self.inner.transaction.lock().unwrap();
            let current = {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(!state.closing, "daemon is shutting down");
                target.check(&state)?;
                state.record.clone()
            };
            let next = spawn_record(current, info, &target)?;
            commit(&self.inner, next, |state| {
                state.live.insert(id.clone(), pane.clone());
            })
        })();
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
        if matches!(target, SpawnTarget::Bootstrap { .. }) {
            Ok(json!({"registry":registry,"pane_id":id,"workspace_id":workspace_id}))
        } else {
            Ok(json!({"registry":registry,"pane_id":id}))
        }
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
            let info = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == id)
                .context("unknown pane")?;
            ensure!(
                info.status != PaneStatus::Running,
                "cannot forget a running pane"
            );
            for workspace in &state.record.registry.workspaces {
                ensure!(
                    !workspace
                        .layout
                        .as_ref()
                        .is_some_and(|layout| layout_references(layout, id)),
                    "pane is still referenced by workspace {} layout; remove its layout references first",
                    workspace.id
                );
            }
            state.record.clone()
        };
        next.registry.panes.retain(|pane| pane.id != id);
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        let registry = commit(&self.inner, next, |state| {
            if let Some(pane) = &pane {
                pane.shared
                    .forgotten
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            state.live.remove(id);
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
        let (mut next, closing) = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            (state.record.clone(), state.closing)
        };
        let Some(info) = next.registry.panes.iter_mut().find(|pane| pane.id == id) else {
            // A concurrent successful PaneForget may retire an already-completed handle.
            return Ok(());
        };
        if info.status == status && info.exit_code == exit_code {
            return Ok(());
        }
        info.status = status;
        info.exit_code = exit_code;
        info.foreground_cwd = None;
        info.harness = None;
        info.harness_status = None;
        info.project = None;
        // Daemon teardown ends executions, not the user's saved layout intent.
        // Only an ordinary pane exit or explicit pane termination closes projections.
        if status == PaneStatus::Exited && !closing {
            for workspace in &mut next.registry.workspaces {
                if workspace
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout_references(layout, id))
                {
                    workspace.layout = workspace
                        .layout
                        .take()
                        .and_then(|layout| remove_layout_pane(layout, id));
                    workspace.revision = next_revision(workspace.revision)?;
                }
            }
        }
        next.registry.revision = next_revision(next.registry.revision)?;
        validate_record(&next)?;
        commit(&self.inner, next, |_| {})?;
        // Keep the final in-memory scene readable until daemon shutdown; no pixels go to disk.
        agent_retirement?;
        Ok(())
    }
    pub fn shutdown(&self) -> Result<()> {
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
        self.stop_projects();
        let mut first_error = agent_shutdown.err();
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
    if let SpawnTarget::Bootstrap { workspace_id } = target {
        let mut suffix = 1;
        let name = loop {
            let name = if suffix == 1 {
                "Main".to_owned()
            } else {
                format!("Main {suffix}")
            };
            if next
                .registry
                .workspaces
                .iter()
                .all(|workspace| workspace.name != name)
            {
                break name;
            }
            suffix += 1;
        };
        next.registry.workspaces.push(Workspace {
            id: workspace_id.clone(),
            name,
            revision: 1,
            layout: None,
        });
    }
    if let Some(workspace_id) = &info.workspace_id {
        let workspace = next
            .registry
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.id == workspace_id)
            .context("unknown workspace")?;
        if let SpawnTarget::Reopen { pane_id, .. } = target {
            let layout = workspace
                .layout
                .as_mut()
                .context("workspace layout does not reference pane")?;
            replace_layout_pane(layout, pane_id, &info.id);
        } else {
            let leaf = LayoutNode::Leaf {
                pane_id: info.id.clone(),
            };
            workspace.layout = Some(match workspace.layout.take() {
                None => leaf,
                Some(first) => LayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 500,
                    first: Box::new(first),
                    second: Box::new(leaf),
                },
            });
        }
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
fn check_spawn_workspace(
    r: &RegistrySnapshot,
    id: Option<&str>,
    revision: Option<u64>,
) -> Result<()> {
    match (id, revision) {
        (Some(id), Some(revision)) => {
            let w = r
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .context("unknown workspace")?;
            ensure!(
                w.revision == revision,
                "stale workspace revision: current {}",
                w.revision
            );
        }
        (None, None) => {}
        _ => bail!("workspace spawn requires expected_revision; standalone spawn must omit it"),
    }
    Ok(())
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
    let mut ids = BTreeSet::new();
    for w in &r.workspaces {
        ensure!(
            Uuid::parse_str(&w.id).is_ok() && ids.insert(&w.id) && w.revision > 0,
            "invalid workspace identity/revision"
        );
        valid_name(&w.name)?;
        if let Some(node) = &w.layout {
            validate_layout(node, &r.panes)?;
        }
    }
    for pane in &r.panes {
        if let Some(workspace_id) = &pane.workspace_id {
            ensure!(
                r.workspaces.iter().any(|w| &w.id == workspace_id),
                "pane refers to unknown backend workspace"
            );
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
    Ok(Some(serde_json::from_slice(&bytes).context(
        "decode registry (never discard unreadable metadata)",
    )?))
}
fn persist(root: &Path, record: &Record) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    let durable = DurableRecord {
        version: record.version,
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
    state.record = next;
    publish(&mut state);
    inner.changed.notify_all();
    let _ = inner.project_wake.try_send(());
    Ok(state.record.registry.clone())
}

#[cfg(test)]
mod tests;
