//! Durable session-local channels. The transport supplies each caller's authority.

use anyhow::{Result, ensure};
use bkpr_protocol::channels::{
    AttachmentMediaType, AttachmentUpload, ChannelCursor, ChannelId, ChannelMentionBinding,
    ChannelRequest, ChannelResponse, ChannelSubscriptionStatus, Participant, ParticipantId,
};
use bkpr_protocol::{ChannelNotificationSource, SessionInfo};
use std::{
    collections::BTreeSet,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) use store::catalog::CatalogRead;
mod store;
#[cfg(test)]
mod tests;

const MAX_REQUESTS: usize = 16;
const MAX_SUBSCRIBERS: usize = 16;
const CANCEL_CHECK: Duration = Duration::from_millis(100);
const STORE_WAIT: Duration = Duration::from_secs(1);
pub(crate) const STALE_USER_MENTION: &str = "selected channel mention changed or left the channel; refresh the roster and select it again; nothing was posted";

/// Current workspace participant supplied by the authenticated owner, not the request.
pub struct WorkspaceParticipant {
    /// Stable reference and current safe display label.
    pub actor: Participant,
    /// Exact live launch eligible for wake work, if any; never restored from channel storage.
    pub launch_id: Option<String>,
    /// Native binding revision of the launch at roster admission.
    pub binding_revision: u64,
}

/// Authority and routing snapshot for the workspace resolved by the channel engine.
pub struct ChannelAuthority {
    /// Authenticated caller, including its stable participant ID.
    pub actor: Participant,
    /// Whether this caller may mutate state through the current attachment.
    pub writable: bool,
    /// Whether this exact workspace is still active; only the user can inspect an archive.
    pub workspace_open: bool,
    /// Currently authorized workspace participants, in addition to the caller.
    pub participants: Vec<WorkspaceParticipant>,
}

/// Verified loss of channel access or of the requesting attachment/session.
///
/// Admission adapters must not use this for internal, storage or registry health failures.
#[derive(Debug)]
pub(crate) struct ChannelAccessLost;

impl std::fmt::Display for ChannelAccessLost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("channel unavailable")
    }
}

impl std::error::Error for ChannelAccessLost {}

pub(crate) struct WakeTarget<'a> {
    pub(crate) launch: &'a str,
    pub(crate) binding: u64,
    participant: &'a str,
    addressed: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct ChannelRecipient<'a> {
    pub(crate) launch: &'a str,
    pub(crate) binding: u64,
    pub(crate) participant: &'a str,
}

pub(crate) struct ChannelClaim<'a> {
    pub(crate) recipient: ChannelRecipient<'a>,
    pub(crate) batch_id: &'a str,
    pub(crate) receiver_id: &'a str,
    pub(crate) generation: u64,
    pub(crate) deadline: u64,
}

/// Authority callbacks for a recipient-scoped wake lookup.
pub(crate) struct ChannelWakeAccess<'a> {
    pub(crate) admit: &'a dyn Fn(&str) -> Result<ChannelAuthority>,
    pub(crate) author_mention: &'a dyn Fn(&str, &ParticipantId) -> Result<Option<String>>,
    pub(crate) connected: &'a dyn Fn() -> bool,
}

/// Format the opaque handle accepted by the exact live-agent mention resolver.
pub(crate) fn agent_mention_id(launch: &str, binding: u64) -> String {
    format!("{launch}:{binding}")
}

pub(crate) struct ChannelRoutes<'a> {
    pub(crate) workspace: Option<&'a str>,
    pub(crate) exact_mentions: Option<(&'a str, u64)>,
    pub(crate) publish: &'a dyn Fn(&[WakeTarget<'_>]),
    pub(crate) binding_live: &'a dyn Fn(&str, u64) -> bool,
    pub(crate) direct_peer: &'a dyn Fn(&ParticipantId) -> Result<Option<WorkspaceParticipant>>,
    pub(crate) fence_user_mentions: &'a UserMentionFence<'a>,
}

pub(crate) type UserMentionFence<'a> =
    dyn Fn(&str, &[ChannelMentionBinding]) -> Result<MutexGuard<'a, ()>> + 'a;

/// Channel storage and bounded subscription admission for one exclusively leased session.
pub struct ChannelEngine {
    root: Arc<Path>,
    store: Mutex<StoreSlot>,
    store_ready: Condvar,
    retirement_epoch: AtomicU64,
    launch_active: std::sync::OnceLock<Box<LaunchActive>>,
    catalog_epoch: AtomicU64,
    requests: AtomicUsize,
    subscriptions: AtomicUsize,
    generation: Mutex<u64>,
    changed: Condvar,
    attention_origin: Option<String>,
    attention_wake: std::sync::OnceLock<AttentionWaker>,
}

/// Bounded per-root wake hints; overflow requests a scan of the durable outboxes.
#[derive(Clone)]
pub(crate) struct AttentionWaker {
    pub(crate) roots: crossbeam_channel::Sender<Arc<Path>>,
    pub(crate) overflow: crossbeam_channel::Sender<()>,
}

impl AttentionWaker {
    fn signal(&self, root: &Arc<Path>) {
        if let Err(crossbeam_channel::TrySendError::Full(_)) = self.roots.try_send(root.clone()) {
            let _ = self.overflow.try_send(());
        }
    }
}

type LaunchActive = dyn Fn(&str) -> bool + Send + Sync;

enum StoreSlot {
    Idle(Option<store::Store>),
    Busy,
}

struct StoreGuard<'a> {
    engine: &'a ChannelEngine,
    store: Option<store::Store>,
}

impl Drop for StoreGuard<'_> {
    fn drop(&mut self) {
        *self.engine.store.lock().unwrap() = StoreSlot::Idle(self.store.take());
        self.engine.store_ready.notify_one();
    }
}

struct Subscription {
    channel_id: ChannelId,
    after: Option<ChannelCursor>,
    limit: u16,
    wait_ms: u32,
}

impl ChannelEngine {
    /// Construct a lazy engine beneath an existing private, exclusively leased session root.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: root.into(),
            store: Mutex::new(StoreSlot::Idle(None)),
            store_ready: Condvar::new(),
            retirement_epoch: AtomicU64::new(0),
            launch_active: std::sync::OnceLock::new(),
            catalog_epoch: AtomicU64::new(0),
            requests: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
            generation: Mutex::new(0),
            changed: Condvar::new(),
            attention_origin: None,
            attention_wake: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn for_session(root: PathBuf, session: &SessionInfo) -> Self {
        let mut engine = Self::new(root);
        engine.attention_origin = Some(
            serde_json::to_string(&attention_session(session))
                .expect("typed attention origin serialization is infallible"),
        );
        engine
    }

    /// Provision committed workspaces without making their lifecycle depend on channel
    /// storage. List and Create retry missed provisioning and surface storage errors.
    pub(crate) fn provision<'a>(&self, workspaces: impl IntoIterator<Item = &'a str>) {
        let result = self
            .acquire_store(Instant::now() + STORE_WAIT, &|| false)
            .map_err(StoreAdmission::into_error)
            .and_then(|mut writer| writer.store.as_mut().unwrap().provision(workspaces));
        if result.is_err() {
            crate::diagnostics::record("channels.provision", None, "deferred");
        }
    }

    pub(crate) fn maintain_legacy(
        &self,
        now_ms: u64,
        workspace_for_pane: impl Fn(&str) -> Option<String>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<u64>> {
        let mut writer = self
            .acquire_store(Instant::now() + STORE_WAIT, &cancelled)
            .map_err(StoreAdmission::into_error)?;
        writer
            .store
            .as_mut()
            .unwrap()
            .maintain_legacy(&self.root, now_ms, &workspace_for_pane)
    }

    /// Execute one typed operation, checking authority against the resolved workspace.
    ///
    /// `admit` must sample live authority without blocking on I/O or returning a registry
    /// guard. Only a typed access loss closes a subscription in response to an admission
    /// error; other errors propagate. `workspace_open` reads exact committed workspace
    /// presence independently of the caller or a provisional close reservation. The caller
    /// must not hold a registry lock around this call. `cancelled` stops pending waits but
    /// does not roll back admitted mutations.
    #[cfg(test)]
    pub fn request(
        &self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        workspace_open: impl Fn(&str) -> bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<ChannelResponse> {
        let fence = Mutex::new(());
        self.request_routed(
            request,
            admit,
            workspace_open,
            cancelled,
            ChannelRoutes {
                workspace: None,
                exact_mentions: None,
                publish: &|_| {},
                binding_live: &|_, _| true,
                direct_peer: &|_| Ok(None),
                fence_user_mentions: &|_, _| Ok(fence.lock().unwrap()),
            },
        )
    }

    pub(crate) fn request_routed(
        &self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        workspace_open: impl Fn(&str) -> bool,
        cancelled: impl Fn() -> bool,
        routes: ChannelRoutes<'_>,
    ) -> Result<ChannelResponse> {
        request.validate()?;
        if let ChannelRequest::Subscribe {
            channel_id,
            after,
            limit,
            wait_ms,
        } = request
        {
            let _permit = Permit::acquire(&self.subscriptions, MAX_SUBSCRIBERS)?;
            return self.subscribe(
                Subscription {
                    channel_id,
                    after,
                    limit,
                    wait_ms,
                },
                admit,
                workspace_open,
                cancelled,
                routes.workspace,
            );
        }
        let _permit = Permit::acquire(&self.requests, MAX_REQUESTS)?;
        let with_store = |deadline| {
            self.acquire_store(deadline, &cancelled)
                .map_err(StoreAdmission::into_error)
        };
        if let ChannelRequest::UploadAttachment { channel_id, upload } = &request {
            let mut writer = with_store(Instant::now() + STORE_WAIT)?;
            writer.run(&workspace_open, |store| {
                store.authorize_upload(channel_id, &admit, routes.workspace)
            })?;
            drop(writer);
            validate_image(upload)?;
        }
        let mut writer = with_store(Instant::now() + STORE_WAIT)?;
        writer.run(&workspace_open, |store| {
            let (response, changed) = store.request(
                request,
                admit,
                routes,
                self.attention_origin.as_deref(),
                |store, workspace| {
                    self.flush_retirements(store, &workspace_open)?;
                    Ok(!workspace_open(workspace))
                },
            )?;
            if changed {
                self.invalidate();
            }
            if changed
                && matches!(
                    &response,
                    ChannelResponse::Posted {
                        attention: true,
                        ..
                    }
                )
                && let Some(wake) = self.attention_wake.get()
            {
                wake.signal(&self.root);
            }
            Ok(response)
        })
    }

    pub(crate) fn with_delivery_store<T>(
        &self,
        cancelled: &impl Fn() -> bool,
        workspace_open: &impl Fn(&str) -> bool,
        work: impl FnOnce(&mut store::Store) -> Result<T>,
    ) -> Result<T> {
        let _permit = Permit::acquire(&self.requests, MAX_REQUESTS)?;
        let mut writer = self
            .acquire_store(Instant::now() + STORE_WAIT, cancelled)
            .map_err(StoreAdmission::into_error)?;
        writer.run(workspace_open, work)
    }

    /// Defer only admission backpressure. Once work starts, its errors retain their meaning.
    pub(crate) fn with_wake_claim_store<T>(
        &self,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
        workspace_open: &impl Fn(&str) -> bool,
        work: impl FnOnce(&mut store::Store) -> Result<T>,
    ) -> Result<Option<T>> {
        let Some(_permit) = Permit::try_acquire(&self.requests, MAX_REQUESTS) else {
            return Ok(None);
        };
        match self.acquire_store(deadline.min(Instant::now() + STORE_WAIT), cancelled) {
            Ok(mut writer) => writer.run(workspace_open, work).map(Some),
            Err(StoreAdmission::Busy) => Ok(None),
            Err(error) => Err(error.into_error()),
        }
    }

    pub(crate) fn set_attention_waker(&self, wake: AttentionWaker) {
        assert!(self.attention_wake.set(wake).is_ok());
        self.attention_wake.get().unwrap().signal(&self.root);
    }

    /// Wake waiters to recheck authority after workspace, attachment or session invalidation.
    /// This never opens storage or acquires the channel writer.
    pub fn invalidate(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(crate) fn workspace_added(&self) {
        self.catalog_epoch.fetch_add(1, Ordering::Release);
        self.invalidate();
    }

    /// Queue retirement after durable workspace closure and wake subscriptions.
    /// The next store operation reconciles pending work against committed workspace presence.
    /// Opening the store retires all work from a previous runtime. This never takes the writer.
    pub fn retire_workspace(&self) {
        self.catalog_epoch.fetch_add(1, Ordering::Release);
        self.reconcile_retired_work();
    }

    /// Mark pending work for reconciliation on the next writer turn, without taking the writer.
    pub(crate) fn reconcile_retired_work(&self) {
        self.retirement_epoch.fetch_add(1, Ordering::Release);
        self.invalidate();
    }

    pub(crate) fn set_launch_liveness(
        &self,
        active: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) {
        assert!(self.launch_active.set(Box::new(active)).is_ok());
    }

    fn flush_retirements(
        &self,
        store: &mut store::Store,
        workspace_open: &impl Fn(&str) -> bool,
    ) -> Result<()> {
        let epoch = self.retirement_epoch.load(Ordering::Acquire);
        if store.retirement_epoch != epoch {
            store.retire_closed_workspaces(workspace_open)?;
            if let Some(active) = self.launch_active.get() {
                store.retire_inactive_launches(active.as_ref())?;
            }
            store.retirement_epoch = epoch;
        }
        Ok(())
    }

    fn acquire_store(
        &self,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> std::result::Result<StoreGuard<'_>, StoreAdmission> {
        loop {
            if cancelled() {
                return Err(StoreAdmission::Cancelled);
            }
            let mut slot = self.store.lock().unwrap();
            if let StoreSlot::Idle(store) = &mut *slot {
                let mut writer = StoreGuard {
                    engine: self,
                    store: store.take(),
                };
                *slot = StoreSlot::Busy;
                drop(slot);
                if writer.store.is_none() {
                    writer.store =
                        Some(store::Store::open(&self.root).map_err(StoreAdmission::Storage)?);
                }
                if cancelled() {
                    return Err(StoreAdmission::Cancelled);
                }
                return Ok(writer);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(StoreAdmission::Busy);
            }
            drop(
                self.store_ready
                    .wait_timeout(slot, remaining.min(CANCEL_CHECK))
                    .unwrap(),
            );
        }
    }

    #[cfg(test)]
    fn with_store<T>(&self, operation: impl FnOnce(&mut store::Store) -> Result<T>) -> Result<T> {
        let mut writer = self
            .acquire_store(Instant::now() + STORE_WAIT, &|| false)
            .map_err(StoreAdmission::into_error)?;
        operation(writer.store.as_mut().unwrap())
    }

    pub(crate) fn catalog_request(
        &self,
        read: CatalogRead<'_>,
        actor_id: &str,
        limit: u16,
        wait_ms: u32,
        workspaces: impl Fn() -> Result<BTreeSet<String>>,
        cancelled: impl Fn() -> bool,
    ) -> Result<ChannelResponse> {
        let (admission, capacity) = match read {
            CatalogRead::Snapshot(_) => (&self.requests, MAX_REQUESTS),
            CatalogRead::Changes(_) => (&self.subscriptions, MAX_SUBSCRIBERS),
        };
        let _permit = Permit::acquire(admission, capacity)?;
        let started = Instant::now();
        let deadline = started + Duration::from_millis(u64::from(wait_ms));
        let read_deadline = if wait_ms == 0 {
            started + STORE_WAIT
        } else {
            deadline
        };
        let mut waiting = false;
        loop {
            let observed = *self.generation.lock().unwrap();
            let mut writer = self
                .acquire_store(read_deadline.min(Instant::now() + STORE_WAIT), &cancelled)
                .map_err(StoreAdmission::into_error)?;
            let epoch = self.catalog_epoch.load(Ordering::Acquire);
            let store = writer.store.as_mut().unwrap();
            if store.catalog_epoch != Some(epoch) {
                store.sync_catalog(workspaces()?, actor_id)?;
                store.catalog_epoch = Some(epoch);
            }
            let page = store.read_catalog(read, actor_id, limit)?;
            drop(writer);
            ensure!(!cancelled(), "channel catalog request disconnected");
            if self.catalog_epoch.load(Ordering::Acquire) != epoch {
                // A committed closure crossed the read. Never return its old live entries.
                if Instant::now() >= read_deadline {
                    return Err(StoreAdmission::Busy.into_error());
                }
                continue;
            }
            // A filtered journal tail can advance the cursor without yielding a visible change.
            // Return that progress rather than scanning it again after every wake.
            if !matches!(
                (&page, read),
                (ChannelResponse::CatalogChanges { changes, cursor, .. }, CatalogRead::Changes(after))
                    if changes.is_empty() && cursor == after
            ) || Instant::now() >= deadline
            {
                return Ok(page);
            }
            if !waiting {
                crate::diagnostics::record("channels.catalog", None, "waiting");
                waiting = true;
            }
            self.wait_for_change(observed, deadline, &cancelled);
        }
    }

    fn wait_for_change(&self, observed: u64, deadline: Instant, cancelled: &impl Fn() -> bool) {
        while !cancelled() {
            let generation = self.generation.lock().unwrap();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if *generation != observed || remaining.is_zero() {
                return;
            }
            drop(
                self.changed
                    .wait_timeout(generation, remaining.min(CANCEL_CHECK))
                    .unwrap(),
            );
        }
    }

    fn subscribe(
        &self,
        subscription: Subscription,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        workspace_open: impl Fn(&str) -> bool,
        cancelled: impl Fn() -> bool,
        caller_workspace: Option<&str>,
    ) -> Result<ChannelResponse> {
        let Subscription {
            channel_id,
            after,
            limit,
            wait_ms,
        } = subscription;
        let deadline = Instant::now() + Duration::from_millis(u64::from(wait_ms));
        let supplied = after.map(|cursor| cursor.sequence);
        let mut after = supplied;
        loop {
            // Capture before reading; a commit during observation must prevent sleeping.
            let observed = *self.generation.lock().unwrap();
            let result =
                match self.acquire_store(deadline.min(Instant::now() + STORE_WAIT), &cancelled) {
                    Ok(mut writer) => writer.run(&workspace_open, |store| {
                        store.changes(&channel_id, after, limit, &admit, caller_workspace)
                    }),
                    Err(StoreAdmission::Cancelled) => break,
                    Err(error) => return Err(error.into_error()),
                };
            let (sequence, changes, more, open) = match result {
                Ok(page) => page,
                Err(error) if error.is::<ChannelAccessLost>() => break,
                Err(error) => return Err(error),
            };
            if !changes.is_empty() || !open || Instant::now() >= deadline {
                return Ok(ChannelResponse::Changes {
                    cursor: ChannelCursor {
                        channel_id,
                        sequence,
                    },
                    changes,
                    more,
                    status: if open || more {
                        ChannelSubscriptionStatus::Active
                    } else {
                        ChannelSubscriptionStatus::Closed
                    },
                });
            }
            after = Some(sequence);
            self.wait_for_change(observed, deadline, &cancelled);
        }
        Ok(ChannelResponse::Changes {
            cursor: ChannelCursor {
                channel_id,
                sequence: supplied.unwrap_or(0),
            },
            changes: Vec::new(),
            more: false,
            status: ChannelSubscriptionStatus::Closed,
        })
    }
}

/// The notification envelope carries a display label, not the exact session name
/// used to derive the native endpoint. Preserve runtime IDs without truncation.
fn attention_session(session: &SessionInfo) -> SessionInfo {
    SessionInfo {
        name: session.name[..session.name.floor_char_boundary(1024)].to_owned(),
        session_id: session.session_id.clone(),
        daemon_id: session.daemon_id.clone(),
        session_instance_id: session.session_instance_id.clone(),
    }
}

/// Advance a bounded batch of durable attention independently of any live session.
/// Returns the next useful Unix-ms deadline. Host saturation is shared across roots,
/// but every root still gets a chance to expire its own oldest pending events.
pub(crate) fn deliver_channel_attention(
    root: &Path,
    service: &crate::notifications::NotificationService,
    legacy_origin: impl Fn() -> Result<SessionInfo>,
    host_retry_at: &mut u64,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<u64>> {
    use crate::notifications::{CHANNEL_ATTENTION_RETRY_MS, ChannelAttentionCapacity};

    if cancelled() {
        return Ok(None);
    }
    let Some(outbox) = store::AttentionStore::open(root)? else {
        return Ok(None);
    };
    for _ in 0..16 {
        if cancelled() {
            return Ok(None);
        }
        let Some(event) = outbox.next()? else {
            return Ok(None);
        };
        let now = store::unix_time_ms()?;
        let at = event.posted_at_ms;
        let expires = at.saturating_add(CHANNEL_ATTENTION_RETRY_MS);
        let has_origin = event.origin.is_some();
        if now >= expires {
            outbox.finish(&event.message_id, has_origin)?;
            continue;
        }
        if now < *host_retry_at {
            return Ok(Some(expires.min(*host_retry_at)));
        }
        let id = event.message_id;
        let source = ChannelNotificationSource {
            session: match event.origin {
                Some(origin) => origin,
                None => legacy_origin()?,
            },
            workspace_id: event.workspace_id,
            channel_id: event.channel_id,
            message_id: id.clone(),
            sequence: event.sequence,
            intent: event.intent,
        };
        // The SELECT has ended. Never hold a SQLite statement, transaction, or
        // registry writer while waiting for the host's durable acknowledgement.
        if let Err(error) = service.record_channel_attention(source, at, &|| !cancelled()) {
            if cancelled() {
                return Ok(None);
            }
            let now = store::unix_time_ms()?;
            *host_retry_at = match error.downcast_ref::<ChannelAttentionCapacity>() {
                Some(capacity) => capacity.next_expiry_ms.max(now.saturating_add(25)),
                None => {
                    crate::diagnostics::record("channels.attention", None, "deferred");
                    now.saturating_add(1_000)
                }
            };
            return Ok(Some(expires.min(*host_retry_at)));
        }
        // If this write fails (or the process exits before it), the next scan
        // repeats the same immutable event; the host owns deduplication.
        outbox.finish(&id, has_origin)?;
    }
    Ok(Some(store::unix_time_ms()?))
}

enum StoreAdmission {
    Busy,
    Cancelled,
    Storage(anyhow::Error),
}

impl StoreAdmission {
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Busy => anyhow::anyhow!("channel store busy"),
            Self::Cancelled => anyhow::anyhow!("channel request cancelled"),
            Self::Storage(error) => error,
        }
    }
}

impl StoreGuard<'_> {
    fn run<T>(
        &mut self,
        workspace_open: &impl Fn(&str) -> bool,
        operation: impl FnOnce(&mut store::Store) -> Result<T>,
    ) -> Result<T> {
        let store = self.store.as_mut().unwrap();
        self.engine.flush_retirements(store, workspace_open)?;
        let before = store.retirement_epoch;
        let result = operation(store);
        // Admission can freeze recipients before retirement, and the capacity check
        // can consume the retirement epoch before inserting that post's work.
        store.retirement_epoch = before;
        if self
            .engine
            .flush_retirements(store, workspace_open)
            .is_err()
        {
            crate::diagnostics::record("channels.retirement", None, "deferred");
        }
        result
    }
}

struct Permit<'a>(&'a AtomicUsize);

impl<'a> Permit<'a> {
    fn acquire(active: &'a AtomicUsize, limit: usize) -> Result<Self> {
        Self::try_acquire(active, limit)
            .ok_or_else(|| anyhow::anyhow!("channel request capacity exceeded"))
    }

    fn try_acquire(active: &'a AtomicUsize, limit: usize) -> Option<Self> {
        active
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |active| {
                (active < limit).then_some(active + 1)
            })
            .ok()
            .map(|_| Self(active))
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn validate_image(upload: &AttachmentUpload) -> Result<()> {
    match upload.media_type {
        AttachmentMediaType::Png => {
            const MAX_DIMENSION: u32 = 4096;
            const MAX_DECODED: usize = 32 * 1024 * 1024;
            let mut options = png::DecodeOptions::default();
            options.set_ignore_checksums(false);
            options.set_skip_ancillary_crc_failures(false);
            options.set_ignore_text_chunk(true);
            options.set_ignore_iccp_chunk(true);
            let mut decoder = png::Decoder::new_with_options(Cursor::new(&upload.bytes), options);
            decoder.set_limits(png::Limits { bytes: 1024 * 1024 });
            let mut reader = decoder
                .read_info()
                .map_err(|_| anyhow::anyhow!("invalid PNG attachment"))?;
            let info = reader.info();
            ensure!(
                info.width <= MAX_DIMENSION
                    && info.height <= MAX_DIMENSION
                    && reader
                        .output_buffer_size()
                        .is_some_and(|size| size <= MAX_DECODED),
                "PNG attachment dimensions exceed limits"
            );
            ensure!(
                info.animation_control.is_none() && info.frame_control.is_none(),
                "animated PNG attachments are unsupported"
            );
            while reader
                .next_row()
                .map_err(|_| anyhow::anyhow!("invalid PNG attachment"))?
                .is_some()
            {}
            reader
                .finish()
                .map_err(|_| anyhow::anyhow!("invalid PNG attachment"))?;
            ensure!(
                reader.info().animation_control.is_none() && reader.info().frame_control.is_none(),
                "animated PNG attachments are unsupported"
            );
        }
    }
    Ok(())
}
