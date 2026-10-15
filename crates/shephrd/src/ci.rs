//! Shared forge CI batching, scheduling, and cache; Git discovery is event-driven.
//! Requests share commit checks while retaining each subscriber's branch metadata.
use shephrd::ci_contract::{
    CiFetchState, CiRefreshTarget, CiSnapshot, CiStatus, ForgeProvider, ForgeRepository,
};
use std::collections::BTreeMap;
use std::path::Path;

mod github;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CiSource {
    pub repository: ForgeRepository,
    pub sha: String,
    pub branch: Option<String>,
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

pub(crate) fn discover(cwd: &Path, branch: Option<&str>) -> Option<CiSource> {
    let sha = crate::project::git_text(cwd, &["rev-parse", "--verify", "HEAD"])?;
    if sha.len() != 40 && sha.len() != 64 {
        return None;
    }
    let branch = branch.filter(|branch| !branch.starts_with("detached @ "));
    let remote = branch
        .and_then(|branch| {
            crate::project::git_text(
                cwd,
                &["config", "--get", &format!("branch.{branch}.pushRemote")],
            )
        })
        .or_else(|| crate::project::git_text(cwd, &["config", "--get", "remote.pushDefault"]))
        .or_else(|| {
            branch.and_then(|branch| {
                crate::project::git_text(
                    cwd,
                    &["config", "--get", &format!("branch.{branch}.remote")],
                )
            })
        })
        .unwrap_or_else(|| "origin".to_owned());
    let url = crate::project::git_text(cwd, &["remote", "get-url", &remote])?;
    let repository = github::repository(&url)?;
    Some(CiSource {
        repository,
        sha,
        branch: branch.map(str::to_owned),
    })
}

const BATCH: usize = 16;
const ACTIVE_SECONDS: u64 = 60;
const IDLE_SECONDS: u64 = 300;
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
        head_sha: source.sha.clone(),
        branch: source.branch.clone(),
        truncated: false,
    }
}

fn same_commit(left: &CiSource, right: &CiSource) -> bool {
    left.repository == right.repository && left.sha == right.sha
}

#[cfg(target_os = "linux")]
pub(crate) use linux::Monitor;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Entry {
        snapshot: CiSnapshot,
        due: Option<Instant>,
        manual: bool,
    }
    struct Pending {
        child: Child,
        fd: OwnedFd,
        stdout: File,
        stderr: File,
        sources: Vec<CiSource>,
        deadline: Instant,
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if self.child.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }

    pub(crate) struct Monitor {
        entries: BTreeMap<CiSource, Entry>,
        pending: Option<Pending>,
        next_request: Instant,
        retry_request: Instant,
        failures: u32,
        requests: u64,
    }
    impl Monitor {
        pub(crate) fn new() -> Self {
            Self {
                entries: BTreeMap::new(),
                pending: None,
                next_request: Instant::now(),
                retry_request: Instant::now(),
                failures: 0,
                requests: 0,
            }
        }
        pub(crate) fn sync(&mut self, sources: impl Iterator<Item = CiSource>) {
            let wanted: BTreeSet<_> = sources
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(256)
                .collect();
            if self.pending.as_ref().is_some_and(|pending| {
                pending
                    .sources
                    .iter()
                    .any(|source| !wanted.contains(source))
            }) {
                // Retire removed identities, including A→B→A, but not unrelated new consumers.
                self.pending = None;
            }
            self.entries.retain(|source, _| wanted.contains(source));
            for source in wanted {
                self.entries.entry(source.clone()).or_insert_with(|| Entry {
                    snapshot: empty(&source, 0),
                    due: Some(Instant::now()),
                    manual: false,
                });
            }
        }
        pub(crate) fn invalidate(&mut self, source: &CiSource) {
            // Same-HEAD Git activity cannot revive a complete result. Source changes
            // are handled by sync, which also cancels requests for retired identities.
            if let Some(entry) = self.entries.get_mut(source)
                && entry.due.is_some()
            {
                if entry.snapshot.fetch_state == CiFetchState::Unavailable {
                    return;
                }
                entry.due = Some(Instant::now());
                refreshing(&mut entry.snapshot);
            }
        }
        pub(crate) fn refresh(&mut self, target: &CiRefreshTarget) {
            let in_flight = self.pending.as_ref().is_some_and(|pending| {
                pending
                    .sources
                    .iter()
                    .any(|source| matches_target(source, target))
            });
            for (source, entry) in &mut self.entries {
                if !matches_target(source, target) || in_flight {
                    continue;
                }
                // Explicit refresh bypasses soft auth/network backoff, never the
                // session's minimum spacing or a hard provider rate-limit deadline.
                if entry.snapshot.fetch_state != CiFetchState::Loading {
                    entry.due = Some(Instant::now());
                    entry.manual = true;
                    refreshing(&mut entry.snapshot);
                }
            }
        }
        pub(crate) fn snapshot(&self, source: &CiSource) -> Option<CiSnapshot> {
            self.entries.get(source).map(|entry| entry.snapshot.clone())
        }
        pub(crate) fn fd(&self) -> i32 {
            self.pending
                .as_ref()
                .map_or(-1, |pending| pending.fd.as_raw_fd())
        }
        pub(crate) fn timeout(&self) -> i32 {
            let deadline = self
                .pending
                .as_ref()
                .map(|pending| pending.deadline)
                .or_else(|| {
                    self.entries
                        .values()
                        .filter_map(|entry| {
                            entry.due.map(|due| {
                                let due = due.max(self.next_request);
                                if entry.manual {
                                    due
                                } else {
                                    due.max(self.retry_request)
                                }
                            })
                        })
                        .min()
                });
            deadline.map_or(-1, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(i32::MAX as u128) as i32
            })
        }
        /// The caller drains Git events before applying any response.
        pub(crate) fn advance(&mut self) {
            let now = Instant::now();
            let done = self.pending.as_mut().is_some_and(|pending| {
                now >= pending.deadline || pending.child.try_wait().ok().flatten().is_some()
            });
            if done {
                let mut pending = self.pending.take().unwrap();
                let result = if now >= pending.deadline {
                    Err(FetchError::normal("CI request timed out"))
                } else {
                    response(&mut pending)
                };
                self.finish(&pending.sources, result);
            }
            if self.pending.is_some() || now < self.next_request {
                return;
            }
            let sources = self.due_batch(now);
            if sources.is_empty() {
                return;
            }
            self.next_request = now + Duration::from_secs(ACTIVE_SECONDS);
            self.requests += 1;
            eprintln!(
                "CI batch {}: {} logical targets",
                self.requests,
                sources.len()
            );
            for source in &sources {
                if let Some(entry) = self.entries.get_mut(source) {
                    refreshing(&mut entry.snapshot);
                }
            }
            match start(&sources) {
                Ok(pending) => self.pending = Some(pending),
                Err(error) => self.finish(
                    &sources,
                    Err(FetchError::normal(format!("CI CLI unavailable: {error}"))),
                ),
            }
        }
        fn due_batch(&self, now: Instant) -> Vec<CiSource> {
            let mut due: Vec<_> = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.due.is_some_and(|due| due <= now)
                        && (entry.manual || now >= self.retry_request)
                })
                .collect();
            due.sort_by_key(|(_, entry)| entry.due);
            let mut commits: Vec<&CiSource> = Vec::new();
            for (source, _) in due {
                if commits.first().is_some_and(|first| {
                    first.repository.provider != source.repository.provider
                        || first.repository.host != source.repository.host
                }) {
                    continue;
                }
                if !commits.iter().any(|other| same_commit(other, source)) {
                    commits.push(source);
                }
                if commits.len() == BATCH {
                    break;
                }
            }
            // The budget counts commits, not their independently queried branches.
            self.entries
                .keys()
                .filter(|source| commits.iter().any(|commit| same_commit(commit, source)))
                .cloned()
                .collect()
        }
        fn finish(
            &mut self,
            sources: &[CiSource],
            mut response: Result<BatchResponse, FetchError>,
        ) {
            let now = Instant::now();
            let error = response.as_ref().err().or_else(|| {
                response
                    .as_ref()
                    .ok()
                    .and_then(|batch| batch.global_error.as_ref())
            });
            if let Some(error) = error {
                self.failures = self.failures.saturating_add(1);
                let deadline = now + Duration::from_secs(error.delay(self.failures));
                match error.retry {
                    RetryPolicy::RateLimited => self.next_request = self.next_request.max(deadline),
                    RetryPolicy::Normal | RetryPolicy::Authentication => {
                        self.retry_request = deadline;
                    }
                }
            } else {
                self.failures = 0;
                self.retry_request = now;
            }
            if let Ok(batch) = &response
                && let Some(delay) = batch.retry_after
            {
                self.next_request = self
                    .next_request
                    .max(now + Duration::from_secs(delay.max(ACTIVE_SECONDS)));
            }
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
                entry.manual = false;
                match result {
                    Ok(snapshot) => {
                        let active = snapshot.checks.iter().any(|check| {
                            matches!(check.status, CiStatus::Pending | CiStatus::Running)
                        });
                        entry.due = if final_snapshot(&snapshot) {
                            None
                        } else {
                            Some(
                                now + Duration::from_secs(if active {
                                    ACTIVE_SECONDS
                                } else {
                                    IDLE_SECONDS
                                }),
                            )
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
                            RetryPolicy::Normal => IDLE_SECONDS,
                        };
                        entry.due = Some(self.next_request.max(now + Duration::from_secs(delay)));
                    }
                }
            }
        }
    }

    fn matches_target(source: &CiSource, target: &CiRefreshTarget) -> bool {
        source.repository == target.repository && source.sha == target.head_sha
    }

    fn final_snapshot(snapshot: &CiSnapshot) -> bool {
        snapshot.fetch_state == CiFetchState::Ready
            && !snapshot.stale
            && snapshot.error.is_none()
            && !snapshot.truncated
            && !snapshot.checks.is_empty()
            && snapshot.checks.iter().all(|check| {
                matches!(
                    check.status,
                    CiStatus::Passed | CiStatus::Failed | CiStatus::Cancelled | CiStatus::Skipped
                )
            })
    }

    fn refreshing(snapshot: &mut CiSnapshot) {
        snapshot.fetch_state = CiFetchState::Loading;
        if snapshot.updated_at == 0 {
            snapshot.fetch_state = CiFetchState::Loading;
            snapshot.stale = false;
            snapshot.error = None;
            return;
        }
        // Job status remains last-known; fetch status must never be inferred from Pending/Running.
        snapshot.stale = true;
        if snapshot.error.is_none() {
            snapshot.error =
                Some("Refreshing provider metadata; showing last known results".into());
        }
    }
    fn timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
    fn memory_file() -> std::io::Result<File> {
        let fd = unsafe { libc::memfd_create(c"shephrd-ci".as_ptr(), libc::MFD_CLOEXEC) };
        if fd == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn start(sources: &[CiSource]) -> std::io::Result<Pending> {
        let (mut command, payload) = match sources[0].repository.provider {
            ForgeProvider::Github => github::request(sources)?,
        };
        let stdout = memory_file()?;
        let stderr = memory_file()?;
        let mut stdin = memory_file()?;
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
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) } as i32;
        if fd == -1 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Pending {
            child,
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            stdout,
            stderr,
            sources: sources.to_vec(),
            deadline: Instant::now() + Duration::from_secs(DEADLINE_SECONDS),
        })
    }
    fn response(pending: &mut Pending) -> Result<BatchResponse, FetchError> {
        let status = pending
            .child
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
                repository: github::repository("https://github.com/owner/repo").unwrap(),
                sha: sha.into(),
                branch: Some("main".into()),
            }
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
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) } as i32;
            assert!(fd >= 0);
            Pending {
                child,
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
                stdout: memory_file().unwrap(),
                stderr: memory_file().unwrap(),
                sources: sources.to_vec(),
                deadline: Instant::now() + Duration::from_secs(60),
            }
        }

        fn complete_snapshot(source: &CiSource, statuses: &[CiStatus]) -> CiSnapshot {
            let mut snapshot = empty(source, 123);
            snapshot.fetch_state = CiFetchState::Ready;
            snapshot.status = CiStatus::aggregate(statuses.iter().copied());
            snapshot.checks = statuses
                .iter()
                .enumerate()
                .map(|(index, status)| shephrd::ci_contract::CiCheck {
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
                .filter(|candidate| same_commit(candidate, source))
                .cloned()
                .collect();
            let results = sources
                .iter()
                .map(|candidate| {
                    let mut snapshot = snapshot.clone();
                    snapshot.branch = candidate.branch.clone();
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
                repository: source.repository.clone(),
                head_sha: source.sha.clone(),
            }
        }

        #[test]
        fn only_complete_known_terminal_check_sets_stop_the_clock() {
            let source = source("final");
            for statuses in [
                vec![CiStatus::Passed],
                vec![CiStatus::Failed, CiStatus::Cancelled, CiStatus::Skipped],
            ] {
                let mut monitor = Monitor::new();
                monitor.sync([source.clone()].into_iter());
                finish_snapshot(&mut monitor, &source, complete_snapshot(&source, &statuses));
                assert!(monitor.entries[&source].due.is_none());
                assert_eq!(monitor.timeout(), -1);
                monitor.invalidate(&source);
                monitor.sync([source.clone()].into_iter());
                assert!(
                    monitor
                        .due_batch(Instant::now() + Duration::from_secs(86400 * 365))
                        .is_empty()
                );
                assert!(!monitor.snapshot(&source).unwrap().stale);
            }
            for statuses in [
                vec![],
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
                if statuses.contains(&CiStatus::Running) || statuses.contains(&CiStatus::Pending) {
                    assert!(due <= Instant::now() + Duration::from_secs(ACTIVE_SECONDS));
                }
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
        fn manual_refresh_shares_commit_coalesces_and_returns_to_frozen_after_rerun() {
            let mut monitor = Monitor::new();
            let source = source("same");
            let mut linked = source.clone();
            linked.branch = Some("linked".into());
            monitor.sync([source.clone(), linked.clone()].into_iter());
            let snapshot = complete_snapshot(&source, &[CiStatus::Passed]);
            finish_snapshot(&mut monitor, &source, snapshot.clone());
            monitor.next_request = Instant::now() + Duration::from_secs(ACTIVE_SECONDS);
            let budget = monitor.next_request;
            monitor.refresh(&target(&source));
            let due = monitor.entries[&source].due;
            for _ in 0..8 {
                monitor.refresh(&target(&source));
                monitor.advance();
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
                assert!(current.error.is_some());
                assert!(monitor.entries[subscriber].manual);
            }
            finish_snapshot(
                &mut monitor,
                &source,
                complete_snapshot(&source, &[CiStatus::Running]),
            );
            assert!(monitor.entries[&source].due.is_some());
            assert!(!monitor.entries[&source].manual);
            finish_snapshot(&mut monitor, &source, snapshot);
            assert!(monitor.entries.values().all(|entry| entry.due.is_none()));
            assert_eq!(monitor.timeout(), -1);
        }

        #[test]
        fn manual_refresh_bypasses_soft_retries_but_not_hard_provider_or_minute_budget() {
            for retry in [
                RetryPolicy::Authentication,
                RetryPolicy::Normal,
                RetryPolicy::RateLimited,
            ] {
                let mut monitor = Monitor::new();
                let source = source("retry");
                monitor.sync([source.clone()].into_iter());
                monitor.next_request = Instant::now() + Duration::from_secs(ACTIVE_SECONDS);
                monitor.finish(
                    std::slice::from_ref(&source),
                    Err(FetchError {
                        message: "offline".into(),
                        retry,
                    }),
                );
                let hard = monitor.next_request;
                monitor.refresh(&target(&source));
                assert_eq!(monitor.next_request, hard);
                assert_eq!(
                    monitor.snapshot(&source).unwrap().fetch_state,
                    CiFetchState::Loading
                );
                monitor.advance();
                assert_eq!(monitor.requests, 0);
                if matches!(retry, RetryPolicy::RateLimited) {
                    assert!(monitor.timeout() >= 3_599_000);
                } else {
                    assert!(monitor.timeout() <= 60_000);
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
            let hard = monitor.next_request;
            monitor.refresh(&target(&source));
            assert_eq!(monitor.next_request, hard);
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
            monitor.next_request = Instant::now() + Duration::from_secs(ACTIVE_SECONDS);
            monitor.sync([source.clone()].into_iter());
            monitor.advance();
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
                snapshot.checks.push(shephrd::ci_contract::CiCheck {
                    name: "test".into(),
                    status,
                    url: None,
                    duration_seconds: Some(42),
                });
                monitor.entries.get_mut(&source).unwrap().snapshot = snapshot.clone();
                monitor.invalidate(&source);
                let refreshing = monitor.snapshot(&source).unwrap();
                assert_eq!(refreshing.fetch_state, CiFetchState::Loading);
                assert_eq!(refreshing.status, status);
                assert!(refreshing.stale);
                assert!(refreshing.error.is_some());
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
                    .retry_request
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
            assert!(same_commit(&new, &linked));
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
            let sources = monitor.due_batch(monitor.retry_request.max(Instant::now()));
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
            assert!(
                monitor.entries[&new]
                    .due
                    .unwrap()
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    >= 299
            );
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
                monitor.advance();
                assert_eq!(monitor.snapshot(&old).unwrap().status, CiStatus::Passed);
                assert_eq!(monitor.entries[&old].due, due);
                assert_eq!(monitor.requests, 0);
                assert!(monitor.due_batch(Instant::now()).is_empty());
            }

            monitor.invalidate(&old);
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
            monitor.finish_fixture(&[source], Ok(json!({"data":{"rateLimit":{"remaining":0,"cost":10,"resetAt":"2099-01-01T00:00:00Z"}},"_retry_after":7200})));
            assert!(
                monitor
                    .next_request
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
                Some(Instant::now() + Duration::from_secs(IDLE_SECONDS));
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
            for _ in 0..100 {
                monitor.advance();
            }
            assert_eq!(monitor.requests, 0);
            assert_eq!(monitor.fd(), -1);
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
                    .next_request
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
            monitor.advance();
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
                let fd = monitor.fd();
                monitor.sync([source.clone(), linked.clone()].into_iter());
                let due = monitor.entries[&linked].due;
                monitor.refresh(&target(&linked));
                assert_eq!(monitor.fd(), fd);
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
                let next = monitor.due_batch(monitor.retry_request.max(Instant::now()));
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
        fn generation_changes_and_monitor_exit_reap_owned_requests() {
            for change_generation in [false, true] {
                let old = source("old");
                let mut monitor = Monitor::new();
                monitor.sync([old.clone()].into_iter());
                let pending = pending_fixture(std::slice::from_ref(&old));
                let pid = pending.child.id() as i32;
                let fd = pending.fd.as_raw_fd();
                monitor.pending = Some(pending);
                monitor.sync([source("old"), source("unrelated")].into_iter());
                assert_eq!(monitor.fd(), fd);
                monitor.sync([source("old")].into_iter());
                assert_eq!(monitor.fd(), fd);
                for _ in 0..8 {
                    monitor.refresh(&target(&source("old")));
                    monitor.invalidate(&source("old"));
                    assert_eq!(monitor.fd(), fd);
                }
                assert_eq!(monitor.requests, 0);
                if change_generation {
                    monitor.sync([source("new")].into_iter());
                    assert_eq!(monitor.fd(), -1);
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
