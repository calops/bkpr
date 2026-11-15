//! Host-owned, bounded durable notifications from authenticated semantic observations.
//!
//! Observation admission is not a durable receipt: a crash before SQLite commit can
//! lose admitted observations. Records survive session closure and host restart;
//! subscriptions report retained-history gaps, not provider delivery or user reads.
//!
//! The SQLite store retains at most 4096 records, 8 MiB of record bodies and seven
//! days of history. Each body is at most 8 KiB; pages contain at most 128 KiB of
//! bodies plus a bounded metadata envelope. There are at most 4096 source episodes,
//! 64 policy rules, 512 queued work items and 32 simultaneous subscriptions.
//! Episode suppression is volatile and resets on host restart; retained records
//! never restore live source authority.
//! Cursors acknowledge both retained sequence and committed observation-loss
//! generation. Historical loss remains diagnostic; an acknowledged boundary does
//! not force repeated gaps or disable otherwise operational subscriptions.
//!
//! `rusqlite` uses bundled SQLite with default features disabled: its native C
//! compilation replaces a system-SQLite packaging dependency, while omitting its
//! optional statement cache, WASM backend, async runtimes and extension features.
//! SQLite owns rollback recovery, transactions and fsync; this module owns only
//! notification policy, admission, private-file authority and finite retention.
mod storage;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{
    NotificationCursor, NotificationHealth, NotificationPage, NotificationPolicy,
    NotificationQuery, NotificationRequest, NotificationSource, Payload,
};
use crossbeam_channel::{Receiver, Sender, bounded, select_biased};
use std::{
    path::Path,
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const QUEUE_CAPACITY: usize = 512;
const REQUEST_CAPACITY: usize = 32;
const SUBSCRIPTIONS: usize = 32;
const CANCEL_INTERVAL: Duration = Duration::from_millis(25);
const STORAGE_WAIT: Duration = Duration::from_secs(5);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// An already authenticated foreground status, or withdrawal of that authority.
///
/// Call only after releasing pane, published-state, registry and capture locks.
/// Strings contain semantic categories, never prompts, terminal bytes or tokens.
#[derive(Clone, Debug)]
pub struct NotificationObservation {
    /// Authenticated session, pane, harness and immutable process identity.
    pub source: NotificationSource,
    /// `working`, `blocked`, `error`, `idle`, `done`, or `None` for withdrawal.
    pub state: Option<String>,
    /// Optional `permission`, `question` or `auth` category.
    pub kind: Option<String>,
    /// Source observation time in Unix milliseconds.
    pub observed_at_ms: u64,
}

struct Shared {
    admission: RwLock<()>,
    accepting: AtomicBool,
    failed: AtomicBool,
    dropped: AtomicU64,
    retained: AtomicU64,
    error: Mutex<Option<String>>,
    epoch: Mutex<u64>,
    changed: Condvar,
    subscribers: AtomicUsize,
    requests: AtomicUsize,
    shutdown_result: Mutex<Option<std::result::Result<(), String>>>,
    shutdown_serial: Mutex<()>,
    finished: AtomicBool,
    #[cfg(test)]
    subscription_waiting: Mutex<Option<Sender<()>>>,
}
impl Shared {
    fn wake(&self) {
        let mut epoch = self.epoch.lock().unwrap();
        *epoch = epoch.wrapping_add(1);
        self.changed.notify_all();
    }
    fn drop_observations(&self, count: u64) {
        let _ = self
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(count).min(i64::MAX as u64))
            });
    }
    fn fail(&self, error: &anyhow::Error) {
        // Do not leak filesystem paths, arbitrary SQLite diagnostics or record bodies.
        let _ = error;
        self.failed.store(true, Ordering::Release);
        *self.error.lock().unwrap() =
            Some("notification storage failed; durability unavailable".into());
        self.wake();
    }
    fn health(&self) -> NotificationHealth {
        let dropped = self.dropped.load(Ordering::Relaxed);
        let error = self.error.lock().unwrap().clone();
        NotificationHealth {
            healthy: !self.failed.load(Ordering::Acquire) && error.is_none(),
            error,
            dropped_observations: dropped,
            retained: self.retained.load(Ordering::Relaxed),
        }
    }
}

struct Handle {
    queue: Sender<Work>,
    stop: Sender<()>,
    shared: Arc<Shared>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.accepting.store(false, Ordering::Release);
        let _ = self.stop.try_send(());
        self.shared.wake();
    }
}

/// Cloneable host-lifetime notification service; dropping the last handle stops admission.
#[derive(Clone)]
pub struct NotificationService {
    handle: Arc<Handle>,
}

struct Admission<'a>(&'a AtomicUsize);
impl<'a> Admission<'a> {
    fn acquire(count: &'a AtomicUsize, limit: usize) -> Result<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < limit).then_some(current + 1)
            })
            .map_err(|_| anyhow::anyhow!("notification request admission saturated"))?;
        Ok(Self(count))
    }
}
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

enum Command {
    Page {
        query: NotificationQuery,
        after: Option<NotificationCursor>,
        baseline: bool,
    },
    PolicyGet,
    PolicySet(NotificationPolicy),
}
enum Reply {
    Page(NotificationPage),
    Policies(Vec<NotificationPolicy>),
}
enum Work {
    Observation(NotificationObservation),
    Request(Command, Sender<std::result::Result<Reply, String>>),
    SyncHealth,
    #[cfg(test)]
    Pause(Sender<()>, Receiver<()>),
}

impl NotificationService {
    /// Open the private host notification directory and start its sole storage worker.
    ///
    /// `root` is the host's `state_root/notifications`, not a session directory.
    /// Rejects unsafe paths/files and another writer. Database corruption or
    /// initialization failure leaves an explicitly unavailable, leased service.
    pub fn open(root: &Path) -> Result<Self> {
        let opened = storage::Store::open(root)?;
        let (retained, dropped, error) = match &opened {
            storage::Opened::Ready(store) => (store.retained()?, store.dropped(), None),
            storage::Opened::Unavailable(_) => (
                0,
                0,
                Some(
                    "notification storage unavailable; durable records and policy cannot be read"
                        .to_owned(),
                ),
            ),
        };
        let (queue, receiver) = bounded(QUEUE_CAPACITY);
        let (stop, stopping) = bounded(1);
        let shared = Arc::new(Shared {
            admission: RwLock::new(()),
            accepting: AtomicBool::new(true),
            failed: AtomicBool::new(error.is_some()),
            dropped: AtomicU64::new(dropped),
            retained: AtomicU64::new(retained),
            error: Mutex::new(error),
            epoch: Mutex::new(0),
            changed: Condvar::new(),
            subscribers: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            shutdown_result: Mutex::new(None),
            shutdown_serial: Mutex::new(()),
            finished: AtomicBool::new(false),
            #[cfg(test)]
            subscription_waiting: Mutex::new(None),
        });
        let worker_shared = shared.clone();
        thread::Builder::new()
            .name("bkpr-notifications".into())
            .spawn(move || {
                match opened {
                    storage::Opened::Ready(store) => run(store, receiver, stopping, &worker_shared),
                    storage::Opened::Unavailable(authority) => {
                        let _ = stopping.recv();
                        drop(authority);
                    }
                }
                worker_shared.finished.store(true, Ordering::Release);
                worker_shared.wake();
            })
            .context("starting notification storage worker")?;
        Ok(Self {
            handle: Arc::new(Handle {
                queue,
                stop,
                shared,
            }),
        })
    }

    /// Attempt bounded nonblocking admission, with no filesystem access or durable ACK.
    ///
    /// Invalid observations, queue saturation, failed storage and shutdown increment
    /// explicit loss health. The caller retains authentication/ordering responsibility.
    pub fn observe(&self, observation: NotificationObservation) {
        if !valid_observation(&observation) {
            self.record_dropped_observations(1);
            return;
        }
        let shared = &self.handle.shared;
        let Ok(_admission) = shared.admission.try_read() else {
            self.record_dropped_observations(1);
            return;
        };
        if !shared.accepting.load(Ordering::Acquire)
            || shared.failed.load(Ordering::Acquire)
            || self
                .handle
                .queue
                .try_send(Work::Observation(observation))
                .is_err()
        {
            self.record_dropped_observations(1);
        }
    }

    /// Report observations lost before service admission, including capture overflow.
    pub fn record_dropped_observations(&self, count: u64) {
        let shared = &self.handle.shared;
        shared.drop_observations(count);
        if let Ok(_admission) = shared.admission.try_read()
            && shared.accepting.load(Ordering::Acquire)
            && !shared.failed.load(Ordering::Acquire)
        {
            let _ = self.handle.queue.try_send(Work::SyncHealth);
        }
        // Capture must not wait for subscriber locks: waiters check health on each
        // bounded cancellation deadline even if this nonblocking wake cannot lock.
        if let Ok(mut epoch) = shared.epoch.try_lock() {
            *epoch = epoch.wrapping_add(1);
            shared.changed.notify_all();
        }
    }

    /// Sample service health without disk access; this is not a durability receipt.
    /// `healthy` describes current operation; historical loss remains in the counter.
    pub fn health(&self) -> NotificationHealth {
        self.handle.shared.health()
    }

    /// Inspect retained records, configure host policy, or wait for resumable changes.
    ///
    /// A session endpoint is clamped to its persistent ID and cannot write policy.
    /// Validates before admission; rejects saturation/cancellation and bounds waits.
    /// A storage timeout reports an unknown outcome, never a negative durable ACK.
    pub fn request(
        &self,
        mut request: NotificationRequest,
        session_id: Option<&str>,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Payload> {
        request.validate()?;
        if matches!(request, NotificationRequest::Health {}) {
            return Ok(Payload::Control(serde_json::to_value(self.health())?));
        }
        ensure!(
            !self.handle.shared.failed.load(Ordering::Acquire),
            "notification storage unavailable; no durable cursor or policy available"
        );
        match &mut request {
            NotificationRequest::Inspect { query, .. }
            | NotificationRequest::Subscribe { query, .. } => clamp(query, session_id)?,
            NotificationRequest::PolicySet { .. } => ensure!(
                session_id.is_none(),
                "notification policy writes require host control"
            ),
            NotificationRequest::PolicyGet {} | NotificationRequest::Health {} => {}
        }
        match request {
            NotificationRequest::Health {} => unreachable!(),
            NotificationRequest::Inspect { query, after } => self
                .command(
                    Command::Page {
                        query,
                        after,
                        baseline: false,
                    },
                    is_connected,
                )?
                .payload(),
            NotificationRequest::Subscribe {
                query,
                after,
                wait_ms,
            } => {
                let _subscriber =
                    Admission::acquire(&self.handle.shared.subscribers, SUBSCRIPTIONS)?;
                let deadline = Instant::now() + Duration::from_millis(u64::from(wait_ms));
                let baseline = after.is_none();
                let mut after = after;
                loop {
                    let shared = &self.handle.shared;
                    let epoch = *shared.epoch.lock().unwrap();
                    let reply = self.command(
                        Command::Page {
                            query: query.clone(),
                            after: after.clone(),
                            baseline: baseline && after.is_none(),
                        },
                        is_connected,
                    )?;
                    let Reply::Page(page) = reply else {
                        unreachable!()
                    };
                    let advanced = after.as_ref().is_some_and(|cursor| cursor != &page.cursor);
                    if !page.notifications.is_empty()
                        || page.gap
                        || page.has_more
                        || advanced
                        || !page.health.healthy
                        || Instant::now() >= deadline
                    {
                        return Reply::Page(page).payload();
                    }
                    after = Some(page.cursor);
                    #[cfg(test)]
                    if let Some(waiting) = shared.subscription_waiting.lock().unwrap().as_ref() {
                        let _ = waiting.try_send(());
                    }
                    let mut current = shared.epoch.lock().unwrap();
                    while *current == epoch && Instant::now() < deadline {
                        ensure!(is_connected(), "notification subscription canceled");
                        ensure!(
                            shared.accepting.load(Ordering::Acquire),
                            "notification service stopping"
                        );
                        let remaining = deadline
                            .saturating_duration_since(Instant::now())
                            .min(CANCEL_INTERVAL);
                        current = shared.changed.wait_timeout(current, remaining).unwrap().0;
                        if shared.dropped.load(Ordering::Relaxed)
                            > after.as_ref().unwrap().loss_generation
                        {
                            break;
                        }
                    }
                }
            }
            NotificationRequest::PolicyGet {} => {
                let Reply::Policies(mut policies) =
                    self.command(Command::PolicyGet, is_connected)?
                else {
                    unreachable!()
                };
                if let Some(id) = session_id {
                    policies.retain(|policy| {
                        policy
                            .scope
                            .session_id
                            .as_deref()
                            .is_none_or(|scope| scope == id)
                    });
                }
                Reply::Policies(policies).payload()
            }
            NotificationRequest::PolicySet { policy } => self
                .command(Command::PolicySet(policy), is_connected)?
                .payload(),
        }
    }

    fn command(&self, command: Command, connected: &dyn Fn() -> bool) -> Result<Reply> {
        ensure!(connected(), "notification request canceled");
        let shared = &self.handle.shared;
        let _request = Admission::acquire(&shared.requests, REQUEST_CAPACITY)?;
        let (sender, receiver) = bounded(1);
        {
            let _admission = shared.admission.read().unwrap();
            ensure!(
                shared.accepting.load(Ordering::Acquire),
                "notification service stopping"
            );
            self.handle
                .queue
                .try_send(Work::Request(command, sender))
                .map_err(|_| anyhow::anyhow!("notification queue saturated or unavailable"))?;
        }
        let deadline = Instant::now() + STORAGE_WAIT;
        loop {
            match receiver.recv_timeout(CANCEL_INTERVAL) {
                Ok(result) => return result.map_err(anyhow::Error::msg),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    bail!("notification storage worker unavailable; outcome unknown")
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    ensure!(
                        connected(),
                        "notification request canceled; queued work outcome unknown"
                    );
                    ensure!(
                        Instant::now() < deadline,
                        "notification storage timeout; queued work outcome unknown"
                    );
                }
            }
        }
    }

    /// Stop admission and drain accepted work for at most two seconds, idempotently.
    ///
    /// Failure/timeout is cached and reported honestly. A stalled worker keeps its
    /// exclusive notification file lease until it exits; this call never joins it.
    /// Success means accepted work drained durably, not lossless capture or delivery.
    /// Rejected observations remain counter/cursor-gap diagnostics and do not fail
    /// a completed drain; actual storage failure or unknown pending work does.
    pub fn shutdown(&self) -> Result<()> {
        let shared = &self.handle.shared;
        let _serial = shared.shutdown_serial.lock().unwrap();
        if let Some(result) = shared.shutdown_result.lock().unwrap().as_ref() {
            return result.clone().map_err(anyhow::Error::msg);
        }
        {
            let _admission = shared.admission.write().unwrap();
            shared.accepting.store(false, Ordering::Release);
            let _ = self.handle.stop.try_send(());
        }
        shared.wake();
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        let mut epoch = shared.epoch.lock().unwrap();
        while !shared.finished.load(Ordering::Acquire) && Instant::now() < deadline {
            epoch = shared
                .changed
                .wait_timeout(epoch, deadline.saturating_duration_since(Instant::now()))
                .unwrap()
                .0;
        }
        drop(epoch);
        let result = if !shared.finished.load(Ordering::Acquire) {
            Err("notification drain timed out; pending durability unknown; worker retains file authority".to_owned())
        } else if let Some(error) = shared.error.lock().unwrap().clone() {
            Err(error)
        } else {
            Ok(())
        };
        *shared.shutdown_result.lock().unwrap() = Some(result.clone());
        result.map_err(anyhow::Error::msg)
    }
}
impl Reply {
    fn payload(self) -> Result<Payload> {
        Ok(Payload::Control(match self {
            Self::Page(page) => serde_json::to_value(page)?,
            Self::Policies(policies) => serde_json::to_value(policies)?,
        }))
    }
}
fn clamp(query: &mut NotificationQuery, session_id: Option<&str>) -> Result<()> {
    if let Some(id) = session_id {
        ensure!(
            query
                .scope
                .session_id
                .as_deref()
                .is_none_or(|scope| scope == id),
            "notification scope conflicts with session endpoint"
        );
        query.scope.session_id = Some(id.to_owned());
        query.validate()?;
    }
    Ok(())
}
fn valid_observation(observation: &NotificationObservation) -> bool {
    observation.source.session.name.len() <= 1024
        && observation.source.validate().is_ok()
        && observation.observed_at_ms <= now_ms().saturating_add(storage::MAX_AGE_MS)
        && observation
            .state
            .as_deref()
            .is_none_or(|state| matches!(state, "working" | "blocked" | "error" | "idle" | "done"))
        && observation
            .kind
            .as_deref()
            .is_none_or(|kind| matches!(kind, "permission" | "question" | "auth"))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as u64
}
fn run(mut store: storage::Store, queue: Receiver<Work>, stopping: Receiver<()>, shared: &Shared) {
    loop {
        let expiry = if shared.failed.load(Ordering::Acquire) {
            None
        } else {
            match store.next_expiry() {
                Ok(expiry) => expiry,
                Err(error) => {
                    shared.fail(&error);
                    None
                }
            }
        };
        let expiration = expiry.map_or_else(crossbeam_channel::never, |deadline| {
            crossbeam_channel::after(Duration::from_millis(deadline.saturating_sub(now_ms())))
        });
        select_biased! {
            recv(stopping) -> _ => break,
            recv(expiration) -> _ => match store.expire(now_ms()) {
                Ok(changed) => {
                    if changed {
                        match store.retained() {
                            Ok(retained) => shared.retained.store(retained, Ordering::Relaxed),
                            Err(error) => shared.fail(&error),
                        }
                        shared.wake();
                    }
                }
                Err(error) => shared.fail(&error),
            },
            recv(queue) -> work => match work {
                Ok(work) => process(&mut store, work, shared),
                Err(_) => break,
            }
        }
    }
    while let Ok(work) = queue.try_recv() {
        process(&mut store, work, shared);
    }
    if let Err(error) = store.sync_dropped(shared.dropped.load(Ordering::Relaxed)) {
        shared.fail(&error);
    }
}
fn process(store: &mut storage::Store, work: Work, shared: &Shared) {
    if let Err(error) = store.sync_dropped(shared.dropped.load(Ordering::Relaxed)) {
        shared.fail(&error);
    }
    match work {
        Work::SyncHealth => {}
        #[cfg(test)]
        Work::Pause(entered, resume) => {
            let _ = entered.send(());
            let _ = resume.recv();
        }
        Work::Observation(observation) => {
            if shared.error.lock().unwrap().is_some() {
                shared.drop_observations(1);
                shared.wake();
                return;
            }
            match store.observe(observation, now_ms()) {
                Ok(changed) => {
                    if changed {
                        match store.retained() {
                            Ok(retained) => shared.retained.store(retained, Ordering::Relaxed),
                            Err(error) => shared.fail(&error),
                        }
                        shared.wake();
                    }
                }
                Err(error) => {
                    shared.drop_observations(1);
                    if store.failed() {
                        shared.fail(&error);
                    } else {
                        shared.wake();
                    }
                }
            }
        }
        Work::Request(command, reply) => {
            if shared.failed.load(Ordering::Acquire) {
                let _ = reply.try_send(Err(
                    "notification storage unavailable; durable outcome unknown".into(),
                ));
                return;
            }
            let mut result = match command {
                Command::Page {
                    query,
                    after,
                    baseline,
                } => store
                    .page(&query, after.as_ref(), baseline, now_ms(), shared.health())
                    .map(Reply::Page),
                Command::PolicyGet => Ok(Reply::Policies(store.policies().to_vec())),
                Command::PolicySet(policy) => {
                    if shared.error.lock().unwrap().is_some() {
                        Err(anyhow::anyhow!(
                            "notification storage failed; policy not changed"
                        ))
                    } else {
                        store
                            .set_policy(policy)
                            .map(|()| Reply::Policies(store.policies().to_vec()))
                    }
                }
            };
            // Validation errors must not poison a healthy store. Storage errors mark
            // their origin inside Store, independently of request/cursor rejection.
            if store.failed() {
                shared.fail(&anyhow::anyhow!("notification storage failure"));
                result = Err(anyhow::anyhow!(
                    "notification storage failed; durable outcome unknown"
                ));
            }
            if let Ok(Reply::Page(page)) = &result {
                shared
                    .retained
                    .store(page.health.retained, Ordering::Relaxed);
            }
            let _ = reply.try_send(result.map_err(|error| error.to_string()));
        }
    }
}
