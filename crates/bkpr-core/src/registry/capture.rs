use super::*;
use crate::{
    harness::{self, Launch, Peer},
    harness_process::{self as proc, Process},
};
use std::{collections::HashMap, sync::atomic::Ordering, thread};

impl Registry {
    /// Configure public shell context and private symlink wrappers before any pane starts.
    pub fn configure_harnesses(&self, socket: &Path, executable: &Path) -> Result<()> {
        {
            let state = self.inner.state.lock().unwrap();
            ensure!(
                state.live.is_empty() && state.spawning == 0 && !state.closing,
                "configure harnesses before first pane"
            );
        }
        let environment = harness::provision(&self.inner.root, socket, executable)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        let state = self.inner.state.lock().unwrap();
        ensure!(
            state.live.is_empty() && state.spawning == 0 && !state.closing,
            "pane started during harness configuration"
        );
        let mut configured = self.inner.spawn_environment.lock().unwrap();
        ensure!(
            configured.is_none(),
            "harness environment already configured"
        );
        *configured = Some(environment);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn request_authenticated(&self, request: Request, peer: &Peer) -> Result<Value> {
        self.request_authenticated_cancellable(request, peer, &|| true)
    }
    pub(crate) fn request_authenticated_cancellable(
        &self,
        request: Request,
        peer: &Peer,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported protocol version"
        );
        match request.operation {
            Operation::Agent {
                launch_id,
                token,
                request,
            } => {
                let launch = self
                    .inner
                    .captures
                    .lock()
                    .unwrap()
                    .lookup(&launch_id, &token)?;
                self.agent_request(launch, peer, request, is_connected)
            }
            Operation::HarnessBegin { kind } => self.begin_capture(peer, kind),
            Operation::HarnessEvent {
                launch_id,
                token,
                event,
            } => {
                self.inner.agents.check_health()?;
                let launch = self
                    .inner
                    .captures
                    .lock()
                    .unwrap()
                    .lookup(&launch_id, &token)?;
                harness::validate_callback(peer, &launch)?;
                self.capture_pane(&launch.grant.pane_id)?;
                if matches!(event, CaptureEvent::End) {
                    self.agent_retire(std::slice::from_ref(&launch_id))?;
                }
                self.agent_capture_event(&launch_id, &token, event)?;
                self.publish_capture_change(&launch)?;
                Ok(json!({"accepted":true}))
            }
            Operation::HarnessContext { launch_id, token } => {
                let launch = self
                    .inner
                    .captures
                    .lock()
                    .unwrap()
                    .lookup(&launch_id, &token)?;
                harness::validate_callback(peer, &launch)?;
                let (active, cwd) = foreground_context(&launch)?;
                let state = self.inner.state.lock().unwrap();
                ensure!(!state.closing, "daemon is shutting down");
                let pane = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|p| p.id == launch.grant.pane_id && p.status == PaneStatus::Running)
                    .context("launch pane is not running")?;
                let workspace = pane.workspace_id.as_ref().and_then(|id| {
                    state
                        .record
                        .registry
                        .workspaces
                        .iter()
                        .find(|w| &w.id == id)
                });
                let native = launch.managed.conversation.as_ref().map(|c| c.id.as_str());
                Ok(
                    json!({"launch_id":launch.grant.launch_id,"harness":launch.grant.kind.as_str(),"active":active,"session_id":native,"pane_id":pane.id,"workspace_id":pane.workspace_id,"workspace_name":workspace.map(|w| w.name.as_str()),"cwd":cwd.as_deref().map(bkpr_protocol::path_wire::WirePath)}),
                )
            }
            operation => self.request(Request::new(operation)),
        }
    }
    pub(super) fn capture_pane(&self, pane_id: &str) -> Result<Pane> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(!state.closing, "daemon is shutting down");
        ensure!(
            state
                .record
                .registry
                .panes
                .iter()
                .any(|p| p.id == pane_id && p.status == PaneStatus::Running),
            "launch pane is not running"
        );
        let pane = state
            .live
            .get(pane_id)
            .context("launch pane is unavailable")?;
        ensure!(
            !pane.shared.finished.load(Ordering::Acquire),
            "launch pane execution ended"
        );
        Ok(pane.clone())
    }
    fn begin_capture(&self, peer: &Peer, kind: HarnessKind) -> Result<Value> {
        self.inner.agents.check_health()?;
        ensure!(kind.is_managed(), "harness is recognition-only");
        let environment = self
            .inner
            .spawn_environment
            .lock()
            .unwrap()
            .clone()
            .context("managed harnesses are not configured")?;
        let caller = peer.current()?;
        ensure!(
            proc::is_launcher(caller.pid, &environment.executable, kind)?,
            "capture begin requires selected bkpr launcher"
        );
        let panes = self.running_panes();
        let mut selected = None;
        for (id, pane, root) in panes {
            if !proc::foreground(&caller, &root) {
                continue;
            }
            let Ok(chain) = harness::ancestry(&caller, &root) else {
                continue;
            };
            harness::reject_nested(&chain[1..], kind, true)?;
            ensure!(selected.is_none(), "ambiguous capture pane");
            selected = Some((id, pane, root));
        }
        let (id, pane, root) =
            selected.context("capture caller is not a live foreground pane descendant")?;
        let cwd = proc::cwd(caller.pid)?.context("capture working directory unavailable")?;
        // Validate existing live roots without holding the capability store over process I/O.
        let existing = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for launch in &existing {
            if launch.grant.pane_id == id && harness::ancestry(&caller, &launch.root).is_ok() {
                bail!("nested managed launch");
            }
        }
        self.capture_pane(&id)?;
        let grant =
            self.inner
                .captures
                .lock()
                .unwrap()
                .insert(caller.clone(), root, id.clone(), kind)?;
        let managed = self
            .inner
            .captures
            .lock()
            .unwrap()
            .lookup(&grant.launch_id, &grant.token)?
            .managed;
        let launch = self
            .inner
            .captures
            .lock()
            .unwrap()
            .lookup(&grant.launch_id, &grant.token)?;
        if let Err(error) = self.agent_register(&launch, &cwd) {
            self.inner
                .captures
                .lock()
                .unwrap()
                .launches
                .remove(&grant.launch_id);
            return Err(error);
        }
        if let Err(error) = self.publish_facts(
            &id,
            &pane,
            Some(cwd.clone()),
            Some(HarnessInfo {
                kind,
                pid: caller.pid as u32,
                birth: caller.identity,
                cwd,
                managed: Some(managed),
            }),
            None,
        ) {
            self.inner
                .captures
                .lock()
                .unwrap()
                .launches
                .remove(&grant.launch_id);
            let _ = self.agent_retire(std::slice::from_ref(&grant.launch_id));
            return Err(error);
        }
        Ok(serde_json::to_value(grant)?)
    }
    fn publish_capture_change(&self, original: &Launch) -> Result<()> {
        let pane = self.capture_pane(&original.grant.pane_id)?;
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut state = self.inner.state.lock().unwrap();
        ensure!(
            !state.closing && !pane.shared.finished.load(Ordering::Acquire),
            "pane execution ended"
        );
        let captures = self.inner.captures.lock().unwrap();
        let info = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|p| p.id == original.grant.pane_id && p.status == PaneStatus::Running)
            .context("capture pane ended")?;
        let Some(visible) = &info.harness else {
            return Ok(());
        };
        if visible
            .managed
            .as_ref()
            .is_none_or(|m| m.launch_id != original.grant.launch_id)
        {
            return Ok(());
        }
        let managed = captures
            .launches
            .get(&original.grant.launch_id)
            .map(|l| l.managed.clone());
        if visible.managed == managed {
            return Ok(());
        }
        let ended = managed.is_none();
        let supervisor =
            visible.pid == original.root.pid as u32 && visible.birth == original.root.identity;
        if ended && supervisor {
            info.harness = None;
        } else {
            info.harness.as_mut().unwrap().managed = managed;
        }
        if ended {
            info.harness_status = None;
            let mut published = pane.shared.published.lock().unwrap();
            published.status.clear();
            published.program_identity = info.harness.as_ref().map(|h| ProcessIdentity {
                pid: h.pid,
                birth: h.birth.clone(),
            });
            pane.shared.status_reset.store(true, Ordering::Release);
            pane.shared.changes.notify();
        }
        state.record.registry.revision = next_revision(state.record.registry.revision)?;
        self.inner.changed.notify_all();
        Ok(())
    }
    fn running_panes(&self) -> Vec<(String, Pane, Process)> {
        let panes = {
            let state = self.inner.state.lock().unwrap();
            if state.closing {
                return Vec::new();
            }
            state
                .live
                .iter()
                .map(|(id, pane)| (id.clone(), pane.clone()))
                .collect::<Vec<_>>()
        };
        panes
            .into_iter()
            .filter_map(|(id, pane)| {
                if pane.shared.finished.load(Ordering::Acquire) {
                    return None;
                }
                let root = pane.shared.root.lock().unwrap().clone()?;
                Some((id, pane, root))
            })
            .collect()
    }
    fn publish_facts(
        &self,
        id: &str,
        pane: &Pane,
        cwd: Option<PathBuf>,
        mut harness: Option<HarnessInfo>,
        expected_capture_version: Option<u64>,
    ) -> Result<()> {
        let _transaction = self.inner.transaction.lock().unwrap();
        let mut state = self.inner.state.lock().unwrap();
        let captures = self.inner.captures.lock().unwrap();
        if expected_capture_version.is_some_and(|expected| captures.version != expected) {
            return Ok(());
        }
        ensure!(
            !state.closing && !pane.shared.finished.load(Ordering::Acquire),
            "pane execution ended"
        );
        let info = state
            .record
            .registry
            .panes
            .iter_mut()
            .find(|p| p.id == id && p.status == PaneStatus::Running)
            .context("pane execution ended")?;
        // Observation runs outside locks. Reconcile its cloned binding with push events:
        // a delayed scan must neither resurrect End nor overwrite a newer generation.
        if let Some(observed) = &mut harness
            && let Some(managed) = &observed.managed
        {
            let Some(live) = captures.launches.get(&managed.launch_id) else {
                return Ok(());
            };
            observed.managed = Some(live.managed.clone());
        }
        if info.foreground_cwd == cwd && info.harness == harness {
            return Ok(());
        }
        let identity = harness.as_ref().map(|h| ProcessIdentity {
            pid: h.pid,
            birth: h.birth.clone(),
        });
        let old_launch = info
            .harness
            .as_ref()
            .and_then(|h| h.managed.as_ref())
            .map(|m| m.launch_id.as_str());
        let new_launch = harness
            .as_ref()
            .and_then(|h| h.managed.as_ref())
            .map(|m| m.launch_id.as_str());
        let initial_child = old_launch.zip(new_launch).is_some_and(|(old_id, new_id)| {
            old_id == new_id
                && captures.launches.get(new_id).is_some_and(|launch| {
                    info.harness.as_ref().is_some_and(|old| {
                        old.pid == launch.root.pid as u32 && old.birth == launch.root.identity
                    }) && harness
                        .as_ref()
                        .is_some_and(|new| new.kind == launch.grant.kind)
                })
        });
        {
            let mut published = pane.shared.published.lock().unwrap();
            if published.program_identity != identity || old_launch != new_launch {
                if !initial_child {
                    published.status.clear();
                    pane.shared.status_reset.store(true, Ordering::Release);
                }
                published.program_identity = identity;
                pane.shared.changes.notify();
            }
            let summary = harness_summary(harness.as_ref(), &published);
            if info.harness_status.as_ref() != summary {
                info.harness_status = summary.cloned();
            }
        }
        info.foreground_cwd = cwd;
        info.harness = harness;
        state.record.registry.revision = next_revision(state.record.registry.revision)?;
        self.inner.changed.notify_all();
        Ok(())
    }
    pub(super) fn start_observer(&self) -> Result<()> {
        if self.inner.observer_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let observation = match proc::ObservationLease::acquire() {
            Ok(lease) => lease,
            Err(error) => {
                self.inner.observer_started.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let weak = Arc::downgrade(&self.inner);
        let status_events = self.inner.status_events.clone();
        let spawned = thread::Builder::new()
            .name("bkpr-processes".into())
            .spawn(move || {
                let _observation = observation;
                let mut next_process = std::time::Instant::now();
                loop {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    if Arc::strong_count(&inner) == 1 {
                        break;
                    }
                    let registry = Registry {
                        inner,
                        fresh: false,
                    };
                    {
                        let state = registry.inner.state.lock().unwrap();
                        if state.closing {
                            break;
                        }
                        if state.spawning == 0
                            && !state
                                .live
                                .values()
                                .any(|p| !p.shared.finished.load(Ordering::Acquire))
                        {
                            // Commit and this decision share state: a later spawn observes false
                            // and starts a new observer. No polling deadline while there is no work.
                            registry
                                .inner
                                .observer_started
                                .store(false, Ordering::Release);
                            break;
                        }
                    }
                    if std::time::Instant::now() >= next_process {
                        registry.observe_processes();
                        next_process = std::time::Instant::now() + Duration::from_millis(200);
                    }
                    registry.refresh_status_summaries();
                    drop(registry);
                    // OSC invalidations wake metadata-only work, never another process scan.
                    let _ = status_events.recv_timeout(
                        next_process.saturating_duration_since(std::time::Instant::now()),
                    );
                }
            });
        if let Err(error) = spawned {
            self.inner.observer_started.store(false, Ordering::Release);
            return Err(error.into());
        }
        Ok(())
    }
    fn refresh_status_summaries(&self) {
        let panes = {
            let state = self.inner.state.lock().unwrap();
            if state.closing {
                return;
            }
            state
                .live
                .iter()
                .filter(|(_, pane)| pane.shared.status_dirty.swap(false, Ordering::AcqRel))
                .map(|(id, pane)| (id.clone(), pane.clone()))
                .collect::<Vec<_>>()
        };
        for (id, pane) in panes {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            if state.closing {
                return;
            }
            let registry = &mut state.record.registry;
            let Some(info) = registry
                .panes
                .iter_mut()
                .find(|info| info.id == id && info.status == PaneStatus::Running)
            else {
                continue;
            };
            let published = pane.shared.published.lock().unwrap();
            let summary = harness_summary(info.harness.as_ref(), &published);
            if info.harness_status.as_ref() != summary {
                let Some(revision) = registry.revision.checked_add(1) else {
                    continue;
                };
                info.harness_status = summary.cloned();
                registry.revision = revision;
                self.inner.changed.notify_all();
            }
        }
    }
    fn observe_processes(&self) {
        let panes = self.running_panes();
        if panes.is_empty() {
            return;
        }
        let (launches, mut version) = {
            let captures = self.inner.captures.lock().unwrap();
            (
                captures.launches.values().cloned().collect::<Vec<_>>(),
                captures.version,
            )
        };
        // Lifecycle retirement requires a sample admitted after the launch list.
        // Cache-only absence is not evidence that a newly authenticated root died.
        let table = match proc::fresh_snapshot() {
            Ok(table) => table,
            Err(_) => {
                for (id, pane, _) in panes {
                    let _ = self.publish_facts(&id, &pane, None, None, Some(version));
                }
                return;
            }
        };
        let mut valid_launches = Vec::new();
        let mut stale = Vec::new();
        for launch in launches {
            let valid = table.get(&launch.root.pid).is_some_and(|p| {
                p.identity == launch.root.identity
                    && p.parent == launch.root.parent
                    && p.tty == launch.root.tty
                    && table
                        .get(&launch.pane_root.pid)
                        .is_some_and(|r| r.identity == launch.pane_root.identity)
                    && proc::lineage(p, &launch.pane_root, &table).is_some()
            });
            if valid {
                valid_launches.push(launch);
            } else {
                stale.push(launch.grant.launch_id);
            }
        }
        if !stale.is_empty() {
            if self.agent_retire(&stale).is_err() {
                // The durable service is failed closed. Volatile capture authority must
                // still disappear even when exit observation cannot be committed.
            }
            let mut captures = self.inner.captures.lock().unwrap();
            if captures.version != version {
                return;
            }
            for id in stale {
                captures.launches.remove(&id);
            }
            captures.version = captures.version.wrapping_add(1);
            version = captures.version;
        }
        let mut foreground: HashMap<u64, Vec<&Process>> = HashMap::new();
        for p in table
            .values()
            .filter(|p| !p.stopped && p.tty != 0 && p.group == p.foreground)
        {
            foreground.entry(p.tty).or_default().push(p);
        }
        let mut kinds = HashMap::new();
        for (id, pane, root) in panes {
            let Some(current_root) = table
                .get(&root.pid)
                .filter(|p| p.identity == root.identity && p.tty == root.tty)
            else {
                let _ = self.publish_facts(&id, &pane, None, None, Some(version));
                continue;
            };
            let mut selected: Option<(usize, &Process, HarnessKind)> = None;
            let mut generic: Option<(usize, &Process)> = None;
            for candidate in foreground.get(&current_root.tty).into_iter().flatten() {
                let Some(chain) = proc::lineage(candidate, current_root, &table) else {
                    continue;
                };
                let depth = chain.len();
                if generic
                    .as_ref()
                    .is_none_or(|(d, p)| (depth, candidate.pid) < (*d, p.pid))
                {
                    generic = Some((depth, candidate));
                }
                let kind = *kinds
                    .entry(candidate.pid)
                    .or_insert_with(|| proc::kind(candidate.pid).ok().flatten());
                if let Some(kind) = kind
                    && selected
                        .as_ref()
                        .is_none_or(|(d, p, _)| (depth, candidate.pid) < (*d, p.pid))
                {
                    selected = Some((depth, candidate, kind));
                }
            }
            let mut managed = None;
            if let Some((_, process, kind)) = selected {
                managed = valid_launches
                    .iter()
                    .find(|l| {
                        l.grant.pane_id == id
                            && l.grant.kind == kind
                            && proc::lineage(process, &l.root, &table).is_some_and(|chain| {
                                // A foreground nested job must not borrow an outer/background
                                // harness's exact conversation merely because it is a descendant.
                                chain
                                    .iter()
                                    .skip(1)
                                    .all(|ancestor| matches!(proc::kind(ancestor.pid), Ok(None)))
                            })
                    })
                    .cloned();
            } else if let Some(launch) = valid_launches.iter().find(|l| {
                l.grant.pane_id == id
                    && table
                        .get(&l.root.pid)
                        .is_some_and(|p| proc::foreground(p, current_root))
            }) {
                selected = table
                    .get(&launch.root.pid)
                    .map(|p| (0, p, launch.grant.kind));
                managed = Some(launch.clone());
            }
            let process = selected
                .as_ref()
                .map(|(_, p, _)| *p)
                .or_else(|| generic.map(|(_, p)| p));
            let Some(process) = process else {
                let _ = self.publish_facts(&id, &pane, None, None, Some(version));
                continue;
            };
            let cwd = proc::cwd(process.pid).ok().flatten();
            let live = proc::lineage(process, current_root, &table)
                .is_some_and(|chain| proc::live(&chain));
            if !live {
                let _ = self.publish_facts(&id, &pane, None, None, Some(version));
                continue;
            }
            let info = selected.and_then(|(_, p, kind)| {
                cwd.clone().map(|cwd| HarnessInfo {
                    kind,
                    pid: p.pid as u32,
                    birth: p.identity.clone(),
                    cwd,
                    managed: managed.map(|l| l.managed),
                })
            });
            let _ = self.publish_facts(&id, &pane, cwd, info, Some(version));
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

fn foreground_context(launch: &Launch) -> Result<(bool, Option<PathBuf>)> {
    let table = proc::fresh_snapshot()?;
    foreground_context_in(launch, &table)
}
pub(super) fn foreground_context_in(
    launch: &Launch,
    table: &HashMap<i32, Process>,
) -> Result<(bool, Option<PathBuf>)> {
    let pane_root = table
        .get(&launch.pane_root.pid)
        .filter(|p| p.identity == launch.pane_root.identity)
        .context("pane root exited")?;
    let mut selected: Option<(usize, &Process, HarnessKind)> = None;
    for candidate in table.values().filter(|p| proc::foreground(p, pane_root)) {
        let Some(chain) = proc::lineage(candidate, pane_root, table) else {
            continue;
        };
        let Some(kind) = proc::kind(candidate.pid)? else {
            continue;
        };
        if selected
            .as_ref()
            .is_none_or(|(depth, p, _)| (chain.len(), candidate.pid) < (*depth, p.pid))
        {
            selected = Some((chain.len(), candidate, kind));
        }
    }
    let process = if let Some((_, candidate, kind)) = selected {
        let chain = proc::lineage(candidate, &launch.root, table);
        if kind != launch.grant.kind
            || !chain.as_ref().is_some_and(|chain| {
                chain
                    .iter()
                    .skip(1)
                    .all(|p| matches!(proc::kind(p.pid), Ok(None)))
            })
        {
            return Ok((false, None));
        }
        candidate
    } else {
        let root = table
            .get(&launch.root.pid)
            .filter(|p| p.identity == launch.root.identity)
            .context("launch root exited")?;
        if !proc::foreground(root, pane_root) {
            return Ok((false, None));
        }
        root
    };
    let cwd = proc::cwd(process.pid)?;
    ensure!(
        proc::lineage(process, pane_root, table).is_some_and(|chain| proc::live(&chain)),
        "foreground context changed during observation"
    );
    Ok((true, cwd))
}

fn harness_summary<'a>(
    harness: Option<&HarnessInfo>,
    published: &'a crate::Published,
) -> Option<&'a ProgramStatus> {
    let harness = harness?;
    if published.lifecycle != "running"
        || !published
            .program_identity
            .as_ref()
            .is_some_and(|identity| identity.pid == harness.pid && identity.birth == harness.birth)
    {
        return None;
    }
    published
        .status
        .iter()
        .filter(|record| match record.app.as_deref() {
            Some(app) => HarnessKind::from_command(app) == Some(harness.kind),
            None => record.id.is_empty(),
        })
        .filter(|record| {
            matches!(
                record.state.as_str(),
                "error" | "blocked" | "working" | "done" | "idle"
            )
        })
        .max_by_key(|record| match record.state.as_str() {
            "error" => 3,
            "blocked" => 2,
            "working" => 1,
            _ => 0,
        })
}
