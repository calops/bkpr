//! Shared forge CI batching, scheduling, and cache; Git discovery is event-driven.
//! Requests share remote targets within one canonical CWD and retain local identity.
//! Adapted from pre-native-rewrite e9ee402 (crates/bkpr/src/ci.rs); native Unix
//! child ownership, context-scoped backoff and source leases replace the plugin loop.
use bkpr_protocol::CiStatus;
use bkpr_protocol::{CiFetchState, CiSnapshot, ForgeRepository};
use bkpr_protocol::{CiRefreshTarget, ForgeProvider};
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

#[path = "github.rs"]
pub(super) mod github;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CiSource {
    /// Exact item directory, not its worktree root: gh wrappers may select auth by CWD.
    pub cwd: PathBuf,
    /// Retired metadata generations never rejoin an in-flight A→B→A result.
    pub generation: u64,
    pub repository: ForgeRepository,
    pub sha: String,
    pub branch: Option<String>,
    pub remote_branch: Option<RemoteBranch>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RemoteBranch {
    pub remote: String,
    pub name: String,
    // Local tracking refs only invalidate generations; the provider resolves the tip.
    pub tracking_sha: Option<String>,
}

impl CiSource {
    pub(crate) fn checked_ref(&self) -> Option<String> {
        self.remote_branch
            .as_ref()
            .map(|branch| format!("{}/{}", branch.remote, branch.name))
    }

    pub(crate) fn matches_snapshot(&self, snapshot: &CiSnapshot) -> bool {
        self.repository == snapshot.repository
            && !snapshot.local_head_sha.is_empty()
            && self.sha == snapshot.local_head_sha
            && self.branch == snapshot.branch
            && (snapshot.fetch_state != CiFetchState::Ready || !snapshot.head_sha.is_empty())
            && match (&self.remote_branch, snapshot.checked_ref.as_deref()) {
                (Some(branch), Some(reference)) => {
                    reference
                        .strip_prefix(branch.remote.as_str())
                        .and_then(|reference| reference.strip_prefix('/'))
                        == Some(branch.name.as_str())
                }
                (None, None) => true,
                _ => false,
            }
            && (self.remote_branch.is_some() || self.sha == snapshot.head_sha)
    }
}

#[derive(Clone, Copy, Debug)]
enum RetryPolicy {
    Normal,
    Authentication,
    RateLimited,
}

#[derive(Clone, Debug)]
struct FetchError {
    message: String,
    retry: RetryPolicy,
}
impl FetchError {
    fn normal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry: RetryPolicy::Normal,
        }
    }
    fn delay(&self, failures: u32) -> u64 {
        match self.retry {
            RetryPolicy::Normal => (60u64 << failures.min(5)).min(1800),
            RetryPolicy::Authentication => 1800,
            RetryPolicy::RateLimited => 3600,
        }
    }
}

struct BatchResponse {
    results: BTreeMap<CiSource, Result<CiSnapshot, FetchError>>,
    global_error: Option<FetchError>,
    retry_after: Option<u64>,
}

#[cfg(test)]
fn discover(cwd: &Path, branch: Option<&str>) -> Option<CiSource> {
    super::discovery::discover(cwd, branch, &std::sync::atomic::AtomicBool::new(false))
        .ok()
        .flatten()
}

const BATCH: usize = 16;
const MIN_REQUEST_SECONDS: u64 = 2;
const ACTIVE_SECONDS: u64 = 60;
const ERROR_RETRY_SECONDS: u64 = 300;
const DEADLINE_SECONDS: u64 = 20;

fn empty(source: &CiSource, now: u64) -> CiSnapshot {
    CiSnapshot {
        status: CiStatus::Unknown,
        checks: Vec::new(),
        change_request: None,
        fetch_state: CiFetchState::Loading,
        updated_at: now,
        stale: false,
        error: None,
        repository: source.repository.clone(),
        head_sha: if source.remote_branch.is_some() {
            String::new()
        } else {
            source.sha.clone()
        },
        local_head_sha: source.sha.clone(),
        checked_ref: source.checked_ref(),
        branch: source.branch.clone(),
        truncated: false,
        commit_missing: false,
    }
}

fn same_target(left: &CiSource, right: &CiSource) -> bool {
    left.cwd == right.cwd
        && left.repository == right.repository
        && match (&left.remote_branch, &right.remote_branch) {
            (Some(left), Some(right)) => left.name == right.name,
            (None, None) => left.sha == right.sha,
            _ => false,
        }
}

pub(super) use portable::Monitor;

mod portable {
    use super::*;
    use std::collections::{BTreeSet, BinaryHeap};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Entry {
        snapshot: CiSnapshot,
        due: Option<Instant>,
        eager: bool,
        admission_waiting: bool,
        request_limited: bool,
        request_bytes: Option<usize>,
    }
    const RETRY_DEADLINE_ERROR: &str = "Provider retry deadline exceeds the supported clock range; CI requests for this authentication context are blocked until its authentication context is retired or the daemon restarts";
    // A daemon-wide memory allowance for retained authentication embargoes, not
    // a subscription limit. Active metadata belongs to current pane membership.
    const CONTEXT_HISTORY_BYTES: usize = 1024 * 1024;
    const CONTEXT_CAPACITY_ERROR: &str = "CI authentication-context history memory exhausted; waiting for provider retry deadlines to expire, authentication-context retirement, or daemon restart";
    // Bound one CLI payload and its cloned source identities independently of
    // active subscriptions. The provider's response remains separately bounded.
    const REQUEST_WORK_BYTES: usize = 1024 * 1024;
    const SOURCE_CAPACITY_ERROR: &str = "CI source metadata exceeds the local request memory allowance; waiting for source metadata to change";

    fn source_work_bytes(source: &CiSource) -> Option<usize> {
        let remote = source.remote_branch.as_ref();
        let strings = [
            source.cwd.as_os_str().len(),
            source.repository.host.len(),
            source.repository.path.len(),
            source.sha.len(),
            source.branch.as_ref().map_or(0, String::len),
            remote.map_or(0, |branch| branch.remote.len()),
            remote.map_or(0, |branch| branch.name.len()),
            remote
                .and_then(|branch| branch.tracking_sha.as_ref())
                .map_or(0, String::len),
        ];
        // GitHub's fixed check/PR query fragments use less than 2 KiB/source.
        // A branch appears at most three times; 48 bytes/input byte covers
        // JSON's six-byte escapes, the enclosing JSON escaping and source clones.
        strings.into_iter().try_fold(
            2048usize.checked_add(std::mem::size_of::<CiSource>())?,
            |bytes, length| bytes.checked_add(length.checked_mul(48)?),
        )
    }

    fn block_entry(entry: &mut Entry) {
        entry.due = None;
        entry.eager = false;
        entry.admission_waiting = false;
        entry.snapshot.stale = entry.snapshot.updated_at != 0;
        entry.snapshot.fetch_state = CiFetchState::Unavailable;
        entry.snapshot.error = Some(RETRY_DEADLINE_ERROR.to_owned());
    }
    struct Pending {
        child: Child,
        stdout: File,
        stderr: File,
        sources: Vec<CiSource>,
        deadline: Instant,
        group_retired: bool,
        waited: bool,
    }
    impl Pending {
        fn exited(&self) -> std::io::Result<bool> {
            // Observe without reaping: the zombie leader reserves its PID/PGID
            // until the owned process group has been retired.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(info.si_signo != 0)
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            if !self.group_retired {
                self.group_retired = true;
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
                let _ = self.child.kill();
            }
            self.waited = true;
            self.child.wait()
        }
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if !self.waited {
                let _ = self.wait();
            }
        }
    }

    #[derive(Default)]
    struct Backoff {
        soft: Option<Instant>,
        hard: Option<Instant>,
        failures: u32,
        blocked: bool,
    }
    type ContextBackoffs = BTreeMap<PathBuf, BTreeMap<ForgeProvider, BTreeMap<String, Backoff>>>;

    // Charge a whole BTree node per entry, including spare key/value slots and
    // internal edges. This deliberately overcounts shared/partially full nodes:
    // std's BTree nodes have at most 11 slots and 12 edges. The extra words cover
    // the node header and allocation bookkeeping, without a hidden context cap.
    fn context_node_bytes<K, V>() -> usize {
        11 * (std::mem::size_of::<K>() + std::mem::size_of::<V>())
            + 16 * std::mem::size_of::<usize>()
    }

    fn context_storage_bytes(contexts: &ContextBackoffs) -> Option<usize> {
        contexts.iter().try_fold(0usize, |bytes, (cwd, providers)| {
            let bytes = bytes
                .checked_add(context_node_bytes::<
                    PathBuf,
                    BTreeMap<ForgeProvider, BTreeMap<String, Backoff>>,
                >())?
                .checked_add(cwd.capacity())?;
            providers.values().try_fold(bytes, |bytes, hosts| {
                let bytes = bytes
                    .checked_add(
                        context_node_bytes::<ForgeProvider, BTreeMap<String, Backoff>>(),
                    )?;
                hosts.iter().try_fold(bytes, |bytes, (host, _)| {
                    bytes
                        .checked_add(context_node_bytes::<String, Backoff>())?
                        .checked_add(host.capacity())
                })
            })
        })
    }

    fn context_admission_bytes(contexts: &ContextBackoffs, source: &CiSource) -> Option<usize> {
        let providers = contexts.get(&source.cwd);
        let hosts = providers.and_then(|providers| providers.get(&source.repository.provider));
        if hosts.is_some_and(|hosts| hosts.contains_key(&source.repository.host)) {
            return Some(0);
        }
        let mut bytes =
            context_node_bytes::<String, Backoff>().checked_add(source.repository.host.len())?;
        if hosts.is_none() {
            bytes = bytes
                .checked_add(context_node_bytes::<ForgeProvider, BTreeMap<String, Backoff>>())?;
        }
        if providers.is_none() {
            bytes = bytes
                .checked_add(context_node_bytes::<
                    PathBuf,
                    BTreeMap<ForgeProvider, BTreeMap<String, Backoff>>,
                >())?
                .checked_add(source.cwd.as_os_str().len())?;
        }
        Some(bytes)
    }
    pub(crate) struct Monitor {
        entries: BTreeMap<CiSource, Entry>,
        pending: Option<Pending>,
        next_request: Instant,
        contexts: ContextBackoffs,
        context_retry: Option<Instant>,
        context_budget_bytes: usize,
        version: u64,
        #[cfg(test)]
        requests: u64,
    }
    impl Monitor {
        pub(crate) fn new() -> Self {
            Self {
                entries: BTreeMap::new(),
                pending: None,
                next_request: Instant::now(),
                contexts: BTreeMap::new(),
                context_retry: None,
                context_budget_bytes: CONTEXT_HISTORY_BYTES,
                version: 0,
                #[cfg(test)]
                requests: 0,
            }
        }
        fn backoff(&self, source: &CiSource) -> Option<&Backoff> {
            self.contexts
                .get(&source.cwd)?
                .get(&source.repository.provider)?
                .get(&source.repository.host)
        }
        fn context_mut<'a>(
            contexts: &'a mut ContextBackoffs,
            source: &CiSource,
        ) -> &'a mut Backoff {
            if !contexts.contains_key(&source.cwd) {
                contexts.insert(source.cwd.clone(), BTreeMap::new());
            }
            let providers = contexts.get_mut(&source.cwd).expect("context inserted");
            let hosts = providers.entry(source.repository.provider).or_default();
            if !hosts.contains_key(&source.repository.host) {
                hosts.insert(source.repository.host.clone(), Backoff::default());
            }
            hosts
                .get_mut(&source.repository.host)
                .expect("host context inserted")
        }
        fn backoff_mut(&mut self, source: &CiSource) -> &mut Backoff {
            Self::context_mut(&mut self.contexts, source)
        }
        fn reconcile_contexts(&mut self, now: Instant) {
            let entries = &mut self.entries;
            let contexts = &mut self.contexts;
            let active: BTreeSet<_> = entries
                .keys()
                .map(|source| {
                    (
                        source.cwd.as_path(),
                        source.repository.provider,
                        source.repository.host.as_str(),
                    )
                })
                .collect();
            let mut retry = None;
            contexts.retain(|cwd, providers| {
                providers.retain(|provider, hosts| {
                    hosts.retain(|host, backoff| {
                        let subscribed =
                            active.contains(&(cwd.as_path(), *provider, host.as_str()));
                        let expiry = backoff.soft.into_iter().chain(backoff.hard).max();
                        let retained =
                            backoff.blocked || expiry.is_some_and(|due| due > now) || subscribed;
                        if retained
                            && !subscribed
                            && !backoff.blocked
                            && let Some(expiry) = expiry
                        {
                            retry = Some(retry.map_or(expiry, |due: Instant| due.min(expiry)));
                        }
                        retained
                    });
                    !hosts.is_empty()
                });
                !providers.is_empty()
            });
            let mut used = context_storage_bytes(contexts);
            for (source, entry) in entries.iter_mut() {
                let context = contexts
                    .get(&source.cwd)
                    .and_then(|providers| providers.get(&source.repository.provider))
                    .and_then(|hosts| hosts.get(&source.repository.host));
                if context.is_some_and(|context| context.blocked) {
                    if entry.admission_waiting
                        || entry.snapshot.error.as_deref() != Some(RETRY_DEADLINE_ERROR)
                    {
                        block_entry(entry);
                        self.version = self.version.wrapping_add(1);
                    }
                    continue;
                }
                if context.is_none() {
                    let admitted = used
                        .and_then(|used| {
                            used.checked_add(context_admission_bytes(contexts, source)?)
                        })
                        .filter(|bytes| *bytes <= self.context_budget_bytes);
                    if admitted.is_none() {
                        if !entry.admission_waiting
                            || entry.snapshot.fetch_state != CiFetchState::Unavailable
                        {
                            entry.snapshot.stale = entry.snapshot.updated_at != 0;
                            entry.snapshot.fetch_state = CiFetchState::Unavailable;
                            entry.snapshot.error = Some(CONTEXT_CAPACITY_ERROR.to_owned());
                            self.version = self.version.wrapping_add(1);
                        }
                        entry.admission_waiting = true;
                        entry.due = None;
                        entry.eager = false;
                        continue;
                    }
                    Self::context_mut(contexts, source);
                    used = admitted;
                }
                if entry.admission_waiting {
                    entry.admission_waiting = false;
                    entry.due = Some(now);
                    entry.eager = true;
                    if entry.snapshot.error.as_deref() == Some(CONTEXT_CAPACITY_ERROR) {
                        entry.snapshot.error = None;
                    }
                    if entry.snapshot.fetch_state == CiFetchState::Unavailable {
                        refreshing(&mut entry.snapshot);
                        self.version = self.version.wrapping_add(1);
                    }
                }
            }
            self.context_retry = entries
                .values()
                .any(|entry| entry.admission_waiting)
                .then_some(retry)
                .flatten();
        }
        fn request_deadline(&self, source: &CiSource) -> Instant {
            self.backoff(source)
                .and_then(|backoff| backoff.hard)
                .map_or(self.next_request, |hard| self.next_request.max(hard))
        }
        fn soft_deadline(&self, source: &CiSource) -> Option<Instant> {
            self.backoff(source).and_then(|backoff| backoff.soft)
        }
        fn blocked(&self, source: &CiSource) -> bool {
            self.backoff(source).is_some_and(|backoff| backoff.blocked)
        }
        pub(crate) fn retire_auth(&mut self) {
            self.pending = None;
            self.contexts.clear();
            self.sync(std::iter::empty());
        }
        pub(crate) fn sync(&mut self, sources: impl Iterator<Item = CiSource>) {
            let wanted: BTreeSet<_> = sources.collect();
            if wanted.len() != self.entries.len()
                || wanted
                    .iter()
                    .any(|source| !self.entries.contains_key(source))
            {
                self.version = self.version.wrapping_add(1);
            }
            if self.pending.as_ref().is_some_and(|pending| {
                pending
                    .sources
                    .iter()
                    .any(|source| !wanted.contains(source))
            }) {
                // Retire removed identities, including A→B→A, but not unrelated new consumers.
                self.pending = None;
            }
            for source in &wanted {
                if self.entries.contains_key(source) {
                    continue;
                }
                let mut snapshot = empty(source, 0);
                // Only a retired HEAD on the same named branch can lend its PR
                // association. Commit checks and completed PRs never cross SHAs.
                let mut previous = self.entries.iter().filter(|(old, entry)| {
                    !wanted.contains(*old)
                        && old.cwd == source.cwd
                        && old.repository == source.repository
                        && old.branch == source.branch
                        && old
                            .remote_branch
                            .as_ref()
                            .map(|branch| (&branch.remote, &branch.name))
                            == source
                                .remote_branch
                                .as_ref()
                                .map(|branch| (&branch.remote, &branch.name))
                        && source.branch.is_some()
                        && entry
                            .snapshot
                            .change_request
                            .as_ref()
                            .is_some_and(|request| {
                                matches!(
                                    request.state,
                                    bkpr_protocol::ChangeRequestState::Open
                                        | bkpr_protocol::ChangeRequestState::Draft
                                )
                            })
                });
                if let Some((_, entry)) = previous.next()
                    && previous.next().is_none()
                {
                    snapshot.change_request = entry.snapshot.change_request.clone();
                    snapshot.updated_at = entry.snapshot.updated_at;
                    snapshot.stale = true;
                }
                self.entries.insert(
                    source.clone(),
                    Entry {
                        snapshot,
                        due: Some(Instant::now()),
                        eager: true,
                        admission_waiting: true,
                        request_limited: false,
                        request_bytes: source_work_bytes(source),
                    },
                );
            }
            self.entries.retain(|source, _| wanted.contains(source));
            self.reconcile_contexts(Instant::now());
        }

        pub(crate) fn refresh(&mut self, target: &CiRefreshTarget) {
            let selected = self
                .entries
                .iter()
                .find(|(source, entry)| {
                    source.cwd == target.cwd && matches_target(&entry.snapshot, target)
                })
                .map(|(source, _)| source.clone());
            let Some(selected) = selected else {
                return;
            };
            if self.blocked(&selected)
                || self.entries[&selected].admission_waiting
                || self.entries[&selected].request_limited
            {
                return;
            }
            for (source, entry) in &mut self.entries {
                let in_flight = self.pending.as_ref().is_some_and(|pending| {
                    pending
                        .sources
                        .iter()
                        .any(|queried| same_target(queried, source))
                });
                if !same_target(&selected, source) || in_flight || entry.request_limited {
                    continue;
                }
                // Explicit refresh bypasses soft backoff, never minimum spacing
                // or the hard provider rate-limit deadline.
                if entry.snapshot.fetch_state != CiFetchState::Loading {
                    entry.due = Some(Instant::now());
                    entry.eager = true;
                    refreshing(&mut entry.snapshot);
                    self.version = self.version.wrapping_add(1);
                }
            }
        }
        pub(crate) fn source_unavailable(&mut self, source: &CiSource, error: &str) {
            if self.blocked(source)
                || self
                    .entries
                    .get(source)
                    .is_some_and(|entry| entry.admission_waiting || entry.request_limited)
            {
                return;
            }
            if let Some(entry) = self.entries.get_mut(source) {
                entry.due = None;
                if entry.snapshot.fetch_state != CiFetchState::Unavailable
                    || entry.snapshot.error.as_deref() != Some(error)
                {
                    self.version = self.version.wrapping_add(1);
                }
                entry.snapshot.stale = entry.snapshot.updated_at != 0;
                entry.snapshot.fetch_state = CiFetchState::Unavailable;
                entry.snapshot.error = Some(error.to_owned());
            }
        }
        pub(crate) fn resume(&mut self, source: &CiSource) {
            if self.blocked(source)
                || self
                    .entries
                    .get(source)
                    .is_some_and(|entry| entry.admission_waiting || entry.request_limited)
            {
                return;
            }
            if let Some(entry) = self.entries.get_mut(source) {
                entry.due = Some(Instant::now());
                entry.eager = true;
                refreshing(&mut entry.snapshot);
                self.version = self.version.wrapping_add(1);
            }
        }
        pub(crate) fn published_schedule(
            &self,
            source: &CiSource,
            snapshot: &CiSnapshot,
        ) -> Option<(Option<Instant>, bool)> {
            let entry = self.entries.get(source)?;
            (entry.snapshot == *snapshot).then_some((entry.due, entry.eager))
        }
        pub(crate) fn restore_published(
            &mut self,
            source: &CiSource,
            snapshot: CiSnapshot,
            schedule: Option<(Option<Instant>, bool)>,
        ) {
            if self.blocked(source)
                || self
                    .entries
                    .get(source)
                    .is_some_and(|entry| entry.admission_waiting || entry.request_limited)
                || !source.matches_snapshot(&snapshot)
            {
                return;
            }
            if let Some(entry) = self.entries.get_mut(source) {
                let (due, eager) = schedule.unwrap_or_else(|| {
                    let due = (!final_snapshot(&snapshot)).then(Instant::now);
                    (due, due.is_some())
                });
                entry.due = due;
                entry.eager = eager;
                entry.snapshot = snapshot;
                self.version = self.version.wrapping_add(1);
            }
        }
        pub(crate) fn version(&self) -> u64 {
            self.version
        }
        pub(crate) fn snapshot_ref(&self, source: &CiSource) -> Option<&CiSnapshot> {
            self.entries.get(source).map(|entry| &entry.snapshot)
        }
        #[cfg(test)]
        pub(crate) fn snapshot(&self, source: &CiSource) -> Option<CiSnapshot> {
            self.entries.get(source).map(|entry| entry.snapshot.clone())
        }
        #[cfg(test)]
        pub(crate) fn request_pid(&self) -> i32 {
            self.pending
                .as_ref()
                .map_or(-1, |pending| pending.child.id() as i32)
        }
        pub(crate) fn timeout(&self) -> i32 {
            let deadline = self
                .pending
                .as_ref()
                .map(|pending| pending.deadline)
                .or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(source, entry)| !entry.admission_waiting && !self.blocked(source))
                        .filter_map(|(source, entry)| {
                            entry.due.map(|due| {
                                let due = due.max(self.request_deadline(source));
                                if entry.eager {
                                    due
                                } else {
                                    self.soft_deadline(source).map_or(due, |soft| due.max(soft))
                                }
                            })
                        })
                        .min()
                });
            let deadline = deadline.into_iter().chain(self.context_retry).min();
            let timeout = deadline.map_or(-1, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(i32::MAX as u128) as i32
            });
            if self.pending.is_some() {
                timeout.clamp(0, 50)
            } else {
                timeout
            }
        }
        pub(crate) fn cancel_pending(&mut self) {
            self.pending = None;
        }
        /// The caller drains Git events before applying any response.
        pub(crate) fn advance(&mut self) {
            let now = Instant::now();
            if self.context_retry.is_some_and(|due| due <= now) {
                self.reconcile_contexts(now);
            }
            let observed = self.pending.as_ref().map(Pending::exited);
            let done = self
                .pending
                .as_ref()
                .is_some_and(|pending| now >= pending.deadline)
                || matches!(observed, Some(Ok(true) | Err(_)));
            if done {
                let mut pending = self.pending.take().unwrap();
                let result = if now >= pending.deadline {
                    Err(FetchError::normal("CI request timed out"))
                } else if matches!(observed, Some(Err(_))) {
                    Err(FetchError::normal("CI CLI process status is unavailable"))
                } else {
                    response(&mut pending)
                };
                self.finish(&pending.sources, result);
            }
            let sources = self.reserve_batch(now);
            if sources.is_empty() {
                return;
            }
            match start(&sources) {
                Ok(pending) => self.pending = Some(pending),
                Err(error) => self.finish(
                    &sources,
                    Err(FetchError::normal(format!("CI CLI unavailable: {error}"))),
                ),
            }
        }
        fn reserve_batch(&mut self, now: Instant) -> Vec<CiSource> {
            if self.pending.is_some() || now < self.next_request {
                return Vec::new();
            }
            for entry in self.entries.values_mut() {
                if !entry.admission_waiting
                    && entry.due.is_some()
                    && entry
                        .request_bytes
                        .is_none_or(|bytes| bytes > REQUEST_WORK_BYTES)
                {
                    entry.due = None;
                    entry.eager = false;
                    entry.request_limited = true;
                    entry.snapshot.stale = entry.snapshot.updated_at != 0;
                    entry.snapshot.fetch_state = CiFetchState::Unavailable;
                    entry.snapshot.error = Some(SOURCE_CAPACITY_ERROR.to_owned());
                    self.version = self.version.wrapping_add(1);
                }
            }
            let sources = self.due_batch(now);
            if !sources.is_empty() {
                self.version = self.version.wrapping_add(1);
                self.next_request = now + Duration::from_secs(MIN_REQUEST_SECONDS);
                #[cfg(test)]
                {
                    self.requests += 1;
                }

                for source in &sources {
                    if let Some(entry) = self.entries.get_mut(source) {
                        refreshing(&mut entry.snapshot);
                    }
                }
            }
            sources
        }
        fn due_batch(&self, now: Instant) -> Vec<CiSource> {
            let eligible = |source: &CiSource, entry: &Entry| {
                !entry.admission_waiting
                    && !self.blocked(source)
                    && entry.due.is_some_and(|due| due <= now)
                    && self
                        .backoff(source)
                        .and_then(|backoff| backoff.hard)
                        .is_none_or(|hard| now >= hard)
                    && (entry.eager || self.soft_deadline(source).is_none_or(|soft| now >= soft))
                    && entry
                        .request_bytes
                        .is_some_and(|bytes| bytes <= REQUEST_WORK_BYTES)
            };
            let Some((first, _)) = self
                .entries
                .iter()
                .filter(|(source, entry)| eligible(source, entry))
                .min_by_key(|(_, entry)| entry.due)
            else {
                return Vec::new();
            };
            // Select only a bounded set of remote targets; don't sort/clone all
            // active subscriptions just to identify the next unit of work.
            let mut targets: Vec<(&CiSource, &Entry)> = Vec::with_capacity(BATCH + 1);
            for (source, entry) in &self.entries {
                if first.cwd != source.cwd
                    || first.repository.provider != source.repository.provider
                    || first.repository.host != source.repository.host
                    || !eligible(source, entry)
                {
                    continue;
                }
                if let Some(target) = targets
                    .iter_mut()
                    .find(|(other, _)| same_target(other, source))
                {
                    if entry.due < target.1.due {
                        *target = (source, entry);
                    }
                } else {
                    targets.push((source, entry));
                }
                targets.sort_unstable_by(|(left, left_entry), (right, right_entry)| {
                    left_entry
                        .due
                        .cmp(&right_entry.due)
                        .then_with(|| left.cmp(right))
                });
                targets.truncate(BATCH);
            }
            // Prioritize due subscribers before optional same-target consumers.
            // Removing the latest candidate keeps storage bounded and ensures a
            // partial shared-target batch cannot starve its oldest due consumer.
            let mut subscribers = BinaryHeap::new();
            let mut bytes = 0;
            for (source, entry) in &self.entries {
                if entry.admission_waiting
                    || !targets
                        .iter()
                        .any(|(target, _)| same_target(target, source))
                {
                    continue;
                }
                let Some(charge) = entry
                    .request_bytes
                    .filter(|bytes| *bytes <= REQUEST_WORK_BYTES)
                else {
                    continue;
                };
                subscribers.push((!eligible(source, entry), entry.due, source, charge));
                bytes += charge;
                while bytes > REQUEST_WORK_BYTES {
                    let (_, _, _, charge) = subscribers.pop().expect("subscriber inserted");
                    bytes -= charge;
                }
            }
            let mut subscribers = subscribers.into_vec();
            subscribers.sort_unstable_by(|left, right| left.2.cmp(right.2));
            subscribers
                .into_iter()
                .map(|(_, _, source, _)| source.clone())
                .collect()
        }
        fn finish(
            &mut self,
            sources: &[CiSource],
            mut response: Result<BatchResponse, FetchError>,
        ) {
            let Some(context_source) = sources
                .iter()
                .find(|source| self.entries.contains_key(*source))
            else {
                return;
            };
            if self.backoff(context_source).is_none() {
                return;
            }
            self.version = self.version.wrapping_add(1);
            let now = Instant::now();
            let error = response.as_ref().err().or_else(|| {
                response
                    .as_ref()
                    .ok()
                    .and_then(|batch| batch.global_error.as_ref())
            });
            let backoff = self.backoff_mut(context_source);
            if let Some(error) = error {
                backoff.failures = backoff.failures.saturating_add(1);
                let deadline = now + Duration::from_secs(error.delay(backoff.failures));
                match error.retry {
                    RetryPolicy::RateLimited => {
                        backoff.hard =
                            Some(backoff.hard.map_or(deadline, |hard| hard.max(deadline)))
                    }
                    RetryPolicy::Normal | RetryPolicy::Authentication => {
                        backoff.soft = Some(deadline)
                    }
                }
            } else {
                backoff.failures = 0;
                backoff.soft = None;
            }
            if let Ok(batch) = &response
                && let Some(delay) = batch.retry_after
            {
                if let Some(deadline) =
                    now.checked_add(Duration::from_secs(delay.max(ACTIVE_SECONDS)))
                {
                    backoff.hard = Some(backoff.hard.map_or(deadline, |hard| hard.max(deadline)));
                } else {
                    // Never shorten a provider's hard retry interval to fit the
                    // host clock. This context stays blocked until retirement.
                    backoff.blocked = true;
                }
            }
            let hard = backoff.hard.unwrap_or(now);
            let blocked = backoff.blocked;
            for source in sources {
                let Some(entry) = self.entries.get_mut(source) else {
                    continue;
                };
                let result = match &mut response {
                    Err(error) => Err(error.clone()),
                    Ok(batch) => batch.results.remove(source).unwrap_or_else(|| {
                        Err(FetchError::normal(
                            "Provider omitted a requested repository result",
                        ))
                    }),
                };
                entry.eager = false;
                match result {
                    Ok(snapshot) => {
                        entry.due = if final_snapshot(&snapshot) {
                            None
                        } else {
                            Some(now + Duration::from_secs(ACTIVE_SECONDS))
                        };
                        entry.snapshot = snapshot;
                    }
                    Err(error) => {
                        let snapshot = &mut entry.snapshot;
                        snapshot.stale = snapshot.updated_at != 0;
                        snapshot.fetch_state = CiFetchState::Unavailable;
                        snapshot.error = Some(error.message);
                        let delay = match error.retry {
                            RetryPolicy::Authentication => 1800,
                            RetryPolicy::RateLimited => 3600,
                            RetryPolicy::Normal => ERROR_RETRY_SECONDS,
                        };
                        entry.due = Some(hard.max(now + Duration::from_secs(delay)));
                    }
                }
            }
            if blocked {
                for (source, entry) in &mut self.entries {
                    if source.cwd == context_source.cwd
                        && source.repository.provider == context_source.repository.provider
                        && source.repository.host == context_source.repository.host
                    {
                        block_entry(entry);
                    }
                }
            }
        }
    }

    fn matches_target(snapshot: &CiSnapshot, target: &CiRefreshTarget) -> bool {
        snapshot.repository == target.repository
            && snapshot.local_head_sha == target.local_head_sha
            && snapshot.checked_ref == target.checked_ref
            && snapshot.head_sha == target.head_sha
    }

    fn final_snapshot(snapshot: &CiSnapshot) -> bool {
        !snapshot.stale
            && !snapshot.truncated
            && if snapshot.commit_missing {
                snapshot.fetch_state == CiFetchState::Unavailable && snapshot.checks.is_empty()
            } else {
                snapshot.fetch_state == CiFetchState::Ready
                    && snapshot.error.is_none()
                    && snapshot.checks.iter().all(|check| {
                        matches!(
                            check.status,
                            CiStatus::Passed
                                | CiStatus::Failed
                                | CiStatus::Cancelled
                                | CiStatus::Skipped
                        )
                    })
            }
    }

    fn refreshing(snapshot: &mut CiSnapshot) {
        snapshot.fetch_state = CiFetchState::Loading;
        if snapshot.updated_at == 0 {
            snapshot.stale = false;
            // Cold retries retain a genuine prior provider error until success.
            return;
        }
        // Job status remains last-known; fetch status must never be inferred from Pending/Running.
        snapshot.stale = true;
    }
    fn timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
    fn scratch_file() -> std::io::Result<File> {
        tempfile::tempfile()
    }
    fn start(sources: &[CiSource]) -> std::io::Result<Pending> {
        let (mut command, payload) = match sources[0].repository.provider {
            ForgeProvider::Github => github::request(sources)?,
        };
        let stdout = scratch_file()?;
        let stderr = scratch_file()?;
        let mut stdin = scratch_file()?;
        stdin.write_all(&payload)?;
        stdin.seek(SeekFrom::Start(0))?;
        command
            .stdin(Stdio::from(stdin))
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let limit = libc::rlimit {
                    rlim_cur: 4 * 1024 * 1024,
                    rlim_max: 4 * 1024 * 1024,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Pending {
            child,
            stdout,
            stderr,
            sources: sources.to_vec(),
            deadline: Instant::now() + Duration::from_secs(DEADLINE_SECONDS),
            group_retired: false,
            waited: false,
        })
    }
    fn response(pending: &mut Pending) -> Result<BatchResponse, FetchError> {
        let status = pending
            .wait()
            .map_err(|_| FetchError::normal("CI CLI process could not be reaped"))?;
        pending
            .stdout
            .seek(SeekFrom::Start(0))
            .map_err(|_| FetchError::normal("Cannot read provider response"))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut pending.stdout)
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| FetchError::normal("Cannot read provider response"))?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(FetchError::normal("Provider response exceeds 2 MiB limit"));
        }
        let mut stderr = String::new();
        if !status.success() {
            pending
                .stderr
                .seek(SeekFrom::Start(0))
                .map_err(|_| FetchError::normal("Cannot read provider error"))?;
            let _ = Read::by_ref(&mut pending.stderr)
                .take(8192)
                .read_to_string(&mut stderr);
        }
        match pending.sources[0].repository.provider {
            ForgeProvider::Github => github::response(
                status.success(),
                &bytes,
                &stderr,
                &pending.sources,
                pending.sources.iter(),
                timestamp(),
            ),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::{Value, json};
        use std::process::Command;
        struct Repository(PathBuf);
        impl Repository {
            fn new() -> Self {
                let directory = tempfile::tempdir().unwrap();
                Self(directory.keep())
            }
            fn git(&self, args: &[&str]) {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&self.0)
                    .args([
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        impl Drop for Repository {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        impl Monitor {
            fn finish_fixture(
                &mut self,
                sources: &[CiSource],
                response: Result<Value, FetchError>,
            ) {
                let batch = response
                    .map(|value| github::normalize(value, sources, sources.iter(), timestamp()));
                self.finish(sources, batch);
            }
            pub(crate) fn finish_json_fixture(&mut self, sources: &[CiSource], value: Value) {
                self.finish_fixture(sources, Ok(value));
            }
        }
        fn source(sha: &str) -> CiSource {
            CiSource {
                cwd: "/project".into(),
                generation: 0,
                repository: github::repository("https://github.com/owner/repo").unwrap(),
                sha: sha.into(),
                branch: Some("main".into()),
                remote_branch: None,
            }
        }

        fn branch_source(local_sha: &str, branch: &str) -> CiSource {
            let mut source = source(local_sha);
            source.remote_branch = Some(RemoteBranch {
                remote: "origin".into(),
                name: branch.into(),
                tracking_sha: None,
            });
            source
        }

        #[test]
        fn discovery_keeps_the_canonical_item_directory_not_the_repository_root() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "main"]);
            repository.git(&["commit", "--allow-empty", "-m", "initial"]);
            repository.git(&["remote", "add", "origin", "https://github.com/owner/repo"]);
            let nested = repository.0.join("nested");
            std::fs::create_dir(&nested).unwrap();
            let alias = repository.0.join("alias");
            std::os::unix::fs::symlink(&nested, &alias).unwrap();
            let direct = discover(&nested, Some("main")).unwrap();
            let linked = discover(&alias, Some("main")).unwrap();
            assert_eq!(direct, linked);
            assert_eq!(direct.cwd, std::fs::canonicalize(&nested).unwrap());
            assert_ne!(direct.cwd, std::fs::canonicalize(&repository.0).unwrap());
        }

        #[test]
        fn discovery_resolves_upstream_push_remote_and_explicit_branch_mappings() {
            let repository = Repository::new();
            repository.git(&["init", "-b", "local-work"]);
            repository.git(&["commit", "--allow-empty", "-m", "published"]);
            repository.git(&["remote", "add", "origin", "https://github.com/owner/repo"]);
            repository.git(&["remote", "add", "fork", "https://github.com/fork/repo"]);
            repository.git(&["config", "branch.local-work.remote", "origin"]);
            repository.git(&[
                "config",
                "branch.local-work.merge",
                "refs/heads/remote-work",
            ]);
            repository.git(&["update-ref", "refs/remotes/origin/remote-work", "HEAD"]);
            let published = discover(&repository.0, Some("local-work")).unwrap();
            assert_eq!(
                published.checked_ref().as_deref(),
                Some("origin/remote-work")
            );
            assert_eq!(
                published
                    .remote_branch
                    .as_ref()
                    .unwrap()
                    .tracking_sha
                    .as_ref(),
                Some(&published.sha)
            );
            repository.git(&["commit", "--allow-empty", "-m", "unpushed"]);
            let local = discover(&repository.0, Some("local-work")).unwrap();
            assert_ne!(local.sha, published.sha);
            assert_eq!(local.remote_branch, published.remote_branch);

            repository.git(&["config", "remote.pushDefault", "fork"]);
            let fork = discover(&repository.0, Some("local-work")).unwrap();
            assert_eq!(fork.repository.path, "fork/repo");
            assert_eq!(fork.checked_ref().as_deref(), Some("fork/local-work"));
            repository.git(&["config", "branch.local-work.pushRemote", "origin"]);
            assert_eq!(
                discover(&repository.0, Some("local-work"))
                    .unwrap()
                    .repository
                    .path,
                "owner/repo"
            );
            repository.git(&[
                "config",
                "remote.origin.push",
                "+refs/heads/local-work:refs/heads/published-work",
            ]);
            assert_eq!(
                discover(&repository.0, Some("local-work"))
                    .unwrap()
                    .checked_ref()
                    .as_deref(),
                Some("origin/published-work")
            );
            repository.git(&[
                "config",
                "remote.origin.push",
                "refs/heads/*:refs/heads/review/*",
            ]);
            assert_eq!(
                discover(&repository.0, Some("local-work"))
                    .unwrap()
                    .checked_ref()
                    .as_deref(),
                Some("origin/review/local-work")
            );
            repository.git(&["config", "--unset", "remote.origin.push"]);
            repository.git(&["config", "push.default", "current"]);
            assert_eq!(
                discover(&repository.0, Some("local-work"))
                    .unwrap()
                    .checked_ref()
                    .as_deref(),
                Some("origin/local-work")
            );
            repository.git(&["checkout", "--detach"]);
            let detached = discover(&repository.0, Some("detached @ abc")).unwrap();
            assert!(detached.remote_branch.is_none());
            assert!(detached.branch.is_none());
            assert_eq!(detached.sha, local.sha);
        }

        #[test]
        fn batches_share_remote_targets_not_local_commits() {
            let first = branch_source("local-a", "published");
            let mut linked = branch_source("local-b", "published");
            linked.branch = Some("linked-local".into());
            let other = branch_source("local-a", "other");
            assert!(same_target(&first, &linked));
            assert!(!same_target(&first, &other));
            let mut monitor = Monitor::new();
            monitor.sync([first.clone(), linked.clone(), other.clone()].into_iter());
            monitor.entries.get_mut(&other).unwrap().due = None;
            let batch = monitor.due_batch(Instant::now());
            assert_eq!(batch.len(), 2);
            assert!(batch.contains(&first));
            assert!(batch.contains(&linked));
            let mut snapshot = complete_snapshot(&first, &[CiStatus::Passed]);
            snapshot.head_sha = "remote-tip".into();
            finish_snapshot(&mut monitor, &first, snapshot);
            for source in [&first, &linked] {
                let snapshot = monitor.snapshot(source).unwrap();
                assert_eq!(snapshot.head_sha, "remote-tip");
                assert_eq!(snapshot.local_head_sha, source.sha);
                assert_eq!(snapshot.checked_ref.as_deref(), Some("origin/published"));
            }
            assert!(monitor.entries[&first].due.is_none());
            monitor.refresh(&target(&first));
            assert!(monitor.entries[&first].due.is_none());
            monitor.refresh(&CiRefreshTarget {
                head_sha: "remote-tip".into(),
                ..target(&first)
            });
            assert!(monitor.entries[&first].due.is_some());
            assert!(monitor.entries[&linked].due.is_some());
            assert!(monitor.entries[&other].due.is_none());
        }

        #[test]
        fn cwd_contexts_split_batches_and_results_but_keep_shared_request_spacing() {
            let first = source("same-head");
            let another = source("another-head");
            let mut other_cwd = first.clone();
            other_cwd.cwd = "/second-project".into();
            assert!(!same_target(&first, &other_cwd));
            let mut monitor = Monitor::new();
            monitor.sync([first.clone(), another.clone(), other_cwd.clone()].into_iter());
            let now = Instant::now();
            let batch = monitor.reserve_batch(now);
            assert_eq!(batch.len(), 2);
            assert!(batch.contains(&first));
            assert!(batch.contains(&another));
            assert!(!batch.contains(&other_cwd));
            finish_snapshot(
                &mut monitor,
                &first,
                complete_snapshot(&first, &[CiStatus::Passed]),
            );
            finish_snapshot(
                &mut monitor,
                &another,
                complete_snapshot(&another, &[CiStatus::Passed]),
            );
            assert_eq!(monitor.snapshot(&other_cwd), Some(empty(&other_cwd, 0)));
            assert!(monitor.reserve_batch(now).is_empty());
            assert_eq!(
                monitor.reserve_batch(now + Duration::from_secs(MIN_REQUEST_SECONDS)),
                vec![other_cwd.clone()]
            );
            finish_snapshot(
                &mut monitor,
                &other_cwd,
                complete_snapshot(&other_cwd, &[CiStatus::Failed]),
            );
            assert_eq!(monitor.snapshot(&first).unwrap().status, CiStatus::Passed);
            assert_eq!(
                monitor.snapshot(&other_cwd).unwrap().status,
                CiStatus::Failed
            );
        }

        #[test]
        fn a_new_cwd_cannot_reuse_an_existing_request_or_its_cached_result() {
            let first = source("same-head");
            let mut second = first.clone();
            second.cwd = "/second-project".into();
            let mut monitor = Monitor::new();
            monitor.sync([first.clone()].into_iter());
            monitor.pending = Some(pending_fixture(std::slice::from_ref(&first)));
            let fd = monitor.request_pid();
            monitor.sync([first.clone(), second.clone()].into_iter());
            assert_eq!(monitor.request_pid(), fd);
            assert!(!same_target(
                &monitor.pending.as_ref().unwrap().sources[0],
                &second
            ));
            finish_snapshot(
                &mut monitor,
                &first,
                complete_snapshot(&first, &[CiStatus::Passed]),
            );
            assert_eq!(monitor.snapshot(&second), Some(empty(&second, 0)));
            monitor.pending = None;
            assert_eq!(monitor.due_batch(Instant::now()), vec![second.clone()]);
            monitor.sync([second.clone()].into_iter());
            assert_eq!(monitor.snapshot(&second), Some(empty(&second, 0)));
        }

        #[test]
        fn remote_tracking_changes_retire_pending_generations_and_wrong_cache_targets() {
            let old = branch_source("local", "published");
            let mut monitor = Monitor::new();
            monitor.sync([old.clone()].into_iter());
            monitor.pending = Some(pending_fixture(std::slice::from_ref(&old)));
            let mut current = old.clone();
            current.remote_branch.as_mut().unwrap().tracking_sha = Some("pushed".into());
            monitor.sync([current.clone()].into_iter());
            assert_eq!(monitor.request_pid(), -1);
            let mut snapshot = complete_snapshot(&current, &[CiStatus::Passed]);
            snapshot.head_sha = "provider-newer-than-tracking".into();
            for mismatch in 0..3 {
                let mut wrong = snapshot.clone();
                match mismatch {
                    0 => wrong.local_head_sha = "other-local".into(),
                    1 => wrong.checked_ref = Some("origin/other".into()),
                    _ => wrong.local_head_sha.clear(),
                }
                monitor.restore_published(&current, wrong, None);
                assert!(monitor.snapshot(&current).unwrap().head_sha.is_empty());
            }
            monitor.restore_published(&current, snapshot.clone(), None);
            assert_eq!(monitor.snapshot(&current), Some(snapshot));
            assert_eq!(monitor.timeout(), -1);
        }

        fn pull_request(source: &CiSource, number: u64) -> Value {
            json!({
                "number": number,
                "title": format!("Request {number}"),
                "url": format!("https://github.com/owner/repo/pull/{number}"),
                "state": "OPEN",
                "isDraft": false,
                "headRefName": source.branch,
                "headRefOid": source.sha,
                "headRepository": {"nameWithOwner": source.repository.path},
                "body": "",
                "bodyHTML": "",
                "comments": {"nodes": []}
            })
        }

        fn pending_fixture(sources: &[CiSource]) -> Pending {
            let mut command = Command::new("sleep");
            command.arg("60");
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            Pending {
                child,
                stdout: scratch_file().unwrap(),
                stderr: scratch_file().unwrap(),
                sources: sources.to_vec(),
                deadline: Instant::now() + Duration::from_secs(60),
                group_retired: false,
                waited: false,
            }
        }

        fn complete_snapshot(source: &CiSource, statuses: &[CiStatus]) -> CiSnapshot {
            let mut snapshot = empty(source, 123);
            snapshot.fetch_state = CiFetchState::Ready;
            snapshot.status = CiStatus::aggregate(statuses.iter().copied());
            snapshot.checks = statuses
                .iter()
                .enumerate()
                .map(|(index, status)| bkpr_protocol::CiCheck {
                    name: format!("check-{index}"),
                    status: *status,
                    url: None,
                    duration_seconds: None,
                })
                .collect();
            snapshot
        }

        fn finish_snapshot(monitor: &mut Monitor, source: &CiSource, snapshot: CiSnapshot) {
            let sources: Vec<_> = monitor
                .entries
                .keys()
                .filter(|candidate| same_target(candidate, source))
                .cloned()
                .collect();
            let results = sources
                .iter()
                .map(|candidate| {
                    let mut snapshot = snapshot.clone();
                    snapshot.branch = candidate.branch.clone();
                    snapshot.local_head_sha.clone_from(&candidate.sha);
                    snapshot.checked_ref = candidate.checked_ref();
                    (candidate.clone(), Ok(snapshot))
                })
                .collect();
            monitor.finish(
                &sources,
                Ok(BatchResponse {
                    results,
                    global_error: None,
                    retry_after: None,
                }),
            );
        }

        fn target(source: &CiSource) -> CiRefreshTarget {
            CiRefreshTarget {
                pane_id: String::new(),
                cwd: source.cwd.clone(),
                local_head_sha: source.sha.clone(),
                checked_ref: source.checked_ref(),
                repository: source.repository.clone(),
                head_sha: source.sha.clone(),
            }
        }

        #[test]
        fn cold_sources_use_short_spacing_without_accelerating_existing_poll_timers() {
            for status in [CiStatus::Running, CiStatus::Unknown, CiStatus::Passed] {
                for new_repository in [false, true] {
                    let mut monitor = Monitor::new();
                    let old = source("existing");
                    monitor.sync([old.clone()].into_iter());
                    let first = Instant::now();
                    assert_eq!(monitor.reserve_batch(first), vec![old.clone()]);
                    finish_snapshot(&mut monitor, &old, complete_snapshot(&old, &[status]));
                    let old_due = monitor.entries[&old].due;
                    let spacing = first + Duration::from_secs(MIN_REQUEST_SECONDS);
                    assert_eq!(monitor.next_request, spacing);

                    let mut cold = source("cold");
                    cold.branch = Some("new-worktree".into());
                    if new_repository {
                        cold.repository =
                            github::repository("https://github.com/owner/another").unwrap();
                    }
                    monitor.sync([old.clone(), cold.clone()].into_iter());
                    assert!(monitor.timeout() <= 2_000);
                    assert!(monitor.reserve_batch(first).is_empty());
                    assert_eq!(monitor.reserve_batch(spacing), vec![cold.clone()]);
                    finish_snapshot(
                        &mut monitor,
                        &cold,
                        complete_snapshot(&cold, &[CiStatus::Passed]),
                    );
                    for _ in 0..4 {
                        monitor.sync([old.clone(), cold.clone()].into_iter());
                        assert_eq!(monitor.entries[&old].due, old_due);
                    }
                    assert!(
                        monitor
                            .reserve_batch(spacing + Duration::from_secs(MIN_REQUEST_SECONDS))
                            .is_empty()
                    );
                    assert_eq!(monitor.requests, 2);
                    if let Some(due) = old_due {
                        let interval = ACTIVE_SECONDS;
                        assert!(due >= first + Duration::from_secs(interval));
                        assert_eq!(monitor.due_batch(due), vec![old]);
                    } else {
                        assert_eq!(monitor.timeout(), -1);
                    }
                }
            }
        }

        #[test]
        fn head_changes_carry_only_same_branch_open_pr_metadata_not_commit_results() {
            for state in ["open", "draft", "closed", "merged"] {
                for identity_change in 0..5 {
                    let mut monitor = Monitor::new();
                    let mut old = source("old");
                    if identity_change == 3 {
                        old.branch = None;
                    }
                    monitor.sync([old.clone()].into_iter());
                    let mut previous = complete_snapshot(&old, &[CiStatus::Passed]);
                    previous.change_request = Some(
                        serde_json::from_value(json!({
                            "number": 42,
                            "title": "Existing request",
                            "url": "https://github.com/owner/repo/pull/42",
                            "state": state,
                            "issues": [{
                                "provider": "linear",
                                "identifier": "ENG-42",
                                "title": null,
                                "url": "https://linear.app/team/issue/ENG-42"
                            }]
                        }))
                        .unwrap(),
                    );
                    finish_snapshot(&mut monitor, &old, previous.clone());
                    let mut new = old.clone();
                    new.sha = "new".into();
                    match identity_change {
                        1 => new.branch = Some("another-branch".into()),
                        2 => {
                            new.repository =
                                github::repository("https://github.com/owner/another").unwrap();
                        }
                        4 => new.cwd = "/other-project".into(),
                        _ => {}
                    }
                    monitor.sync([new.clone()].into_iter());
                    finish_snapshot(&mut monitor, &old, previous.clone());
                    let current = monitor.snapshot(&new).unwrap();
                    assert_eq!(current.head_sha, new.sha);
                    assert_eq!(current.status, CiStatus::Unknown);
                    assert!(current.checks.is_empty());
                    assert_eq!(current.fetch_state, CiFetchState::Loading);
                    assert_eq!(monitor.due_batch(Instant::now()), vec![new]);
                    if identity_change == 0 && matches!(state, "open" | "draft") {
                        assert_eq!(current.change_request, previous.change_request);
                        assert_eq!(current.updated_at, previous.updated_at);
                        assert!(current.stale);
                        assert!(current.error.is_none());
                    } else {
                        assert!(current.change_request.is_none());
                        assert_eq!(current.updated_at, 0);
                        assert!(!current.stale);
                    }
                }
            }
        }

        #[test]
        fn only_complete_known_terminal_check_sets_stop_the_clock() {
            let source = source("final");
            for statuses in [
                vec![CiStatus::Passed],
                vec![CiStatus::Passed, CiStatus::Skipped],
                vec![CiStatus::Failed],
                vec![CiStatus::Cancelled],
                vec![CiStatus::Passed, CiStatus::Failed, CiStatus::Skipped],
                vec![],
            ] {
                let mut monitor = Monitor::new();
                monitor.sync([source.clone()].into_iter());
                finish_snapshot(&mut monitor, &source, complete_snapshot(&source, &statuses));
                assert!(monitor.entries[&source].due.is_none());
                assert_eq!(monitor.timeout(), -1);
                monitor.sync([source.clone()].into_iter());
                assert!(
                    monitor
                        .due_batch(Instant::now() + Duration::from_secs(86400 * 365))
                        .is_empty()
                );
                assert!(!monitor.snapshot(&source).unwrap().stale);
            }
            for statuses in [
                vec![CiStatus::Unknown],
                vec![CiStatus::NoChecks],
                vec![CiStatus::Failed, CiStatus::Running],
                vec![CiStatus::Failed, CiStatus::Pending],
            ] {
                let mut monitor = Monitor::new();
                monitor.sync([source.clone()].into_iter());
                finish_snapshot(&mut monitor, &source, complete_snapshot(&source, &statuses));
                let due = monitor.entries[&source].due.unwrap();
                assert!(due > Instant::now());
                assert!(monitor.timeout() >= 0);
                assert!(due <= Instant::now() + Duration::from_secs(ACTIVE_SECONDS));
            }
            for variant in 0..4 {
                let mut snapshot = complete_snapshot(&source, &[CiStatus::Passed]);
                match variant {
                    0 => snapshot.truncated = true,
                    1 => snapshot.stale = true,
                    2 => snapshot.error = Some("partial data".into()),
                    _ => snapshot.fetch_state = CiFetchState::Unavailable,
                }
                let mut monitor = Monitor::new();
                monitor.sync([source.clone()].into_iter());
                finish_snapshot(&mut monitor, &source, snapshot);
                assert!(monitor.entries[&source].due.is_some());
            }
        }

        #[test]
        fn restored_current_results_freeze_only_fresh_terminal_results() {
            let source = source("cached");
            for statuses in [
                vec![],
                vec![CiStatus::Passed, CiStatus::Skipped],
                vec![CiStatus::Failed],
                vec![CiStatus::Cancelled],
                vec![CiStatus::Running],
            ] {
                for variant in 0..5 {
                    let mut monitor = Monitor::new();
                    monitor.sync([source.clone()].into_iter());
                    let mut snapshot = complete_snapshot(&source, &statuses);
                    match variant {
                        1 => snapshot.stale = true,
                        2 => snapshot.error = Some("cached error".into()),
                        3 => snapshot.truncated = true,
                        4 => snapshot.fetch_state = CiFetchState::Unavailable,
                        _ => {}
                    }
                    let frozen = variant == 0
                        && statuses.iter().all(|status| {
                            matches!(
                                status,
                                CiStatus::Passed
                                    | CiStatus::Failed
                                    | CiStatus::Cancelled
                                    | CiStatus::Skipped
                            )
                        });
                    monitor.restore_published(&source, snapshot.clone(), None);
                    assert_eq!(monitor.snapshot(&source), Some(snapshot));
                    assert_eq!(monitor.entries[&source].due.is_none(), frozen);
                    assert_eq!(monitor.due_batch(Instant::now()).is_empty(), frozen);
                }
            }
            for mismatch in 0..3 {
                let mut monitor = Monitor::new();
                monitor.sync([source.clone()].into_iter());
                let mut cached = complete_snapshot(&source, &[CiStatus::Passed]);
                match mismatch {
                    0 => cached.head_sha = "other".into(),
                    1 => cached.branch = Some("other".into()),
                    _ => {
                        cached.repository =
                            github::repository("https://github.com/owner/another").unwrap();
                    }
                }
                monitor.restore_published(&source, cached, None);
                assert_eq!(monitor.snapshot(&source), Some(empty(&source, 0)));
            }
        }

        #[test]
        fn unpublished_commits_stay_idle_until_explicit_refresh() {
            let source = source("unpublished");
            let mut monitor = Monitor::new();
            monitor.sync([source.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&source),
                Ok(json!({
                    "data": {
                        "r0": {
                            "object": null,
                            "b0": {"associatedPullRequests": {
                                "nodes": [], "pageInfo": {"hasNextPage": false}
                            }},
                            "c0": {"nodes": []}
                        },
                        "rateLimit": {"remaining": 5000}
                    }
                })),
            );
            let unpublished = monitor.snapshot(&source).unwrap();
            assert!(unpublished.commit_missing);
            assert_eq!(unpublished.fetch_state, CiFetchState::Unavailable);
            assert!(unpublished.checks.is_empty());
            assert_eq!(monitor.timeout(), -1);
            monitor.sync([source.clone()].into_iter());
            assert_eq!(monitor.timeout(), -1);

            let mut restored = Monitor::new();
            restored.sync([source.clone()].into_iter());
            restored.restore_published(&source, unpublished.clone(), None);
            assert_eq!(restored.snapshot(&source), Some(unpublished));
            assert_eq!(restored.timeout(), -1);
            restored.refresh(&target(&source));
            assert_eq!(restored.reserve_batch(Instant::now()), vec![source]);
        }

        #[test]
        fn cold_loads_bypass_unrelated_soft_errors_but_never_hard_provider_limits() {
            let mut monitor = Monitor::new();
            let old = source("old");
            let cold = source("cold");
            monitor.sync([old.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&old),
                Err(FetchError {
                    message: "Repository authentication unavailable".into(),
                    retry: RetryPolicy::Authentication,
                }),
            );
            monitor.sync([old.clone(), cold.clone()].into_iter());
            let now = Instant::now();
            assert_eq!(monitor.reserve_batch(now), vec![cold.clone()]);
            assert!(monitor.entries[&old].due.unwrap() > now);
            let mut next = source("next");
            next.branch = Some("another".into());
            monitor.sync([old, cold, next.clone()].into_iter());
            monitor.backoff_mut(&next).hard = Some(now + Duration::from_secs(3600));
            assert!(
                monitor
                    .reserve_batch(now + Duration::from_secs(2))
                    .is_empty()
            );
            assert!(monitor.timeout() >= 3_599_000);
        }

        #[test]
        fn manual_refresh_reports_progress_without_inventing_a_provider_error() {
            let mut monitor = Monitor::new();
            let source = source("refresh");
            monitor.sync([source.clone()].into_iter());
            let snapshot = complete_snapshot(&source, &[]);
            finish_snapshot(&mut monitor, &source, snapshot.clone());

            monitor.refresh(&target(&source));
            let loading = monitor.snapshot(&source).unwrap();
            assert_eq!(loading.fetch_state, CiFetchState::Loading);
            assert_eq!(loading.status, CiStatus::NoChecks);
            assert!(loading.stale);
            assert_eq!(loading.updated_at, snapshot.updated_at);
            assert!(loading.error.is_none());

            monitor.finish(
                std::slice::from_ref(&source),
                Err(FetchError::normal("offline")),
            );
            monitor.refresh(&target(&source));
            let retrying = monitor.snapshot(&source).unwrap();
            assert_eq!(retrying.fetch_state, CiFetchState::Loading);
            assert_eq!(retrying.error.as_deref(), Some("offline"));

            finish_snapshot(&mut monitor, &source, snapshot);
            let ready = monitor.snapshot(&source).unwrap();
            assert_eq!(ready.fetch_state, CiFetchState::Ready);
            assert!(!ready.stale);
            assert!(ready.error.is_none());
        }

        #[test]
        fn manual_refresh_shares_commit_coalesces_and_returns_to_frozen_after_rerun() {
            for status in [CiStatus::Passed, CiStatus::Failed, CiStatus::Cancelled] {
                let mut monitor = Monitor::new();
                let source = source("same");
                let mut linked = source.clone();
                linked.branch = Some("linked".into());
                monitor.sync([source.clone(), linked.clone()].into_iter());
                let snapshot = complete_snapshot(&source, &[status]);
                finish_snapshot(&mut monitor, &source, snapshot.clone());
                assert!(monitor.entries.values().all(|entry| entry.due.is_none()));
                monitor.next_request = Instant::now() + Duration::from_secs(MIN_REQUEST_SECONDS);
                let budget = monitor.next_request;
                monitor.refresh(&target(&source));
                let due = monitor.entries[&source].due;
                for _ in 0..8 {
                    monitor.refresh(&target(&source));
                    assert!(
                        monitor
                            .reserve_batch(budget - Duration::from_secs(1))
                            .is_empty()
                    );
                    assert_eq!(monitor.entries[&source].due, due);
                }
                assert_eq!(monitor.requests, 0);
                assert_eq!(monitor.next_request, budget);
                assert_eq!(monitor.due_batch(budget).len(), 2);
                for subscriber in [&source, &linked] {
                    let current = monitor.snapshot(subscriber).unwrap();
                    assert_eq!(current.fetch_state, CiFetchState::Loading);
                    assert_eq!(current.checks, snapshot.checks);
                    assert!(current.stale);
                    assert!(monitor.entries[subscriber].eager);
                }
                finish_snapshot(
                    &mut monitor,
                    &source,
                    complete_snapshot(&source, &[CiStatus::Running]),
                );
                assert!(monitor.entries[&source].due.is_some());
                assert!(!monitor.entries[&source].eager);
                finish_snapshot(&mut monitor, &source, snapshot);
                assert!(monitor.entries.values().all(|entry| entry.due.is_none()));
                assert_eq!(monitor.timeout(), -1);
            }
        }

        #[test]
        fn manual_refresh_bypasses_soft_retries_but_not_hard_provider_or_minimum_spacing() {
            for retry in [
                RetryPolicy::Authentication,
                RetryPolicy::Normal,
                RetryPolicy::RateLimited,
            ] {
                let mut monitor = Monitor::new();
                let source = source("retry");
                monitor.sync([source.clone()].into_iter());
                monitor.next_request = Instant::now() + Duration::from_secs(MIN_REQUEST_SECONDS);
                monitor.finish(
                    std::slice::from_ref(&source),
                    Err(FetchError {
                        message: "offline".into(),
                        retry,
                    }),
                );
                let hard = monitor.request_deadline(&source);
                monitor.refresh(&target(&source));
                assert_eq!(monitor.request_deadline(&source), hard);
                assert_eq!(
                    monitor.snapshot(&source).unwrap().fetch_state,
                    CiFetchState::Loading
                );
                let before_spacing = monitor.next_request - Duration::from_secs(1);
                assert!(monitor.reserve_batch(before_spacing).is_empty());
                assert_eq!(monitor.requests, 0);
                if matches!(retry, RetryPolicy::RateLimited) {
                    assert!(monitor.timeout() >= 3_599_000);
                } else {
                    assert!(monitor.timeout() <= 2_000);
                    assert_eq!(monitor.due_batch(hard), vec![source]);
                }
            }
            let mut monitor = Monitor::new();
            let source = source("hard");
            monitor.sync([source.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&source),
                Ok(json!({
                    "data": {"r0": null, "rateLimit": {"remaining": 0}},
                    "_retry_after": 7200,
                })),
            );
            let hard = monitor.request_deadline(&source);
            monitor.refresh(&target(&source));
            assert_eq!(monitor.request_deadline(&source), hard);
            assert!(monitor.timeout() >= 7_199_000);
        }

        #[test]
        fn obsolete_refresh_and_provider_identities_cannot_change_the_current_head() {
            let mut monitor = Monitor::new();
            let old = source("old");
            let new = source("new");
            monitor.sync([old.clone()].into_iter());
            finish_snapshot(
                &mut monitor,
                &old,
                complete_snapshot(&old, &[CiStatus::Passed]),
            );
            monitor.sync([new.clone()].into_iter());
            finish_snapshot(
                &mut monitor,
                &new,
                complete_snapshot(&new, &[CiStatus::Passed]),
            );
            monitor.refresh(&target(&old));
            let mut forged = target(&new);
            forged.repository.host = "other.example".into();
            monitor.refresh(&forged);
            finish_snapshot(
                &mut monitor,
                &old,
                complete_snapshot(&old, &[CiStatus::Running]),
            );
            assert!(monitor.entries[&new].due.is_none());
            assert_eq!(monitor.snapshot(&new).unwrap().status, CiStatus::Passed);
            assert_eq!(monitor.timeout(), -1);
        }

        #[test]
        fn initial_metadata_is_loading_even_when_the_shared_budget_defers_io() {
            let mut monitor = Monitor::new();
            let source = source("abc");
            monitor.next_request = Instant::now() + Duration::from_secs(MIN_REQUEST_SECONDS);
            monitor.sync([source.clone()].into_iter());
            let before_spacing = monitor.next_request - Duration::from_secs(1);
            assert!(monitor.reserve_batch(before_spacing).is_empty());
            let snapshot = monitor.snapshot(&source).unwrap();
            assert_eq!(snapshot.fetch_state, CiFetchState::Loading);
            assert_eq!(snapshot.repository.web_url, "https://github.com/owner/repo");
            assert_eq!(snapshot.head_sha, "abc");
            assert!(snapshot.checks.is_empty());
            assert_eq!(monitor.requests, 0);
        }

        #[test]
        fn refresh_and_failure_keep_last_known_results_without_confusing_job_and_fetch_state() {
            for status in [CiStatus::Passed, CiStatus::Pending, CiStatus::Running] {
                let mut monitor = Monitor::new();
                let source = source("abc");
                monitor.sync([source.clone()].into_iter());
                let mut snapshot = empty(&source, 123);
                snapshot.fetch_state = CiFetchState::Ready;
                snapshot.status = status;
                snapshot.checks.push(bkpr_protocol::CiCheck {
                    name: "test".into(),
                    status,
                    url: None,
                    duration_seconds: Some(42),
                });
                monitor.entries.get_mut(&source).unwrap().snapshot = snapshot.clone();
                monitor.refresh(&target(&source));
                let refreshing = monitor.snapshot(&source).unwrap();
                assert_eq!(refreshing.fetch_state, CiFetchState::Loading);
                assert_eq!(refreshing.status, status);
                assert!(refreshing.stale);
                monitor.finish(
                    std::slice::from_ref(&source),
                    Err(FetchError::normal("offline")),
                );
                let unavailable = monitor.snapshot(&source).unwrap();
                assert_eq!(unavailable.fetch_state, CiFetchState::Unavailable);
                assert_eq!(unavailable.status, status);
                assert_eq!(unavailable.checks, snapshot.checks);
                assert_eq!(unavailable.updated_at, 123);
                assert!(unavailable.stale);
            }
        }

        #[test]
        fn authentication_failures_bound_requests_and_keep_unknown_visible() {
            let mut monitor = Monitor::new();
            let source = source("abc");
            monitor.sync([source.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&source),
                Err(FetchError {
                    message: "GitHub authentication unavailable".into(),
                    retry: RetryPolicy::Authentication,
                }),
            );
            assert!(
                monitor
                    .soft_deadline(&source)
                    .unwrap()
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    >= 1799
            );
            assert_eq!(monitor.snapshot(&source).unwrap().status, CiStatus::Unknown);
            assert_eq!(
                monitor.snapshot(&source).unwrap().fetch_state,
                CiFetchState::Unavailable
            );
            assert!(!monitor.snapshot(&source).unwrap().stale);
            assert!(monitor.timeout() > 1_700_000);
        }

        #[test]
        fn completed_cache_is_shared_and_old_generations_are_not_applied() {
            let mut monitor = Monitor::new();
            let old = source("abc");
            let new = source("def");
            monitor.sync([old.clone()].into_iter());
            monitor.sync([new.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&old),
                Err(FetchError::normal("offline")),
            );
            assert_eq!(
                monitor.snapshot(&new).unwrap().fetch_state,
                CiFetchState::Loading
            );
            assert!(monitor.snapshot(&old).is_none());
            let mut linked = new.clone();
            linked.branch = Some("next".into());
            assert!(same_target(&new, &linked));
            let value = json!({
                "data": {
                    "r0": {
                        "object": {"oid": "def", "statusCheckRollup": null},
                        "b0": {
                            "associatedPullRequests": {
                                "nodes": [pull_request(&new, 101)],
                                "pageInfo": {"hasNextPage": false}
                            }
                        },
                        "c0": {"nodes": []},
                        "b1": {
                            "associatedPullRequests": {
                                "nodes": [pull_request(&linked, 102)],
                                "pageInfo": {"hasNextPage": false}
                            }
                        },
                        "c1": {"nodes": []}
                    },
                    "rateLimit": {"remaining": 5000}
                }
            });
            monitor.sync([new.clone(), linked.clone()].into_iter());
            let sources = monitor.due_batch(
                monitor
                    .soft_deadline(&new)
                    .unwrap_or_else(Instant::now)
                    .max(Instant::now()),
            );
            assert_eq!(sources, vec![new.clone(), linked.clone()]);
            monitor.finish_fixture(&sources, Ok(value));
            assert_eq!(monitor.snapshot(&new).unwrap().status, CiStatus::NoChecks);
            assert_eq!(
                monitor.snapshot(&linked).unwrap().status,
                CiStatus::NoChecks
            );
            for (subscriber, number) in [(&new, 101), (&linked, 102)] {
                let snapshot = monitor.snapshot(subscriber).unwrap();
                assert_eq!(snapshot.branch, subscriber.branch);
                assert_eq!(snapshot.change_request.unwrap().number, number);
            }
            assert!(monitor.entries[&new].due.is_none());
        }

        #[test]
        fn unchanged_subscriptions_keep_passing_ci_and_do_not_make_commit_due_again() {
            let mut monitor = Monitor::new();
            let old = source("old");
            monitor.sync([old.clone()].into_iter());
            let response = json!({
                "data": {
                    "r0": {
                        "object": {
                            "oid": "old",
                            "statusCheckRollup": {
                                "contexts": {
                                    "nodes": [{
                                        "__typename": "CheckRun", "name": "test",
                                        "status": "COMPLETED", "conclusion": "SUCCESS"
                                    }],
                                    "pageInfo": {"hasNextPage": false}
                                }
                            }
                        },
                        "b0": {"associatedPullRequests": {
                            "nodes": [], "pageInfo": {"hasNextPage": false}
                        }},
                        "c0": {"nodes": []}
                    },
                    "rateLimit": {"remaining": 5000}
                }
            });
            monitor.finish_fixture(std::slice::from_ref(&old), Ok(response.clone()));
            let due = monitor.entries[&old].due;
            assert!(due.is_none());
            for _ in 0..4 {
                monitor.sync([old.clone(), old.clone()].into_iter());
                assert!(monitor.reserve_batch(Instant::now()).is_empty());
                assert_eq!(monitor.snapshot(&old).unwrap().status, CiStatus::Passed);
                assert_eq!(monitor.entries[&old].due, due);
                assert_eq!(monitor.requests, 0);
                assert!(monitor.due_batch(Instant::now()).is_empty());
            }

            monitor.sync([old.clone()].into_iter());
            assert_eq!(monitor.snapshot(&old).unwrap().status, CiStatus::Passed);
            assert!(!monitor.snapshot(&old).unwrap().stale);
            assert_eq!(monitor.timeout(), -1);
            assert!(
                monitor
                    .due_batch(Instant::now() + Duration::from_secs(86400))
                    .is_empty()
            );
            let new = source("new");
            monitor.sync([new.clone()].into_iter());
            monitor.finish_fixture(std::slice::from_ref(&old), Ok(response));
            assert!(monitor.snapshot(&old).is_none());
            assert_eq!(
                monitor.snapshot(&new).unwrap().fetch_state,
                CiFetchState::Loading
            );
            assert_eq!(monitor.due_batch(Instant::now()), vec![new]);
        }

        #[test]
        fn rate_reset_and_http_retry_are_honored() {
            let mut monitor = Monitor::new();
            let source = source("abc");
            monitor.sync([source.clone()].into_iter());
            monitor.finish_fixture(std::slice::from_ref(&source), Ok(json!({"data":{"rateLimit":{"remaining":0,"cost":10,"resetAt":"2099-01-01T00:00:00Z"}},"_retry_after":7200})));
            assert!(
                monitor
                    .request_deadline(&source)
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    > 7200
            );
        }

        #[test]
        fn batching_deduplicates_commits_bounds_work_and_backoff_stops_launches() {
            let mut monitor = Monitor::new();
            let mut sources: Vec<_> = (0..20)
                .map(|index| source(&format!("sha{index}")))
                .collect();
            let mut linked = sources[0].clone();
            linked.branch = Some("linked".into());
            sources.push(linked.clone());
            monitor.sync(sources.into_iter());
            // A subscriber's own later timer must not exclude its branch metadata
            // when another subscriber has already made their shared commit due.
            monitor.entries.get_mut(&linked).unwrap().due =
                Some(Instant::now() + Duration::from_secs(ACTIVE_SECONDS));
            let batch = monitor.due_batch(Instant::now());
            assert_eq!(batch.len(), BATCH + 1);
            let commits: BTreeSet<_> = batch
                .iter()
                .map(|source| (&source.repository, &source.sha))
                .collect();
            assert_eq!(commits.len(), BATCH);
            assert_eq!(
                batch.iter().filter(|source| source.sha == "sha0").count(),
                2
            );
            monitor.finish_fixture(
                &batch,
                Err(FetchError {
                    message: "GitHub authentication unavailable".into(),
                    retry: RetryPolicy::Authentication,
                }),
            );
            monitor.sync(batch.into_iter());
            assert!(monitor.reserve_batch(Instant::now()).is_empty());
            assert_eq!(monitor.requests, 0);
            assert_eq!(monitor.request_pid(), -1);
        }

        #[test]
        fn provider_rate_errors_defer_the_whole_batch_not_only_the_reported_alias() {
            let mut monitor = Monitor::new();
            let sources: Vec<_> = (0..BATCH)
                .map(|index| source(&format!("sha{index}")))
                .collect();
            monitor.sync(sources.iter().cloned());
            monitor.finish_fixture(
                &sources,
                Ok(json!({
                    "data": null, "errors": [{"type": "RATE_LIMITED", "path": ["r0"]}]
                })),
            );
            assert!(
                monitor
                    .request_deadline(&sources[0])
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    >= 3599
            );
            for source in &sources {
                let snapshot = monitor.snapshot(source).unwrap();
                assert_eq!(snapshot.fetch_state, CiFetchState::Unavailable);
                assert_eq!(snapshot.status, CiStatus::Unknown);
                assert!(snapshot.error.is_some());
            }
            assert!(monitor.reserve_batch(Instant::now()).is_empty());
            assert_eq!(monitor.requests, 0);
        }

        #[test]
        fn partial_repository_auth_error_does_not_poison_accessible_commit() {
            let mut monitor = Monitor::new();
            let first = source("first");
            let second = source("second");
            monitor.sync([first.clone(), second.clone()].into_iter());
            let response = json!({
                "data": {
                    "r0": null,
                    "r1": {
                        "object": {
                            "oid": "second",
                            "statusCheckRollup": {
                                "contexts": {
                                    "nodes": [{
                                        "__typename": "CheckRun", "name": "test",
                                        "status": "COMPLETED", "conclusion": "SUCCESS"
                                    }],
                                    "pageInfo": {"hasNextPage": false}
                                }
                            }
                        },
                        "b1": {"associatedPullRequests": {
                            "nodes": [], "pageInfo": {"hasNextPage": false}
                        }},
                        "c1": {"nodes": []}
                    },
                    "rateLimit": {"remaining": 4999, "cost": 1}
                },
                "errors": [{"type": "NOT_FOUND", "path": ["r0"]}]
            });
            monitor.finish_fixture(&[first.clone(), second.clone()], Ok(response));
            assert_eq!(monitor.snapshot(&first).unwrap().status, CiStatus::Unknown);
            assert_eq!(monitor.snapshot(&second).unwrap().status, CiStatus::Passed);
            assert!(monitor.snapshot(&second).unwrap().error.is_none());
            assert!(
                monitor.entries[&first]
                    .due
                    .unwrap()
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    >= 1799
            );
            assert!(monitor.next_request <= Instant::now());
        }

        #[test]
        fn subscribers_added_in_flight_wait_for_their_own_branch_metadata() {
            for succeeds in [true, false] {
                let mut monitor = Monitor::new();
                let source = source("shared");
                let mut linked = source.clone();
                linked.branch = Some("next".into());
                monitor.sync([source.clone()].into_iter());
                let queried = monitor.due_batch(Instant::now());
                monitor.pending = Some(pending_fixture(&queried));
                let fd = monitor.request_pid();
                monitor.sync([source.clone(), linked.clone()].into_iter());
                let due = monitor.entries[&linked].due;
                monitor.refresh(&target(&linked));
                assert_eq!(monitor.request_pid(), fd);
                let pending = monitor.pending.take().unwrap();
                assert_eq!(pending.sources, queried);
                let mut value = json!({
                    "data": {
                        "r0": {
                            "object": {
                                "oid": "shared",
                                "statusCheckRollup": {
                                    "contexts": {
                                        "nodes": [{
                                            "__typename": "CheckRun", "name": "test",
                                            "status": "COMPLETED", "conclusion": "SUCCESS"
                                        }],
                                        "pageInfo": {"hasNextPage": false}
                                    }
                                }
                            },
                            "b0": {"associatedPullRequests": {
                                "nodes": [pull_request(&source, 101)],
                                "pageInfo": {"hasNextPage": false}
                            }},
                            "c0": {"nodes": []}
                        },
                        "rateLimit": {"remaining": 5000}
                    }
                });
                monitor.finish_fixture(
                    &pending.sources,
                    if succeeds {
                        Ok(value.clone())
                    } else {
                        Err(FetchError::normal("offline"))
                    },
                );
                drop(pending);
                assert_eq!(monitor.snapshot(&linked), Some(empty(&linked, 0)));
                assert_eq!(monitor.entries[&linked].due, due);
                let next = monitor.due_batch(
                    monitor
                        .soft_deadline(&source)
                        .unwrap_or_else(Instant::now)
                        .max(Instant::now()),
                );
                assert_eq!(next, vec![source.clone(), linked.clone()]);
                if succeeds {
                    assert!(monitor.entries[&source].due.is_none());
                    assert_eq!(
                        monitor
                            .snapshot(&source)
                            .unwrap()
                            .change_request
                            .unwrap()
                            .number,
                        101
                    );
                    value["data"]["r0"]["b1"] = json!({"associatedPullRequests": {
                        "nodes": [pull_request(&linked, 102)],
                        "pageInfo": {"hasNextPage": false}
                    }});
                    value["data"]["r0"]["c1"] = json!({"nodes": []});
                    monitor.finish_fixture(&next, Ok(value));
                    assert_eq!(
                        monitor
                            .snapshot(&linked)
                            .unwrap()
                            .change_request
                            .unwrap()
                            .number,
                        102
                    );
                    assert!(monitor.entries.values().all(|entry| entry.due.is_none()));
                } else {
                    assert_eq!(
                        monitor.snapshot(&source).unwrap().fetch_state,
                        CiFetchState::Unavailable
                    );
                }
            }
        }

        #[test]
        fn retired_a_to_b_to_a_cannot_apply_the_first_a_response() {
            let mut first = source("same-local-head");
            first.generation = 1;
            let mut monitor = Monitor::new();
            monitor.sync([first.clone()].into_iter());
            monitor.pending = Some(pending_fixture(std::slice::from_ref(&first)));
            monitor.sync([source("intermediate-head")].into_iter());
            let mut current = first.clone();
            current.generation = 3;
            monitor.sync([current.clone()].into_iter());
            monitor.finish_fixture(
                std::slice::from_ref(&first),
                Err(FetchError::normal("retired response")),
            );
            assert_eq!(monitor.snapshot(&current), Some(empty(&current, 0)));
            assert_eq!(monitor.request_pid(), -1);
        }

        #[test]
        fn local_source_failures_keep_checks_but_never_advertise_ready_or_poll_providers() {
            let source = source("local");
            let mut monitor = Monitor::new();
            monitor.sync([source.clone()].into_iter());
            let previous = complete_snapshot(&source, &[CiStatus::Passed]);
            finish_snapshot(&mut monitor, &source, previous.clone());
            monitor.source_unavailable(&source, "local source unavailable");
            let unavailable = monitor.snapshot(&source).unwrap();
            assert_eq!(unavailable.checks, previous.checks);
            assert_eq!(unavailable.fetch_state, CiFetchState::Unavailable);
            assert!(unavailable.stale);
            assert_eq!(monitor.timeout(), -1);
            monitor.resume(&source);
            let loading = monitor.snapshot(&source).unwrap();
            assert_eq!(loading.checks, previous.checks);
            assert_eq!(loading.fetch_state, CiFetchState::Loading);
            assert_eq!(loading.error.as_deref(), Some("local source unavailable"));
            finish_snapshot(&mut monitor, &source, previous);
            assert_eq!(
                monitor.snapshot(&source).unwrap().fetch_state,
                CiFetchState::Ready
            );
            assert!(monitor.snapshot(&source).unwrap().error.is_none());
        }

        #[test]
        fn subscriptions_above_256_are_retained_while_requests_remain_bounded() {
            let mut monitor = Monitor::new();
            monitor.sync((0..300).map(|index| source(&format!("sha-{index}"))));
            assert_eq!(monitor.entries.len(), 300);
            assert_eq!(monitor.due_batch(Instant::now()).len(), BATCH);
        }

        #[test]
        fn auth_and_rate_backoff_never_block_another_cwd_or_host() {
            for retry in [RetryPolicy::Authentication, RetryPolicy::RateLimited] {
                for distinct_host in [false, true] {
                    let limited = source("limited");
                    let mut active = source("active");
                    if distinct_host {
                        active.repository.host = "other.example".into();
                    } else {
                        active.cwd = "/other-auth-context".into();
                    }
                    let mut monitor = Monitor::new();
                    monitor.sync([limited.clone(), active.clone()].into_iter());
                    finish_snapshot(
                        &mut monitor,
                        &active,
                        complete_snapshot(&active, &[CiStatus::Running]),
                    );
                    monitor.finish_fixture(
                        std::slice::from_ref(&limited),
                        Err(FetchError {
                            message: "private context unavailable".into(),
                            retry,
                        }),
                    );
                    let future = Instant::now() + Duration::from_secs(ACTIVE_SECONDS + 1);
                    assert_eq!(monitor.reserve_batch(future), vec![active.clone()]);
                    assert_eq!(
                        monitor.snapshot(&limited).unwrap().fetch_state,
                        CiFetchState::Unavailable
                    );
                    assert_eq!(
                        monitor.snapshot(&active).unwrap().fetch_state,
                        CiFetchState::Loading
                    );
                    let spacing = monitor.next_request;
                    monitor.retire_auth();
                    assert_eq!(monitor.next_request, spacing);
                    assert!(monitor.contexts.is_empty());
                    monitor.sync([limited.clone()].into_iter());
                    assert_eq!(monitor.reserve_batch(spacing), vec![limited]);
                }
            }
        }

        #[test]
        fn huge_retry_after_blocks_only_its_context_without_panicking_or_retrying_early() {
            let limited = source("limited");
            let sibling = source("same-context");
            let mut active = source("other-context");
            active.cwd = "/other-auth-context".into();
            let mut monitor = Monitor::new();
            monitor.sync([limited.clone(), sibling.clone(), active.clone()].into_iter());
            let passed = complete_snapshot(&sibling, &[CiStatus::Passed]);
            finish_snapshot(&mut monitor, &sibling, passed.clone());
            finish_snapshot(
                &mut monitor,
                &active,
                complete_snapshot(&active, &[CiStatus::Running]),
            );
            let sources = [limited.clone()];
            let wire = format!(
                "HTTP/2.0 429 Too Many Requests\r\nRetry-After: {}\r\n\r\n{{\"data\":null,\"errors\":[{{\"type\":\"RATE_LIMITED\"}}]}}",
                u64::MAX
            );
            let response = github::response(
                false,
                wire.as_bytes(),
                "rate limit",
                &sources,
                sources.iter(),
                timestamp(),
            )
            .unwrap();
            monitor.finish(&sources, Ok(response));
            for source in [&limited, &sibling] {
                let snapshot = monitor.snapshot(source).unwrap();
                assert_eq!(snapshot.fetch_state, CiFetchState::Unavailable);
                assert_eq!(snapshot.error.as_deref(), Some(RETRY_DEADLINE_ERROR));
                assert!(monitor.entries[source].due.is_none());
            }
            assert_eq!(monitor.snapshot(&sibling).unwrap().checks, passed.checks);
            assert_eq!(
                monitor.snapshot(&active).unwrap().fetch_state,
                CiFetchState::Ready
            );
            assert!(monitor.snapshot(&active).unwrap().error.is_none());
            monitor.refresh(&target(&limited));
            monitor.resume(&limited);
            monitor.source_unavailable(&limited, "local source error");
            monitor.restore_published(&limited, complete_snapshot(&limited, &[]), None);
            assert_eq!(
                monitor.snapshot(&limited).unwrap().error.as_deref(),
                Some(RETRY_DEADLINE_ERROR)
            );
            let cold = source("new-source-in-blocked-context");
            monitor.sync([limited.clone(), sibling, active.clone(), cold.clone()].into_iter());
            assert_eq!(
                monitor.snapshot(&cold).unwrap().fetch_state,
                CiFetchState::Unavailable
            );
            assert_eq!(
                monitor.snapshot(&cold).unwrap().error.as_deref(),
                Some(RETRY_DEADLINE_ERROR)
            );
            let future = Instant::now() + Duration::from_secs(ACTIVE_SECONDS + 1);
            assert_eq!(monitor.reserve_batch(future), vec![active]);
            let spacing = monitor.next_request;
            monitor.sync(std::iter::empty());
            monitor.sync([cold.clone()].into_iter());
            monitor.refresh(&target(&cold));
            assert_eq!(monitor.timeout(), -1);
            assert!(
                monitor
                    .due_batch(future + Duration::from_secs(3600))
                    .is_empty()
            );
            monitor.retire_auth();
            monitor.sync([limited.clone()].into_iter());
            assert_eq!(
                monitor.snapshot(&limited).unwrap().fetch_state,
                CiFetchState::Loading
            );
            assert_eq!(monitor.reserve_batch(spacing), vec![limited]);
        }

        #[test]
        fn representable_long_retry_after_is_honored_without_a_shorter_cap() {
            let source = source("long-retry");
            let mut monitor = Monitor::new();
            monitor.sync([source.clone()].into_iter());
            let delay = Duration::from_secs(400 * 24 * 3600);
            let before = Instant::now();
            monitor.finish_fixture(
                std::slice::from_ref(&source),
                Ok(json!({
                    "data":null,"errors":[{"type":"RATE_LIMITED"}],"_retry_after":delay.as_secs()
                })),
            );
            assert!(!monitor.blocked(&source));
            let deadline = monitor.request_deadline(&source);
            assert!(deadline >= before.checked_add(delay).unwrap());
            monitor.refresh(&target(&source));
            assert!(
                monitor
                    .reserve_batch(before + delay - Duration::from_secs(1))
                    .is_empty()
            );
            assert_eq!(monitor.reserve_batch(deadline), vec![source]);
        }

        #[test]
        fn published_active_schedule_survives_equivalent_metadata_retirement() {
            let old = source("same");
            let mut monitor = Monitor::new();
            monitor.sync([old.clone()].into_iter());
            let snapshot = complete_snapshot(&old, &[CiStatus::Running]);
            finish_snapshot(&mut monitor, &old, snapshot.clone());
            let schedule = monitor.published_schedule(&old, &snapshot);
            let mut current = old.clone();
            current.generation = 2;
            monitor.sync([current.clone()].into_iter());
            monitor.restore_published(&current, snapshot, schedule);
            assert_eq!(monitor.entries[&current].due, schedule.unwrap().0);
            assert!(monitor.reserve_batch(Instant::now()).is_empty());
        }

        #[test]
        fn child_readiness_keeps_the_leader_unreaped_until_owned_group_cleanup() {
            let source = source("leader");
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "exit 0"]).process_group(0);
            let child = command.spawn().unwrap();
            let pid = child.id() as i32;
            let mut pending = Pending {
                child,
                stdout: scratch_file().unwrap(),
                stderr: scratch_file().unwrap(),
                sources: vec![source],
                deadline: Instant::now() + Duration::from_secs(5),
                group_retired: false,
                waited: false,
            };
            while !pending.exited().unwrap() {
                assert!(Instant::now() < pending.deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                0,
                "leader must still reserve its PID"
            );
            assert!(pending.wait().unwrap().success());
            assert!(pending.waited && pending.group_retired);
            drop(pending);
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        }

        fn history_bytes(sources: &[CiSource]) -> usize {
            let mut contexts = ContextBackoffs::new();
            for source in sources {
                Monitor::context_mut(&mut contexts, source);
            }
            context_storage_bytes(&contexts).unwrap()
        }

        #[test]
        fn hundreds_of_independent_active_contexts_retain_truthful_metadata() {
            let sources: Vec<_> = (0..400)
                .map(|index| {
                    let mut source = source(&format!("commit-{index}"));
                    source.cwd = format!("/project-{index}").into();
                    source
                })
                .collect();
            let mut monitor = Monitor::new();
            monitor.sync(sources.iter().cloned());
            assert_eq!(monitor.entries.len(), sources.len());
            for source in &sources {
                assert!(!monitor.entries[source].admission_waiting);
                assert!(monitor.backoff(source).is_some());
                assert_eq!(monitor.snapshot(source), Some(empty(source, 0)));
            }
            // Each distinct authentication context gets a turn, without IO.
            for _ in &sources {
                let batch = monitor.reserve_batch(monitor.next_request.max(Instant::now()));
                assert_eq!(batch.len(), 1);
                let source = &batch[0];
                finish_snapshot(
                    &mut monitor,
                    source,
                    complete_snapshot(source, &[CiStatus::Passed]),
                );
            }
            assert_eq!(monitor.timeout(), -1);
        }

        #[test]
        fn history_memory_pressure_preserves_limits_until_expiry_or_auth_retirement() {
            for finite_expiry in [false, true] {
                let mut active = source("active");
                active.cwd = "/active-context".into();
                let mut retired = source("retired");
                retired.cwd = format!("/retired/{}", "long-directory-".repeat(128)).into();
                retired.repository.host = format!("{}.example.invalid", "host".repeat(256));
                let budget = history_bytes(&[active.clone(), retired.clone()]);
                let mut monitor = Monitor {
                    context_budget_bytes: budget,
                    ..Monitor::new()
                };
                monitor.sync([active.clone(), retired.clone()].into_iter());
                assert_eq!(context_storage_bytes(&monitor.contexts), Some(budget));
                finish_snapshot(
                    &mut monitor,
                    &active,
                    complete_snapshot(&active, &[CiStatus::Running]),
                );
                monitor.finish_fixture(
                    std::slice::from_ref(&retired),
                    Ok(json!({
                        "data":null,"errors":[{"type":"RATE_LIMITED"}],
                        "_retry_after": if finite_expiry { 7200 } else { u64::MAX }
                    })),
                );
                let deadline = monitor.backoff(&retired).unwrap().hard;
                let mut waiting = source("waiting");
                waiting.cwd = "/waiting-context".into();
                monitor.sync([active.clone(), waiting.clone()].into_iter());
                assert_eq!(monitor.entries.len(), 2);
                assert_eq!(context_storage_bytes(&monitor.contexts), Some(budget));
                assert!(monitor.entries[&waiting].admission_waiting);
                assert_eq!(
                    monitor.snapshot(&waiting).unwrap().error.as_deref(),
                    Some(CONTEXT_CAPACITY_ERROR)
                );
                monitor.refresh(&target(&waiting));
                monitor.resume(&waiting);
                assert!(monitor.entries[&waiting].due.is_none());
                let future = Instant::now() + Duration::from_secs(ACTIVE_SECONDS + 1);
                assert_eq!(monitor.reserve_batch(future), vec![active.clone()]);
                finish_snapshot(
                    &mut monitor,
                    &active,
                    complete_snapshot(&active, &[CiStatus::Passed]),
                );
                if finite_expiry {
                    let deadline = deadline.unwrap();
                    assert_eq!(monitor.context_retry, Some(deadline));
                    monitor.reconcile_contexts(deadline - Duration::from_nanos(1));
                    assert!(monitor.entries[&waiting].admission_waiting);
                    assert_eq!(monitor.backoff(&retired).unwrap().hard, Some(deadline));
                    assert!(
                        monitor
                            .reserve_batch(deadline - Duration::from_nanos(1))
                            .is_empty()
                    );
                    monitor.reconcile_contexts(deadline);
                    assert!(monitor.backoff(&retired).is_none());
                    assert!(!monitor.entries[&waiting].admission_waiting);
                    assert_eq!(
                        monitor.snapshot(&waiting).unwrap().fetch_state,
                        CiFetchState::Loading
                    );
                    assert!(monitor.snapshot(&waiting).unwrap().error.is_none());
                    assert_eq!(monitor.reserve_batch(deadline), vec![waiting.clone()]);
                    finish_snapshot(
                        &mut monitor,
                        &waiting,
                        complete_snapshot(&waiting, &[CiStatus::Passed]),
                    );
                    monitor.sync([active.clone()].into_iter());
                    assert!(monitor.backoff(&waiting).is_none());
                    assert_eq!(
                        context_storage_bytes(&monitor.contexts),
                        Some(history_bytes(&[active]))
                    );
                } else {
                    assert!(monitor.blocked(&retired));
                    assert_eq!(monitor.context_retry, None);
                    assert_eq!(monitor.timeout(), -1);
                    monitor.sync([active, waiting.clone()].into_iter());
                    assert!(monitor.entries[&waiting].admission_waiting);
                    let spacing = monitor.next_request;
                    monitor.retire_auth();
                    monitor.sync([waiting.clone()].into_iter());
                    assert!(!monitor.entries[&waiting].admission_waiting);
                    assert_eq!(monitor.reserve_batch(spacing), vec![waiting]);
                }
            }
        }

        #[test]
        fn history_accounts_for_dynamic_keys_and_retains_soft_auth_embargoes() {
            let mut old = source("old");
            old.cwd = format!("/{}", "directory".repeat(256)).into();
            old.repository.host = format!("{}.invalid", "host".repeat(128));
            let exact = history_bytes(std::slice::from_ref(&old));
            let mut monitor = Monitor {
                context_budget_bytes: exact - 1,
                ..Monitor::new()
            };
            monitor.sync([old.clone()].into_iter());
            assert!(monitor.backoff(&old).is_none());
            assert!(monitor.entries[&old].admission_waiting);
            assert_eq!(monitor.timeout(), -1);
            let mut monitor = Monitor {
                context_budget_bytes: exact,
                ..Monitor::new()
            };
            monitor.sync([old.clone()].into_iter());
            assert!(!monitor.entries[&old].admission_waiting);
            monitor.finish(
                std::slice::from_ref(&old),
                Err(FetchError {
                    message: "authentication unavailable".into(),
                    retry: RetryPolicy::Authentication,
                }),
            );
            let deadline = monitor.backoff(&old).unwrap().soft.unwrap();
            let mut waiting = source("new");
            waiting.cwd = "/new-context".into();
            monitor.sync([waiting.clone()].into_iter());
            assert!(monitor.entries[&waiting].admission_waiting);
            monitor.reconcile_contexts(deadline - Duration::from_nanos(1));
            assert!(monitor.entries[&waiting].admission_waiting);
            assert_eq!(monitor.backoff(&old).unwrap().soft, Some(deadline));
            monitor.reconcile_contexts(deadline);
            assert!(monitor.backoff(&old).is_none());
            assert!(!monitor.entries[&waiting].admission_waiting);
            assert_eq!(monitor.reserve_batch(deadline), vec![waiting]);
        }

        #[test]
        fn oversized_request_keys_fail_locally_without_polling_and_recover_on_metadata_change() {
            let mut oversized = source("large");
            oversized.branch = Some("b".repeat(REQUEST_WORK_BYTES / 48 + 1));
            let normal = source("normal");
            let mut monitor = Monitor::new();
            monitor.sync([oversized.clone(), normal.clone()].into_iter());
            assert_eq!(monitor.reserve_batch(Instant::now()), vec![normal.clone()]);
            assert_eq!(
                monitor.snapshot(&oversized).unwrap().error.as_deref(),
                Some(SOURCE_CAPACITY_ERROR)
            );
            assert!(monitor.entries[&oversized].due.is_none());
            finish_snapshot(
                &mut monitor,
                &normal,
                complete_snapshot(&normal, &[CiStatus::Passed]),
            );
            monitor.refresh(&target(&oversized));
            monitor.resume(&oversized);
            monitor.source_unavailable(&oversized, "superseded diagnostic");
            monitor.restore_published(
                &oversized,
                complete_snapshot(&oversized, &[CiStatus::Running]),
                Some((Some(Instant::now()), true)),
            );
            assert_eq!(
                monitor.snapshot(&oversized).unwrap().error.as_deref(),
                Some(SOURCE_CAPACITY_ERROR)
            );
            assert_eq!(monitor.timeout(), -1);
            assert!(monitor.reserve_batch(monitor.next_request).is_empty());
            let mut replacement = oversized;
            replacement.branch = Some("small".into());
            monitor.sync([replacement.clone()].into_iter());
            assert_eq!(
                monitor.reserve_batch(monitor.next_request.max(Instant::now())),
                vec![replacement]
            );
        }

        #[test]
        fn partial_shared_target_batches_bound_bytes_without_losing_or_starving_subscribers() {
            let sources: Vec<_> = (0..400)
                .map(|index| {
                    let mut source = source("shared-commit");
                    source.branch = Some(format!("branch-{index:04}-{}", "b".repeat(512)));
                    source
                })
                .collect();
            let mut monitor = Monitor::new();
            monitor.sync(sources.iter().cloned());
            let mut remaining: BTreeSet<_> = sources.iter().cloned().collect();
            let mut turns = 0;
            while !remaining.is_empty() {
                let batch = monitor.reserve_batch(monitor.next_request.max(Instant::now()));
                assert!(!batch.is_empty());
                assert!(batch.len() < sources.len());
                assert!(
                    batch
                        .iter()
                        .map(|source| source_work_bytes(source).unwrap())
                        .sum::<usize>()
                        <= REQUEST_WORK_BYTES
                );
                let (_, payload) = github::request(&batch).unwrap();
                assert!(payload.len() <= REQUEST_WORK_BYTES);
                let before = remaining.len();
                for source in &batch {
                    remaining.remove(source);
                }
                assert!(
                    remaining.len() < before,
                    "oldest due subscribers must make progress"
                );
                let results = batch
                    .iter()
                    .map(|source| {
                        (
                            source.clone(),
                            Ok(complete_snapshot(source, &[CiStatus::Passed])),
                        )
                    })
                    .collect();
                monitor.finish(
                    &batch,
                    Ok(BatchResponse {
                        results,
                        global_error: None,
                        retry_after: None,
                    }),
                );
                for source in &remaining {
                    assert!(monitor.entries[source].due.is_some());
                    assert_eq!(monitor.snapshot(source).unwrap().updated_at, 0);
                }
                turns += 1;
                assert!(turns <= sources.len());
            }
            assert!(turns > 1);
            assert_eq!(monitor.timeout(), -1);
        }

        #[test]
        fn generation_changes_and_monitor_exit_reap_owned_requests() {
            for change_generation in [false, true] {
                let old = source("old");
                let mut monitor = Monitor::new();
                monitor.sync([old.clone()].into_iter());
                let pending = pending_fixture(std::slice::from_ref(&old));
                let pid = pending.child.id() as i32;
                let fd = pending.child.id() as i32;
                monitor.pending = Some(pending);
                monitor.sync([source("old"), source("unrelated")].into_iter());
                assert_eq!(monitor.request_pid(), fd);
                monitor.sync([source("old")].into_iter());
                assert_eq!(monitor.request_pid(), fd);
                for _ in 0..8 {
                    monitor.refresh(&target(&source("old")));
                    monitor.sync([source("old")].into_iter());
                    assert_eq!(monitor.request_pid(), fd);
                }
                assert_eq!(monitor.requests, 0);
                if change_generation {
                    monitor.sync([source("new")].into_iter());
                    assert_eq!(monitor.request_pid(), -1);
                }
                drop(monitor);
                assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            }
        }
    }
}
