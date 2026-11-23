//! Durable session-local channels. The transport supplies each caller's authority.

use anyhow::{Result, ensure};
use bkpr_protocol::channels::{
    AttachmentMediaType, AttachmentUpload, ChannelCursor, ChannelId, ChannelRequest,
    ChannelResponse, ChannelSubscriptionStatus, Participant,
};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

mod store;
#[cfg(test)]
mod tests;

const MAX_REQUESTS: usize = 16;
const MAX_SUBSCRIBERS: usize = 16;
const CANCEL_CHECK: Duration = Duration::from_millis(100);
const STORE_WAIT: Duration = Duration::from_secs(1);

/// Current workspace participant supplied by the authenticated owner, not the request.
pub struct WorkspaceParticipant {
    /// Stable reference and current safe display label.
    pub actor: Participant,
    /// Exact live launch eligible for wake work, if any; never restored from channel storage.
    pub launch_id: Option<String>,
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

/// Channel storage and bounded subscription admission for one exclusively leased session.
pub struct ChannelEngine {
    root: PathBuf,
    store: Mutex<StoreSlot>,
    store_ready: Condvar,
    retirement_epoch: AtomicU64,
    requests: AtomicUsize,
    subscriptions: AtomicUsize,
    generation: Mutex<u64>,
    changed: Condvar,
}

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
            root,
            store: Mutex::new(StoreSlot::Idle(None)),
            store_ready: Condvar::new(),
            retirement_epoch: AtomicU64::new(0),
            requests: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
            generation: Mutex::new(0),
            changed: Condvar::new(),
        }
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

    /// Execute one typed operation, checking authority against the resolved workspace.
    ///
    /// `admit` must sample live authority without blocking on I/O or returning a registry
    /// guard. Only a typed access loss closes a subscription in response to an admission
    /// error; other errors propagate. `workspace_open` reads exact committed workspace
    /// presence independently of the caller or a provisional close reservation. The caller
    /// must not hold a registry lock around this call. `cancelled` stops pending waits but
    /// does not roll back admitted mutations.
    pub fn request(
        &self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        workspace_open: impl Fn(&str) -> bool,
        cancelled: impl Fn() -> bool,
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
                store.authorize_upload(channel_id, &admit)
            })?;
            drop(writer);
            validate_image(upload)?;
        }
        let mut writer = with_store(Instant::now() + STORE_WAIT)?;
        writer.run(&workspace_open, |store| {
            let (response, changed) = store.request(request, admit, |store, workspace| {
                self.flush_retirements(store, &workspace_open)?;
                Ok(!workspace_open(workspace))
            })?;
            if changed {
                self.invalidate();
            }
            Ok(response)
        })
    }

    /// Wake waiters to recheck authority after workspace, attachment or session invalidation.
    /// This never opens storage or acquires the channel writer.
    pub fn invalidate(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }

    /// Queue retirement after durable workspace closure and wake subscriptions.
    /// The next store operation reconciles pending work against committed workspace presence.
    /// Opening the store retires all work from a previous runtime. This never takes the writer.
    pub fn retire_workspace(&self) {
        self.retirement_epoch.fetch_add(1, Ordering::Release);
        self.invalidate();
    }

    fn flush_retirements(
        &self,
        store: &mut store::Store,
        workspace_open: &impl Fn(&str) -> bool,
    ) -> Result<()> {
        let epoch = self.retirement_epoch.load(Ordering::Acquire);
        if store.retirement_epoch != epoch {
            store.retire_closed_workspaces(workspace_open)?;
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

    fn subscribe(
        &self,
        subscription: Subscription,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        workspace_open: impl Fn(&str) -> bool,
        cancelled: impl Fn() -> bool,
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
        'observe: loop {
            // Capture before reading; a commit during observation must prevent sleeping.
            let observed = *self.generation.lock().unwrap();
            let result =
                match self.acquire_store(deadline.min(Instant::now() + STORE_WAIT), &cancelled) {
                    Ok(mut writer) => writer.run(&workspace_open, |store| {
                        store.changes(&channel_id, after, limit, &admit)
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
            loop {
                if cancelled() {
                    break 'observe;
                }
                let generation = self.generation.lock().unwrap();
                let remaining = deadline.saturating_duration_since(Instant::now());
                if *generation != observed || remaining.is_zero() {
                    break;
                }
                drop(
                    self.changed
                        .wait_timeout(generation, remaining.min(CANCEL_CHECK))
                        .unwrap(),
                );
            }
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
        let result = operation(store);
        // A close can arrive after authority admission but before its post commits.
        // It must retire that post's work as well, without changing the committed reply.
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
        ensure!(
            active
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, |active| {
                    (active < limit).then_some(active + 1)
                })
                .is_ok(),
            "channel request capacity exceeded"
        );
        Ok(Self(active))
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
