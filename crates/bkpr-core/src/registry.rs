//! Backend-owned workspace intent. Persisted metadata is not PTY recovery.
//! Atomic tempfile/fsync/rename follows main's recovery.rs, without its session/KDL assumptions.
use crate::{Pane, SpawnSpec};
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
    state: Mutex<State>,
    // Serialize durable writers, never pane I/O. Lock order: transaction, then state.
    transaction: Mutex<()>,
    view_budget: Arc<crate::authority::ViewBudget>,
    changed: Condvar,
    _lease: Lease,
    #[cfg(test)]
    spawn_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
    #[cfg(test)]
    persist_barrier: Mutex<Option<Arc<SpawnBarrier>>>,
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
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
    fresh: bool,
}

impl Registry {
    pub fn open(root: &Path) -> Result<Self> {
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
        Ok(Self {
            inner: Arc::new(Inner {
                root: root.to_owned(),
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
                changed: Condvar::new(),
                _lease: lease,
                #[cfg(test)]
                spawn_barrier: Mutex::new(None),
                #[cfg(test)]
                persist_barrier: Mutex::new(None),
            }),
            fresh,
        })
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
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        match request.operation {
            Operation::Inspect => Ok(json!({"registry":self.snapshot()?})),
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
            } => self.spawn(workspace_id, expected_revision, spec),
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
                let result = pane.request(request)?;
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
    fn spawn(
        &self,
        workspace_id: Option<String>,
        expected_revision: Option<u64>,
        spec: PaneSpawnSpec,
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
        let id = Uuid::new_v4().to_string();
        let info = PaneInfo {
            id: id.clone(),
            workspace_id: workspace_id.clone(),
            title,
            status: PaneStatus::Running,
            exit_code: None,
            cwd: spec.cwd.clone(),
            home: spec.home.clone(),
            isolated_environment: spec.isolated_environment,
        };
        {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(!state.closing, "daemon is shutting down");
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
            check_spawn_workspace(
                &state.record.registry,
                workspace_id.as_deref(),
                expected_revision,
            )?;
            if let Some(workspace_id) = &workspace_id {
                workspace_available(&state, workspace_id)?;
            }
            // Validate the exact appended layout and revisions before argv can execute.
            append_pane_record(state.record.clone(), info.clone())?;
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
        );
        let pane = spawned?;
        let result = (|| -> Result<RegistrySnapshot> {
            let _transaction = self.inner.transaction.lock().unwrap();
            let current = {
                let state = self.inner.state.lock().unwrap();
                healthy(&state)?;
                ensure!(!state.closing, "daemon is shutting down");
                state.record.clone()
            };
            let next = append_pane_record(current, info)?;
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
        Ok(json!({"registry":registry,"pane_id":id}))
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
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut next = {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            state.record.clone()
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
        if status == PaneStatus::Exited {
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
        Ok(())
    }
    pub fn shutdown(&self) -> Result<()> {
        let panes = {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            state.closing = true;
            self.inner.changed.notify_all();
            state.live.values().cloned().collect::<Vec<_>>()
        };
        let mut first_error = None;
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
fn append_pane_record(mut next: Record, info: PaneInfo) -> Result<Record> {
    if let Some(workspace_id) = &info.workspace_id {
        let workspace = next
            .registry
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.id == workspace_id)
            .context("unknown workspace")?;
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
        workspace.revision = next_revision(workspace.revision)?;
    }
    next.registry.panes.push(info);
    next.registry.revision = next_revision(next.registry.revision)?;
    validate_record(&next)?;
    Ok(next)
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
    serde_json::to_writer(temporary.as_file_mut(), record)?;
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
    Ok(state.record.registry.clone())
}

#[cfg(test)]
mod tests;
