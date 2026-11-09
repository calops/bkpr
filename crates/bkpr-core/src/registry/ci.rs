//! Native CI coordinator. Git discovery and gh children run only on this worker;
//! neither the local Git worker nor registry/input/render paths wait for providers.
mod discovery;
mod monitor;
use super::*;
use bkpr_protocol::{CiRefreshTarget, CiSnapshot, ProjectState};
use crossbeam_channel::Receiver;
use monitor::{CiSource, Monitor};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    cwd: PathBuf,
    root: PathBuf,
    head: String,
    branch: Option<String>,
    metadata: u64,
}
struct Binding {
    identity: Identity,
    source: Option<CiSource>,
    error: Option<String>,
    retry: Option<Instant>,
    project_error: bool,
    live: bool,
    projected_metadata: Option<u64>,
    published_snapshot: Option<CiSnapshot>,
    published_schedule: Option<(Option<Instant>, bool)>,
}
struct Worker {
    registry: Weak<Inner>,
    cancel: Arc<AtomicBool>,
    wake: Receiver<()>,
    monitor: Monitor,
    bindings: BTreeMap<String, Binding>,
    next_source_generation: u64,
    published_version: u64,
    auth_dirty: Arc<AtomicBool>,
    auth_watcher: Option<RecommendedWatcher>,
    auth_watched: BTreeSet<PathBuf>,
    auth_retry: Option<Instant>,
    auth_config: Option<PathBuf>,
    auth_directories: BTreeSet<PathBuf>,
    auth_events: Receiver<notify::Result<notify::Event>>,
    auth_sender: crossbeam_channel::Sender<notify::Result<notify::Event>>,
    auth_overflow: Arc<AtomicBool>,
    notify_wake: crossbeam_channel::Sender<()>,
}

impl Registry {
    pub(super) fn start_ci(&self, wake: Receiver<()>) -> Result<()> {
        let registry = Arc::downgrade(&self.inner);
        let cancel = Arc::clone(&self.inner.ci_cancel);
        let notify_wake = self.inner.ci_wake.clone();
        let (auth_sender, auth_events) = crossbeam_channel::bounded(256);
        let worker = thread::Builder::new()
            .name("bkpr-ci".into())
            .spawn(move || {
                Worker {
                    registry,
                    cancel,
                    wake,
                    monitor: Monitor::new(),
                    bindings: BTreeMap::new(),
                    next_source_generation: 0,
                    published_version: 0,
                    auth_dirty: Arc::new(AtomicBool::new(true)),
                    auth_watcher: None,
                    auth_watched: BTreeSet::new(),
                    auth_retry: None,
                    notify_wake,
                    auth_config: auth_directory(),
                    auth_directories: BTreeSet::new(),
                    auth_events,
                    auth_sender,
                    auth_overflow: Arc::new(AtomicBool::new(true)),
                }
                .run();
            })?;
        *self.inner.ci_worker.lock().unwrap() = Some(worker);
        Ok(())
    }
    pub(super) fn stop_ci(&self) {
        self.inner.ci_cancel.store(true, Ordering::Release);
        let _ = self.inner.ci_wake.try_send(());
        if let Some(worker) = self.inner.ci_worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
    /// Admission is exact and cheap. The authenticated dispatch already owns the
    /// transaction lock; the provider request is only queued, never performed here.
    pub fn refresh_ci(&self, target: &CiRefreshTarget) -> Result<RegistrySnapshot> {
        let state = self.inner.state.lock().unwrap();
        ensure!(!state.closing, "registry is shutting down");
        let pane = state
            .record
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == target.pane_id && pane.status == PaneStatus::Running)
            .context("CI pane is not running")?;
        let project = pane.project.as_ref().context("CI project is unavailable")?;
        let ci = project.ci.as_ref().context("CI source is unavailable")?;
        ensure!(
            project.cwd == target.cwd
                && project::effective_cwd(pane) == target.cwd
                && project.repository.as_ref().is_some_and(
                    |repo| repo.head_sha.as_deref() == Some(target.local_head_sha.as_str())
                )
                && ci.repository == target.repository
                && ci.head_sha == target.head_sha
                && ci.local_head_sha == target.local_head_sha
                && ci.checked_ref == target.checked_ref,
            "CI refresh target is no longer current"
        );
        let mut requests = self.inner.ci_refresh.lock().unwrap();
        ensure!(
            requests.len() < 256 || requests.contains(target),
            "CI refresh queue is full"
        );
        if !requests.contains(target) {
            requests.push(target.clone());
        }
        drop(requests);
        let _ = self.inner.ci_wake.try_send(());
        Ok(registry_snapshot(&state))
    }
}

impl Worker {
    fn run(&mut self) {
        while !self.cancel.load(Ordering::Acquire) {
            let Some(inner) = self.registry.upgrade() else {
                break;
            };
            let retry_auth = self.auth_retry.is_some_and(|due| due <= Instant::now());
            if self.auth_dirty.swap(false, Ordering::AcqRel) || retry_auth {
                let changed = self.auth_events_changed();
                // A changed-and-restored file still retires the old account.
                // gh alone reads credentials; no token contents enter this cache.
                if changed || retry_auth {
                    self.watch_auth();
                    self.retire_auth(&inner);
                }
            }
            if !self.sync_inputs(&inner) {
                break;
            }
            for mut request in std::mem::take(&mut *inner.ci_refresh.lock().unwrap()) {
                if let Some(binding) = self.bindings.get(&request.pane_id)
                    && binding.identity.cwd == request.cwd
                    && binding.identity.head == request.local_head_sha
                    && binding.error.is_none()
                    && let Some(source) = &binding.source
                {
                    request.cwd.clone_from(&source.cwd);
                    self.monitor.refresh(&request);
                }
            }
            // Authentication invalidation retires the whole auth context. Generic
            // status/layout changes do not invalidate independently keyed sources.
            if self.auth_dirty.load(Ordering::Acquire) {
                continue;
            }
            self.monitor.advance();
            self.publish(&inner);
            let provider = self.monitor.timeout();
            let retry = self
                .bindings
                .values()
                .filter_map(|binding| binding.retry)
                .chain(self.auth_retry)
                .min();
            let deadline = (provider >= 0)
                .then(|| Instant::now() + Duration::from_millis(provider as u64))
                .into_iter()
                .chain(retry)
                .min();
            drop(inner);
            if let Some(deadline) = deadline {
                let _ = self
                    .wake
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()));
            } else if self.wake.recv().is_err() {
                break;
            }
        }
    }
    fn sync_inputs(&mut self, inner: &Inner) -> bool {
        // Borrow-only fast path for capture/status and ordinary Git changes.
        // State -> source-inputs is also the producer lock order.
        {
            let state = inner.state.lock().unwrap();
            if state.closing {
                return false;
            }
            let mut metadata = inner.ci_inputs.lock().unwrap();
            let mut count = 0;
            let stable = state
                .record
                .registry
                .panes
                .iter()
                .filter_map(|pane| {
                    if pane.status != PaneStatus::Running {
                        return None;
                    }
                    let project = pane.project.as_ref()?;
                    let repository = project.repository.as_ref()?;
                    if project.state == ProjectState::NotRepository
                        || project.cwd != project::effective_cwd(pane)
                        || repository.head_sha.is_none()
                    {
                        return None;
                    }
                    Some((pane, project, repository))
                })
                .all(|(pane, project, repository)| {
                    count += 1;
                    self.bindings.get(&pane.id).is_some_and(|binding| {
                        binding.identity.cwd == project.cwd
                            && binding.identity.root == repository.root
                            && repository.head_sha.as_deref()
                                == Some(binding.identity.head.as_str())
                            && binding.identity.branch == repository.branch
                            && binding.identity.metadata
                                == metadata.get(&project.cwd).copied().unwrap_or(0)
                            && binding.project_error == project.error.is_some()
                            && binding.retry.is_none_or(|due| due > Instant::now())
                    })
                });
            if stable && count == self.bindings.len() {
                metadata.retain(|cwd, _| {
                    state.record.registry.panes.iter().any(|pane| {
                        pane.status == PaneStatus::Running && project::effective_cwd(pane) == cwd
                    })
                });
                return true;
            }
        }
        let metadata = inner.ci_inputs.lock().unwrap().clone();
        let desired: BTreeMap<_, _> = {
            let state = inner.state.lock().unwrap();
            if state.closing {
                return false;
            }
            state
                .record
                .registry
                .panes
                .iter()
                .filter(|pane| pane.status == PaneStatus::Running)
                .filter_map(|pane| {
                    let project = pane.project.as_ref()?;
                    let repository = project.repository.as_ref()?;
                    if project.state == ProjectState::NotRepository
                        || project.cwd != project::effective_cwd(pane)
                    {
                        return None;
                    }
                    let cwd = project.cwd.clone();
                    Some((
                        pane.id.clone(),
                        (
                            Identity {
                                metadata: metadata.get(&cwd).copied().unwrap_or(0),
                                cwd,
                                root: repository.root.clone(),
                                head: repository.head_sha.clone()?,
                                branch: repository.branch.clone(),
                            },
                            project.error.is_some(),
                        ),
                    ))
                })
                .collect()
        };
        for (id, binding) in &mut self.bindings {
            binding.live = desired.get(id).is_some_and(|(identity, unavailable)| {
                binding.identity == *identity && binding.project_error == *unavailable
            });
        }
        if self.bindings.values().any(|binding| !binding.live) {
            self.monitor.cancel_pending();
        }
        {
            // Retire the pending source lease before forgetting its CWD epoch.
            let state = inner.state.lock().unwrap();
            inner.ci_inputs.lock().unwrap().retain(|cwd, _| {
                state.record.registry.panes.iter().any(|pane| {
                    pane.status == PaneStatus::Running && project::effective_cwd(pane) == cwd
                })
            });
        }
        self.bindings.retain(|id, _| desired.contains_key(id));
        // One source lease per exact active identity, not per subscribing pane.
        // New consumers join the existing request/cache without rediscovery.
        let mut shared: BTreeMap<_, _> = self
            .bindings
            .iter()
            .filter_map(|(id, binding)| {
                let (identity, unavailable) = desired.get(id)?;
                (binding.identity == *identity
                    && binding.project_error == *unavailable
                    && binding.retry.is_none_or(|due| due > Instant::now()))
                .then(|| (identity.clone(), id.clone()))
            })
            .collect();
        // Cold fairness: one bounded local discovery per turn, then give existing
        // provider work a turn. The wake below keeps draining cold identities.
        let mut discovered = false;
        let mut restores = Vec::new();
        for (id, (identity, project_error)) in desired {
            if self.bindings.get(&id).is_some_and(|old| {
                old.identity == identity
                    && old.project_error == project_error
                    && old.retry.is_none_or(|due| due > Instant::now())
            }) {
                continue;
            }
            if let Some(existing) = shared
                .get(&identity)
                .and_then(|id| self.bindings.get(id))
                .filter(|binding| binding.project_error == project_error)
            {
                let binding = Binding {
                    identity: identity.clone(),
                    source: existing.source.clone(),
                    error: existing.error.clone(),
                    retry: existing.retry,
                    project_error,
                    live: true,
                    projected_metadata: None,
                    published_snapshot: None,
                    published_schedule: None,
                };
                self.bindings.insert(id, binding);
                continue;
            }
            if discovered {
                let _ = inner.ci_wake.try_send(());
                continue;
            }
            discovered = true;
            let previous = self
                .bindings
                .get(&id)
                .and_then(|binding| binding.source.clone());
            let recovering = self
                .bindings
                .get(&id)
                .is_some_and(|binding| binding.error.is_some());
            let retired = self
                .bindings
                .get(&id)
                .is_some_and(|binding| binding.identity.metadata != identity.metadata);
            let same_location = self.bindings.get(&id).is_some_and(|binding| {
                binding.identity.cwd == identity.cwd && binding.identity.root == identity.root
            });
            let schedule = self
                .bindings
                .get(&id)
                .and_then(|binding| binding.published_schedule);
            let published = self
                .bindings
                .get(&id)
                .and_then(|binding| binding.published_snapshot.clone());
            let result = if project_error {
                Err(anyhow::anyhow!("local project source is unavailable"))
            } else {
                discovery::discover(&identity.cwd, identity.branch.as_deref(), &self.cancel)
            };
            let (source, error, retry) = match result {
                Ok(Some(mut source))
                    if source.sha == identity.head && source.branch == identity.branch =>
                {
                    source.generation = previous.as_ref().map_or(0, |old| old.generation);
                    let equivalent = previous.as_ref() == Some(&source);
                    if !equivalent || retired {
                        self.next_source_generation = self.next_source_generation.wrapping_add(1);
                        source.generation = self.next_source_generation;
                    }
                    if equivalent
                        && retired
                        && let Some(published) = published
                    {
                        restores.push((source.clone(), published, schedule));
                    }
                    (Some(source), None, None)
                }
                Ok(None) => (None, None, None),
                Ok(Some(_)) => (
                    None,
                    Some("local CI source changed during discovery".into()),
                    Some(Instant::now() + Duration::from_secs(1)),
                ),
                Err(_) => {
                    let mut source = previous.filter(|old| {
                        same_location && old.sha == identity.head && old.branch == identity.branch
                    });
                    if retired && let Some(source) = &mut source {
                        self.next_source_generation = self.next_source_generation.wrapping_add(1);
                        source.generation = self.next_source_generation;
                        if let Some(published) = published {
                            restores.push((source.clone(), published, schedule));
                        }
                    }
                    (
                        source,
                        Some("local CI source discovery unavailable".into()),
                        Some(Instant::now() + Duration::from_secs(60)),
                    )
                }
            };
            if recovering
                && error.is_none()
                && let Some(source) = &source
            {
                self.monitor.resume(source);
            }
            let binding = Binding {
                identity,
                source,
                error,
                retry,
                project_error,
                live: true,
                projected_metadata: None,
                published_snapshot: None,
                published_schedule: None,
            };
            shared.insert(binding.identity.clone(), id.clone());
            self.bindings.insert(id, binding);
        }
        self.monitor.sync(
            self.bindings
                .values()
                .filter(|binding| binding.live)
                .filter_map(|binding| binding.source.clone()),
        );
        for (source, snapshot, schedule) in restores {
            self.monitor.restore_published(&source, snapshot, schedule);
        }
        for binding in self.bindings.values().filter(|binding| binding.live) {
            if let (Some(source), Some(error)) = (&binding.source, &binding.error) {
                self.monitor.source_unavailable(source, error);
            }
        }
        self.watch_auth();
        true
    }
    fn publish(&mut self, inner: &Inner) {
        let version = self.monitor.version();
        if self.published_version == version
            && self
                .bindings
                .values()
                .filter(|binding| binding.live)
                .all(|binding| binding.projected_metadata == Some(binding.identity.metadata))
        {
            return;
        }
        // Select changed provider projections before touching registry state.
        // Status-only wakes neither clone checks nor rebuild local Git facts.
        let candidates: BTreeMap<_, _> = self
            .bindings
            .iter()
            .filter(|(_, binding)| binding.live)
            .filter_map(|(id, binding)| {
                let snapshot = binding.source.as_ref().and_then(|source| {
                    self.monitor
                        .snapshot_ref(source)
                        .filter(|snapshot| source.matches_snapshot(snapshot))
                });
                (snapshot != binding.published_snapshot.as_ref()
                    || binding.projected_metadata != Some(binding.identity.metadata))
                .then(|| (id.clone(), (binding.identity.clone(), snapshot.cloned())))
            })
            .collect();
        let facts: BTreeMap<_, _> = {
            let state = inner.state.lock().unwrap();
            state
                .record
                .registry
                .panes
                .iter()
                .filter_map(|pane| {
                    let (identity, _) = candidates.get(&pane.id)?;
                    if pane.status != PaneStatus::Running
                        || project::effective_cwd(pane) != identity.cwd
                    {
                        return None;
                    }
                    let project = pane.project.as_ref()?;
                    let repository = project.repository.as_ref()?;
                    if repository.root != identity.root
                        || repository.head_sha.as_deref() != Some(identity.head.as_str())
                        || repository.branch != identity.branch
                    {
                        return None;
                    }
                    let old_base = project
                        .ci
                        .as_ref()
                        .and_then(|ci| ci.change_request.as_ref())
                        .and_then(|request| request.base.clone());
                    Some((pane.id.clone(), (repository.clone(), old_base)))
                })
                .collect()
        };
        let mut updates = Vec::new();
        for (id, (identity, snapshot)) in candidates {
            let Some((repository, old_base)) = facts.get(&id) else {
                continue;
            };
            let new_base = snapshot
                .as_ref()
                .and_then(|ci| ci.change_request.as_ref())
                .and_then(|request| request.base.as_ref());
            let metadata_changed = self.bindings[&id].projected_metadata != Some(identity.metadata);
            // Only a changed PR base or targeted Git metadata reruns this diff.
            let base = if old_base.as_ref() != new_base || (new_base.is_some() && metadata_changed)
            {
                Some(project::ci_base(
                    &identity.cwd,
                    repository,
                    new_base,
                    &self.cancel,
                ))
            } else {
                None
            };
            updates.push((id, identity, snapshot, base));
        }
        let _transaction = inner.transaction.lock().unwrap();
        let mut state = inner.state.lock().unwrap();
        if state.closing || self.auth_dirty.load(Ordering::Acquire) {
            return;
        }
        let inputs = inner.ci_inputs.lock().unwrap();
        let mut changed = false;
        for (id, identity, snapshot, base) in updates {
            if inputs.get(&identity.cwd).copied().unwrap_or(0) != identity.metadata {
                continue;
            }
            let Some(pane) = state
                .record
                .registry
                .panes
                .iter_mut()
                .find(|pane| pane.id == id && pane.status == PaneStatus::Running)
            else {
                continue;
            };
            if project::effective_cwd(pane) != identity.cwd {
                continue;
            }
            let Some(project) = &mut pane.project else {
                continue;
            };
            if project.cwd != identity.cwd
                || project.state == ProjectState::NotRepository
                || self
                    .bindings
                    .get(&id)
                    .is_none_or(|binding| binding.project_error != project.error.is_some())
                || !project.repository.as_ref().is_some_and(|repository| {
                    repository.root == identity.root
                        && repository.head_sha.as_deref() == Some(identity.head.as_str())
                        && repository.branch == identity.branch
                })
            {
                continue;
            }
            changed |= project.ci != snapshot;
            project.ci.clone_from(&snapshot);
            if let Some(base) = base
                && let Some(repository) = &mut project.repository
            {
                let (reference, sha, committed) = base.unwrap_or_else(|_| {
                    (
                        project
                            .ci
                            .as_ref()
                            .and_then(|ci| ci.change_request.as_ref())
                            .and_then(|request| request.base.as_ref())
                            .map(|base| base.branch.clone()),
                        None,
                        None,
                    )
                });
                changed |= repository.base_ref != reference
                    || repository.base_sha != sha
                    || repository.committed != committed;
                repository.base_ref = reference;
                repository.base_sha = sha;
                repository.committed = committed;
            }
            if let Some(binding) = self.bindings.get_mut(&id) {
                binding.published_schedule =
                    binding.source.as_ref().zip(snapshot.as_ref()).and_then(
                        |(source, snapshot)| self.monitor.published_schedule(source, snapshot),
                    );
                binding.projected_metadata = Some(identity.metadata);
                binding.published_snapshot = snapshot;
            }
        }
        self.published_version = version;
        if changed && let Ok(revision) = next_revision(state.record.registry.revision) {
            state.record.registry.revision = revision;
            inner.changed.notify_all();
        }
    }
    fn retire_auth(&mut self, inner: &Inner) {
        self.monitor.retire_auth();
        self.bindings.clear();
        let _transaction = inner.transaction.lock().unwrap();
        let mut state = inner.state.lock().unwrap();
        let mut changed = false;
        for pane in &mut state.record.registry.panes {
            if let Some(project) = &mut pane.project
                && let Some(ci) = project.ci.take()
            {
                if ci
                    .change_request
                    .as_ref()
                    .and_then(|request| request.base.as_ref())
                    .is_some()
                    && let Some(repository) = &mut project.repository
                {
                    repository.base_ref = None;
                    repository.base_sha = None;
                    repository.committed = None;
                }
                changed = true;
            }
        }
        if changed && let Ok(revision) = next_revision(state.record.registry.revision) {
            state.record.registry.revision = revision;
            inner.changed.notify_all();
            let _ = inner.project_wake.try_send(());
        }
    }
    fn auth_events_changed(&mut self) -> bool {
        let mut changed = self.auth_overflow.swap(false, Ordering::AcqRel);
        while let Ok(event) = self.auth_events.try_recv() {
            changed |= event.as_ref().is_err()
                || event.as_ref().is_ok_and(|event| {
                    event.need_rescan()
                        || event.paths.is_empty()
                        || event.paths.iter().any(|path| {
                            self.auth_directories.iter().any(|config| {
                                path == config
                                    || config.starts_with(path)
                                    || path == &config.join("hosts.yml")
                            })
                        })
                });
        }
        if changed {
            // A directory may already have been recreated at the same pathname.
            // Do not keep an inode watch merely because its path still exists.
            self.auth_watcher.take();
            self.auth_watched.clear();
        }
        changed
    }
    fn watch_auth(&mut self) {
        let mut directories = BTreeSet::new();
        if let Some(config) = &self.auth_config {
            if config.is_absolute() {
                directories.insert(config.clone());
            } else {
                for binding in self.bindings.values().filter(|binding| binding.live) {
                    if let Some(source) = &binding.source {
                        directories.insert(source.cwd.join(config));
                    }
                }
            }
        }
        let canonical: Vec<_> = directories
            .iter()
            .filter_map(|path| {
                let existing = path.ancestors().find(|parent| parent.is_dir())?;
                Some(
                    fs::canonicalize(existing)
                        .ok()?
                        .join(path.strip_prefix(existing).ok()?),
                )
            })
            .collect();
        directories.extend(canonical);
        self.auth_directories = directories;
        let mut wanted = BTreeSet::new();
        for config in &self.auth_directories {
            if let Some(parent) = config
                .parent()
                .and_then(|parent| parent.ancestors().find(|path| path.is_dir()))
            {
                wanted.insert(parent.to_owned());
            }
            if config.is_dir() {
                wanted.insert(config.clone());
            }
        }
        let result = (|| -> notify::Result<()> {
            if self.auth_watcher.is_none() && !wanted.is_empty() {
                let dirty = Arc::clone(&self.auth_dirty);
                let overflow = Arc::clone(&self.auth_overflow);
                let events = self.auth_sender.clone();
                let wake = self.notify_wake.clone();
                self.auth_watcher = Some(notify::recommended_watcher(
                    move |event: notify::Result<notify::Event>| {
                        if event
                            .as_ref()
                            .is_ok_and(|event| matches!(event.kind, notify::EventKind::Access(_)))
                        {
                            return;
                        }
                        if events.try_send(event).is_err() {
                            overflow.store(true, Ordering::Release);
                        }
                        dirty.store(true, Ordering::Release);
                        let _ = wake.try_send(());
                    },
                )?);
            }
            if let Some(watcher) = &mut self.auth_watcher {
                for path in wanted.difference(&self.auth_watched) {
                    watcher.watch(path, RecursiveMode::NonRecursive)?;
                }
                for path in self.auth_watched.difference(&wanted) {
                    let _ = watcher.unwatch(path);
                }
            }
            self.auth_watched = wanted;
            Ok(())
        })();
        self.auth_retry = result
            .err()
            .map(|_| Instant::now() + Duration::from_secs(60));
    }
}

fn auth_directory() -> Option<PathBuf> {
    std::env::var_os("GH_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|root| PathBuf::from(root).join("gh")))
        .or_else(|| std::env::var_os("HOME").map(|root| PathBuf::from(root).join(".config/gh")))
}

#[cfg(test)]
mod tests;
