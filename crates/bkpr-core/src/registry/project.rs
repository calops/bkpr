//! Off-path, event-driven local project cache. One coordinator owns bounded Git
//! work and native watches; panes sharing a canonical worktree share its result.
//! Registry/input/render locks never encompass Git, canonicalization or walks.
mod git;
use super::*;
use bkpr_protocol::{ProjectSnapshot, ProjectState};
use crossbeam_channel::Receiver;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

const DEBOUNCE: Duration = Duration::from_millis(100);
const MAX_COALESCE: Duration = Duration::from_secs(1);
const MAX_NATIVE_WATCHES: usize = 65_536;

pub(super) fn loading(cwd: &Path, color_slot: Option<u32>) -> ProjectSnapshot {
    ProjectSnapshot {
        cwd: cwd.to_owned(),
        state: ProjectState::Loading,
        color_slot,
        repository: None,
        error: None,
    }
}
struct Entry {
    source: PathBuf,
    identity: Option<git::Identity>,
    identity_known: bool,
    snapshot: ProjectSnapshot,
    directories: BTreeSet<PathBuf>,
    external_files: BTreeSet<PathBuf>,
    rebuild: bool,
    metadata_dirty: bool,
    coverage_error: Option<String>,
    generation: u64,
    dirty_since: Option<Instant>,
    due: Option<Instant>,
}
impl Entry {
    fn new(source: PathBuf, identity: Option<git::Identity>, identity_known: bool) -> Self {
        Self {
            snapshot: loading(&source, None),
            source,
            identity,
            identity_known,
            directories: BTreeSet::new(),
            rebuild: true,
            external_files: BTreeSet::new(),
            metadata_dirty: true,
            coverage_error: None,
            generation: 0,
            dirty_since: Some(Instant::now()),
            due: Some(Instant::now()),
        }
    }
    fn invalidate(&mut self, rebuild: bool) {
        let now = Instant::now();
        let first = *self.dirty_since.get_or_insert(now);
        self.due = Some((now + DEBOUNCE).min(first + MAX_COALESCE));
        self.generation = self.generation.wrapping_add(1);
        self.rebuild |= rebuild;
        // Cached identity and facts survive ordinary edits. An error remains
        // observable until a successful observation replaces it.
        if self.snapshot.error.is_none() {
            self.snapshot.state = ProjectState::Loading;
        }
    }
}
struct Worker {
    registry: Weak<Inner>,
    cancel: Arc<AtomicBool>,
    wake: Receiver<()>,
    events: Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    watcher: Option<RecommendedWatcher>,
    event_sender: crossbeam_channel::Sender<notify::Result<Event>>,
    notify_wake: crossbeam_channel::Sender<()>,
    installed: BTreeSet<PathBuf>,
    bindings: BTreeMap<PathBuf, PathBuf>,
    canonical_cwds: BTreeMap<PathBuf, PathBuf>,
    cache: BTreeMap<PathBuf, Entry>,
    // Adapted from main 9ad93bd native.rs::sync_project_colors. Unlike main's
    // recovered model, these reservations exist only for this live worker.
    owners: BTreeMap<String, PathBuf>,
    color_slots: BTreeMap<PathBuf, u32>,
    rebind: BTreeSet<PathBuf>,
    inputs_pending: bool,
    watch_retry: Option<Instant>,
    retry_delay: Duration,
}

impl Registry {
    pub(super) fn start_projects(&self, wake: Receiver<()>) -> Result<()> {
        let weak = Arc::downgrade(&self.inner);
        let cancel = Arc::clone(&self.inner.project_cancel);
        let (events_tx, events) = crossbeam_channel::bounded(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let notify_wake = self.inner.project_wake.clone();
        let handle = thread::Builder::new()
            .name("bkpr-projects".into())
            .spawn(move || {
                Worker {
                    registry: weak,
                    cancel,
                    wake,
                    events,
                    overflow,
                    watcher: None,
                    event_sender: events_tx,
                    notify_wake,
                    installed: BTreeSet::new(),
                    bindings: BTreeMap::new(),
                    canonical_cwds: BTreeMap::new(),
                    cache: BTreeMap::new(),
                    owners: BTreeMap::new(),
                    color_slots: BTreeMap::new(),
                    rebind: BTreeSet::new(),
                    inputs_pending: false,
                    watch_retry: None,
                    retry_delay: Duration::from_secs(2),
                }
                .run();
            })?;
        *self.inner.project_worker.lock().unwrap() = Some(handle);
        Ok(())
    }
    pub(super) fn stop_projects(&self) {
        self.inner.project_cancel.store(true, Ordering::Release);
        let _ = self.inner.project_wake.try_send(());
        if let Some(worker) = self.inner.project_worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

impl Worker {
    fn run(&mut self) {
        loop {
            if self.cancel.load(Ordering::Acquire) {
                break;
            }
            self.drain_events();
            if !self.sync_inputs() {
                break;
            }
            if self.watch_retry.is_some_and(|due| due <= Instant::now()) {
                self.install_watches();
            }
            // Earliest invalidation first; no continuously writing project can
            // starve another project's initial discovery or refresh.
            let next = self
                .cache
                .iter()
                .filter_map(|(key, entry)| entry.due.map(|due| (key, due)))
                .min_by_key(|(_, due)| *due)
                .map(|(key, due)| (key.clone(), due));
            if let Some((key, due)) = &next
                && *due <= Instant::now()
            {
                self.refresh(key);
                continue;
            }
            self.publish();
            if self.inputs_pending {
                continue;
            }
            let deadline = next
                .map(|(_, due)| due)
                .into_iter()
                .chain(self.watch_retry)
                .min();
            if let Some(deadline) = deadline {
                let _ = self
                    .wake
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()));
            } else if self.wake.recv().is_err() {
                break;
            }
        }
    }
    fn sync_inputs(&mut self) -> bool {
        let Some(inner) = self.registry.upgrade() else {
            return false;
        };
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
                .map(|pane| (pane.id.clone(), effective_cwd(pane).to_owned()))
                .collect()
        };
        drop(inner);
        let desired_count = {
            let desired_cwds: BTreeSet<_> = desired.values().collect();
            self.bindings.retain(|cwd, _| desired_cwds.contains(cwd));
            self.canonical_cwds
                .retain(|cwd, _| desired_cwds.contains(cwd));
            self.rebind.retain(|cwd| desired_cwds.contains(cwd));
            desired_cwds.len()
        };
        for cwd in desired.into_values() {
            if self.bindings.contains_key(&cwd) && !self.rebind.contains(&cwd) {
                continue;
            }
            self.rebind.remove(&cwd);
            let source = fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone());
            self.canonical_cwds.insert(cwd.clone(), source.clone());
            let git = git::Git::new(&source, &self.cancel);
            let discovered = git.identify();
            let identity = discovered.as_ref().ok().cloned().flatten();
            let key = identity
                .as_ref()
                .map(|identity| identity.root.clone())
                .unwrap_or_else(|| source.clone());
            let source = identity
                .as_ref()
                .map(|identity| identity.root.clone())
                .unwrap_or(source);
            let identity_known = discovered.is_ok();
            let entry = self
                .cache
                .entry(key.clone())
                .or_insert_with(|| Entry::new(source.clone(), identity.clone(), identity_known));
            if entry.identity != identity || !entry.identity_known {
                entry.metadata_dirty = true;
            }
            if identity_known && entry.identity != identity {
                entry.source = source;
                entry.identity = identity;
                entry.snapshot.repository = None;
                entry.invalidate(true);
            }
            entry.identity_known = identity_known;
            if !entry.directories.contains(&self.canonical_cwds[&cwd]) {
                entry.invalidate(true);
            }
            if let Err(error) = discovered {
                entry.snapshot.state = ProjectState::Error;
                entry.snapshot.error = Some(format!("{error:#}"));
                // Refresh installs topology watches even for a broken .git; its
                // later repair must not require changing pane CWD.
            }
            self.bindings.insert(cwd, key);
            // Bound cold discovery to one CWD per turn so already dirty
            // worktrees receive ingestion before the next cold CWD.
            break;
        }
        self.inputs_pending = self.bindings.len() < desired_count || !self.rebind.is_empty();
        let active: BTreeSet<_> = self.bindings.values().cloned().collect();
        let before = self.cache.len();
        if !self.inputs_pending {
            self.cache.retain(|key, _| active.contains(key));
            if self.cache.len() != before {
                self.install_watches();
            }
        }
        true
    }
    fn refresh(&mut self, key: &Path) {
        let Some(entry) = self.cache.get(key) else {
            return;
        };
        let source = entry.source.clone();
        let generation = entry.generation;
        let rebuild = entry.rebuild;
        let previous_identity = entry.identity.clone();
        let git = git::Git::new(&source, &self.cancel);
        let discovered = if entry.identity_known {
            Ok(previous_identity.clone())
        } else {
            git.identify()
        };
        let identity_known = discovered.is_ok();
        let identity = match &discovered {
            Ok(identity) => identity.clone(),
            Err(_) => previous_identity.clone(),
        };
        let result = match discovered {
            Ok(Some(identity)) => git
                .repository(
                    &identity,
                    (!entry.metadata_dirty && entry.snapshot.error.is_none())
                        .then_some(entry.snapshot.repository.as_ref())
                        .flatten(),
                )
                .map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        let watches = if rebuild || identity != previous_identity {
            Some(git.watches(identity.as_ref()).and_then(|mut plan| {
                // A pane can live in an ignored directory or enter through a
                // symlink. Its canonical ancestry still needs topology coverage
                // so a nested repository can replace that binding.
                for (cwd, bound) in &self.bindings {
                    if bound.as_path() == key {
                        plan.directories.extend(
                            self.canonical_cwds[cwd]
                                .ancestors()
                                .filter(|path| path.is_dir())
                                .map(Path::to_path_buf),
                        );
                    }
                }
                // Admit a complete plan before retaining it: inactive/failed
                // plans cannot accumulate beyond the daemon's native budget.
                let mut combined: BTreeSet<_> = self
                    .cache
                    .iter()
                    .filter(|(other, _)| other.as_path() != key)
                    .flat_map(|(_, entry)| entry.directories.iter().cloned())
                    .collect();
                for path in &plan.directories {
                    combined.insert(path.clone());
                    ensure!(
                        combined.len() <= MAX_NATIVE_WATCHES,
                        "native project watch budget exceeded ({MAX_NATIVE_WATCHES})"
                    );
                }
                Ok(plan)
            }))
        } else {
            None
        };
        if self.cancel.load(Ordering::Acquire) {
            return;
        }
        let entry = self.cache.get_mut(key).unwrap();
        entry.identity = identity.clone();
        entry.identity_known = identity_known;
        if let Some(identity) = &identity {
            entry.source = identity.root.clone();
        }
        entry.due = None;
        entry.dirty_since = None;
        entry.rebuild = false;
        entry.metadata_dirty = false;
        let mut snapshot = entry.snapshot.clone();
        if identity_known && identity != previous_identity {
            snapshot.repository = None;
        }
        match result {
            Ok(repository) => {
                snapshot.state = if repository.is_some() {
                    ProjectState::Ready
                } else {
                    ProjectState::NotRepository
                };
                snapshot.repository = repository;
                snapshot.error = None;
            }
            Err(error) => {
                snapshot.state = ProjectState::Error;
                snapshot.error = Some(format!("{error:#}"));
            }
        }
        let mut watches_changed = false;
        if let Some(watches) = watches {
            match watches {
                Ok(plan) => {
                    watches_changed = plan.directories != entry.directories
                        || !plan.directories.is_subset(&self.installed);
                    entry.directories = plan.directories;
                    entry.external_files = plan.external_files;
                }
                Err(error) => {
                    snapshot.state = ProjectState::Error;
                    snapshot.error = Some(format!("project watch coverage: {error:#}"));
                    entry.coverage_error = snapshot.error.clone();
                    // Retain installed coverage and arrange bounded watch-plan
                    // retries after failure; never advertise incomplete Ready.
                    entry.rebuild = true;
                    self.watch_retry = Some(Instant::now() + self.retry_delay);
                    self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(60));
                }
            }
        }
        if let Some(error) = &entry.coverage_error {
            snapshot.state = ProjectState::Error;
            snapshot.error = Some(error.clone());
        }
        entry.snapshot = snapshot;
        // A directory can become a nested repo, or git init can create a new root.
        // Re-key aliases before publication; sibling panes still share one entry.
        if identity_known
            && let Some(identity) = identity
            && identity.root != key
        {
            let moved = self.cache.remove(key).unwrap();
            for binding in self.bindings.values_mut() {
                if binding == key {
                    *binding = identity.root.clone();
                }
            }
            if let Some(existing) = self.cache.get_mut(&identity.root) {
                existing.invalidate(true);
            } else {
                self.cache.insert(identity.root, moved);
            }
            self.install_watches();
            self.drain_events();
            return;
        }
        if watches_changed {
            self.install_watches();
        }
        // Events arriving while Git ran invalidate that result rather than
        // publishing an older generation as Ready after a newer file change.
        self.drain_events();
        if let Some(entry) = self.cache.get_mut(key)
            && entry.generation != generation
            && entry.snapshot.error.is_none()
        {
            entry.snapshot.state = ProjectState::Loading;
        }
        self.publish();
    }
    fn install_watches(&mut self) {
        let wanted: BTreeSet<_> = self
            .cache
            .values()
            .flat_map(|entry| entry.directories.iter().cloned())
            .collect();
        let result = (|| -> Result<BTreeSet<PathBuf>> {
            ensure!(
                wanted.len() <= MAX_NATIVE_WATCHES,
                "native project watch budget exceeded ({MAX_NATIVE_WATCHES})"
            );
            if self.watcher.is_none() && !wanted.is_empty() {
                self.watcher = Some(make_watcher(
                    self.event_sender.clone(),
                    self.overflow.clone(),
                    self.notify_wake.clone(),
                )?);
            }
            let mut added = BTreeSet::new();
            let install_deadline = Instant::now() + Duration::from_secs(10);
            let missing: Vec<_> = wanted.difference(&self.installed).cloned().collect();
            for path in missing {
                ensure!(
                    !self.cancel.load(Ordering::Acquire),
                    "project watch installation cancelled"
                );
                ensure!(
                    Instant::now() < install_deadline,
                    "project watch installation timed out"
                );
                self.watcher
                    .as_mut()
                    .context("native project watcher unavailable")?
                    .watch(&path, RecursiveMode::NonRecursive)
                    .with_context(|| format!("watching {}", path.display()))?;
                self.installed.insert(path.clone());
                added.insert(path);
            }
            Ok(added)
        })();
        match result {
            Ok(added) => {
                for path in self.installed.difference(&wanted) {
                    if let Some(watcher) = &mut self.watcher {
                        let _ = watcher.unwatch(path);
                    }
                }
                self.installed = wanted;
                let was_retry = self.watch_retry.take().is_some();
                if !self.cache.values().any(|entry| entry.rebuild) {
                    self.retry_delay = Duration::from_secs(2);
                }
                if !added.is_empty() || was_retry {
                    // Rebuild coverage as well as statistics: configuration or
                    // directories may have changed before their watches existed.
                    for entry in self.cache.values_mut() {
                        if entry.directories.is_disjoint(&added)
                            && !(was_retry && (entry.coverage_error.is_some() || entry.rebuild))
                        {
                            continue;
                        }
                        entry.coverage_error = None;
                        let identity_gap = entry.identity.as_ref().is_none_or(|identity| {
                            added.iter().any(|path| {
                                identity.git_dir.starts_with(path)
                                    || identity.common.starts_with(path)
                            })
                        });
                        entry.identity_known &= !identity_gap;
                        entry.metadata_dirty |= was_retry
                            || identity_gap
                            || added.iter().any(|path| {
                                metadata_change(entry, path)
                                    || entry
                                        .external_files
                                        .iter()
                                        .any(|file| file.starts_with(path))
                            });
                        entry.invalidate(true);
                    }
                    // Close discovery/subscription gaps for nested or ignored
                    // pane CWDs as well as the repository's own marker.
                    self.rebind.extend(
                        self.bindings
                            .keys()
                            .filter(|cwd| {
                                self.canonical_cwds[*cwd]
                                    .ancestors()
                                    .any(|path| added.contains(path))
                            })
                            .cloned(),
                    );
                }
            }
            Err(error) => {
                for entry in self.cache.values_mut() {
                    entry.snapshot.state = ProjectState::Error;
                    entry.snapshot.error =
                        Some(format!("native project watch coverage: {error:#}"));
                    entry.coverage_error = entry.snapshot.error.clone();
                    entry.due = None;
                }
                self.watch_retry = Some(Instant::now() + self.retry_delay);
                self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(60));
            }
        }
    }
    fn drain_events(&mut self) {
        if self.overflow.swap(false, Ordering::AcqRel) {
            self.invalidate_all("project event queue overflow; resynchronizing watch coverage");
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                Ok(event) => {
                    if event.need_rescan() {
                        self.invalidate_all("native project watcher lost events; resynchronizing");
                        continue;
                    }
                    let topology = match event.kind {
                        EventKind::Create(notify::event::CreateKind::File)
                        | EventKind::Remove(notify::event::RemoveKind::File) => false,
                        EventKind::Create(_) | EventKind::Remove(_) => true,
                        EventKind::Modify(notify::event::ModifyKind::Name(_)) => event
                            .paths
                            .iter()
                            .any(|path| path.is_dir() || self.installed.contains(path)),
                        _ => false,
                    };
                    if topology {
                        for path in &event.paths {
                            // A rename/replacement may already have recreated
                            // this pathname. Never retain the old inode's watch
                            // merely because is_dir() is true again.
                            self.installed.retain(|watched| {
                                if !watched.starts_with(path) {
                                    return true;
                                }
                                if let Some(watcher) = &mut self.watcher {
                                    let _ = watcher.unwatch(watched);
                                }
                                false
                            });
                        }
                    }
                    for (cwd, key) in &self.bindings {
                        let Some(entry) = self.cache.get(key) else {
                            continue;
                        };
                        if event.paths.iter().any(|path| {
                            identity_change(
                                &self.canonical_cwds[cwd],
                                entry.identity.as_ref(),
                                path,
                                topology,
                            )
                        }) {
                            self.rebind.insert(cwd.clone());
                        }
                    }
                    for entry in self.cache.values_mut() {
                        let mut affected = false;
                        let mut rebuild = topology;
                        let mut metadata = false;
                        for path in &event.paths {
                            if !relevant(entry, path) {
                                continue;
                            }
                            affected = true;
                            let rules = is_ignore(path)
                                || entry
                                    .external_files
                                    .iter()
                                    .any(|file| file == path || file.starts_with(path));
                            rebuild |= rules
                                || entry
                                    .identity
                                    .as_ref()
                                    .is_some_and(|identity| local_index(identity, path));
                            metadata |= rules || metadata_change(entry, path);
                        }
                        if affected {
                            if topology {
                                // Other affected entries may refresh later.
                                // Their old plans must not reinstall deleted or
                                // renamed paths in the meantime.
                                entry.directories.retain(|watched| {
                                    !event.paths.iter().any(|path| watched.starts_with(path))
                                });
                            }
                            entry.metadata_dirty |= metadata;
                            entry.invalidate(rebuild);
                        }
                    }
                }
                Err(error) => {
                    self.invalidate_all(&format!("native project watcher error: {error}"))
                }
            }
        }
    }
    fn invalidate_all(&mut self, reason: &str) {
        for entry in self.cache.values_mut() {
            entry.invalidate(true);
            entry.identity_known = false;
            entry.metadata_dirty = true;
            entry.snapshot.state = ProjectState::Error;
            entry.snapshot.error = Some(reason.to_owned());
        }
        self.rebind.extend(self.bindings.keys().cloned());
        self.publish();
        // Lost events may include deletion and recreation at the same path.
        // A remembered pathname is not proof its old native watch still exists.
        self.watcher.take();
        self.installed.clear();
        self.watch_retry = Some(Instant::now());
    }
    fn publish(&mut self) {
        let Some(inner) = self.registry.upgrade() else {
            return;
        };
        let _transaction = inner.transaction.lock().unwrap();
        let mut state = inner.state.lock().unwrap();
        if state.closing {
            return;
        }
        let retired = std::mem::take(&mut state.project_retired);
        if !retired.is_empty() {
            self.owners.retain(|id, _| !retired.contains(id));
            let live: BTreeSet<_> = self.owners.values().collect();
            self.color_slots.retain(|common, _| live.contains(common));
        }
        let panes: BTreeMap<_, _> = state
            .record
            .registry
            .panes
            .iter()
            .filter(|pane| pane.status == PaneStatus::Running)
            .map(|pane| (&pane.id, pane))
            .collect();
        self.owners.retain(|id, _| panes.contains_key(id));
        let mut unbound = false;
        for pane in panes.values() {
            let Some(entry) = self
                .bindings
                .get(effective_cwd(pane))
                .and_then(|key| self.cache.get(key))
            else {
                unbound = true;
                continue;
            };
            if !entry.identity_known {
                // A metadata error or asynchronous CWD gap is not evidence that
                // the pane left its previous canonical repository.
                continue;
            }
            if let Some(identity) = &entry.identity {
                if self.owners.get(&pane.id) != Some(&identity.common) {
                    self.owners.insert(pane.id.clone(), identity.common.clone());
                }
            } else {
                self.owners.remove(&pane.id);
            }
        }
        let live: BTreeSet<_> = self.owners.values().collect();
        self.color_slots.retain(|common, _| live.contains(common));
        if !unbound {
            // Cold CWDs are discovered incrementally. Allocate only once this
            // input batch is bound, so CWD/cache ordering cannot choose hues.
            let occupied: BTreeSet<_> = self.color_slots.values().copied().collect();
            let mut available = (0..=u32::MAX).filter(|slot| !occupied.contains(slot));
            for common in self.owners.values() {
                if !self.color_slots.contains_key(common)
                    && let Some(slot) = available.next()
                {
                    self.color_slots.insert(common.clone(), slot);
                }
            }
        }
        drop(panes);
        let mut changed = false;
        for pane in &mut state.record.registry.panes {
            if pane.status != PaneStatus::Running {
                continue;
            }
            let cwd = effective_cwd(pane);
            let Some(entry) = self.bindings.get(cwd).and_then(|key| self.cache.get(key)) else {
                continue;
            };
            let color_slot = self
                .owners
                .get(&pane.id)
                .and_then(|common| self.color_slots.get(common))
                .copied();
            if pane.project.as_ref().is_some_and(|current| {
                current.cwd == cwd
                    && current.state == entry.snapshot.state
                    && current.color_slot == color_slot
                    && current.repository == entry.snapshot.repository
                    && current.error == entry.snapshot.error
            }) {
                continue;
            }
            let mut snapshot = entry.snapshot.clone();
            snapshot.cwd = cwd.to_owned();
            snapshot.color_slot = color_slot;
            pane.project = Some(snapshot);
            changed = true;
        }
        if changed && let Ok(revision) = next_revision(state.record.registry.revision) {
            state.record.registry.revision = revision;
            inner.changed.notify_all();
        }
    }
}
fn effective_cwd(pane: &PaneInfo) -> &Path {
    pane.foreground_cwd.as_deref().unwrap_or(&pane.cwd)
}
fn is_ignore(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some(
                ".gitignore"
                    | ".ignore"
                    | "ignore"
                    | "exclude"
                    | "config"
                    | ".gitconfig"
                    | "config.worktree"
                    | ".gitattributes"
            )
        )
    })
}
fn relevant(entry: &Entry, path: &Path) -> bool {
    let parent_watched = path
        .parent()
        .is_some_and(|parent| entry.directories.contains(parent));
    let Some(identity) = &entry.identity else {
        return parent_watched
            && (path.ancestors().any(|marker| {
                marker.file_name().is_some_and(|name| name == ".git")
                    && marker
                        .parent()
                        .is_some_and(|root| entry.source.starts_with(root))
            }) || entry.source.starts_with(path));
    };
    // Common/git parents are topology watches, not subscriptions to unrelated
    // worktrees, object creation, logs, hooks or the surrounding filesystem.
    for metadata in [&identity.git_dir, &identity.common] {
        if let Ok(relative) = path.strip_prefix(metadata) {
            let first = relative.components().next().map(|part| part.as_os_str());
            let local = metadata == &identity.git_dir;
            // Git's documented exceptions to shared refs remain local even
            // though the main worktree stores them beneath the common dir.
            if !local
                && ["refs/bisect", "refs/worktree", "refs/rewritten"]
                    .iter()
                    .any(|namespace| relative.starts_with(namespace))
            {
                return false;
            }
            return first.is_none()
                || first.is_some_and(|name| {
                    matches!(
                        name.to_str(),
                        Some("config" | "packed-refs" | "refs" | "info" | "shallow")
                    ) || (local
                        && matches!(
                            name.to_str(),
                            Some("HEAD" | "index" | "commondir" | "config.worktree")
                        ))
                });
        }
    }
    if path.starts_with(&identity.root) {
        return parent_watched || identity.root.starts_with(path);
    }
    if identity.root.starts_with(path) || entry.source.starts_with(path) {
        return parent_watched;
    }
    // Parents of configured global ignore files are deliberately nonrecursive.
    parent_watched
        && entry
            .external_files
            .iter()
            .any(|file| file == path || file.starts_with(path))
}

// Only a marker on this CWD's ancestry can change its repository association.
// Metadata parents are watched for replacement, not for sibling repositories.
fn identity_change(
    cwd: &Path,
    identity: Option<&git::Identity>,
    path: &Path,
    topology: bool,
) -> bool {
    if (path.file_name().is_some_and(|name| name == ".git")
        && path.parent().is_some_and(|root| cwd.starts_with(root)))
        || (topology && cwd.starts_with(path))
    {
        return true;
    }
    identity.is_some_and(|identity| {
        [&identity.git_dir, &identity.common]
            .iter()
            .any(|metadata| path == metadata.as_path() || (topology && metadata.starts_with(path)))
            || path
                .strip_prefix(&identity.git_dir)
                .is_ok_and(|relative| relative == Path::new("commondir"))
    })
}

fn metadata_change(entry: &Entry, path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name == ".gitattributes")
    {
        return true;
    }
    entry.identity.as_ref().is_none_or(|identity| {
        [&identity.git_dir, &identity.common]
            .iter()
            .any(|metadata| {
                (path.starts_with(metadata) || metadata.starts_with(path))
                    && !local_index(identity, path)
            })
    })
}

fn local_index(identity: &git::Identity, path: &Path) -> bool {
    path.strip_prefix(&identity.git_dir)
        .is_ok_and(|relative| relative == Path::new("index"))
}

fn make_watcher(
    events: crossbeam_channel::Sender<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    wake: crossbeam_channel::Sender<()>,
) -> notify::Result<RecommendedWatcher> {
    notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event
            .as_ref()
            .is_ok_and(|event| matches!(event.kind, EventKind::Access(_)))
        {
            return;
        }
        if event.as_ref().is_ok_and(|event| event.paths.len() > 64)
            || events.try_send(event).is_err()
        {
            overflow.store(true, Ordering::Release);
        }
        let _ = wake.try_send(());
    })
}

#[cfg(test)]
mod tests;
