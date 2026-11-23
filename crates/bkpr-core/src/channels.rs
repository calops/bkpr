//! Durable session-local channels. The transport supplies each caller's authority.

use anyhow::{Result, ensure};
use bkpr_protocol::channels::{
    AttachmentMediaType, AttachmentUpload, ChannelCursor, ChannelId, ChannelRequest,
    ChannelResponse, Participant,
};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

mod store;
#[cfg(test)]
mod tests;

const MAX_REQUESTS: usize = 16;
const MAX_SUBSCRIBERS: usize = 16;
const CANCEL_CHECK: Duration = Duration::from_millis(100);

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

/// Channel storage and bounded subscription admission for one exclusively leased session.
pub struct ChannelEngine {
    root: PathBuf,
    store: Mutex<Option<store::Store>>,
    requests: AtomicUsize,
    subscriptions: AtomicUsize,
    generation: Mutex<u64>,
    changed: Condvar,
}

impl ChannelEngine {
    /// Construct a lazy engine beneath an existing private, exclusively leased session root.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            store: Mutex::new(None),
            requests: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
            generation: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    /// Execute one typed operation, checking authority against the resolved workspace.
    ///
    /// `admit` must sample live authority without blocking on I/O or returning a registry
    /// guard. The caller must not hold a registry lock around this call. `cancelled` must
    /// be cheap; it stops pending waits but does not roll back already admitted mutations.
    pub fn request(
        &self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
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
            return self.subscribe(channel_id, after, limit, wait_ms, admit, cancelled);
        }
        let _permit = Permit::acquire(&self.requests, MAX_REQUESTS)?;
        ensure!(!cancelled(), "channel request cancelled");
        if let ChannelRequest::UploadAttachment { upload, .. } = &request {
            validate_image(upload)?;
        }
        self.with_store(|store| {
            ensure!(!cancelled(), "channel request cancelled");
            let (response, changed) = store.request(request, admit)?;
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

    fn with_store<T>(&self, operation: impl FnOnce(&mut store::Store) -> Result<T>) -> Result<T> {
        let mut store = self.store.lock().unwrap();
        if store.is_none() {
            *store = Some(store::Store::open(&self.root)?);
        }
        operation(store.as_mut().unwrap())
    }

    fn subscribe(
        &self,
        channel_id: ChannelId,
        after: Option<ChannelCursor>,
        limit: u16,
        wait_ms: u32,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        cancelled: impl Fn() -> bool,
    ) -> Result<ChannelResponse> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(wait_ms));
        let mut after = after.map(|cursor| cursor.sequence);
        loop {
            ensure!(!cancelled(), "channel request cancelled");
            // Capture before reading; a commit during observation must prevent sleeping.
            let observed = *self.generation.lock().unwrap();
            let (sequence, changes, more, workspace_open) = self.with_store(|store| {
                ensure!(!cancelled(), "channel request cancelled");
                store.changes(&channel_id, after, limit, &admit)
            })?;
            if !changes.is_empty() || !workspace_open || Instant::now() >= deadline {
                return Ok(ChannelResponse::Changes {
                    cursor: ChannelCursor {
                        channel_id,
                        sequence,
                    },
                    changes,
                    more,
                });
            }
            after = Some(sequence);
            let mut generation = self.generation.lock().unwrap();
            while *generation == observed {
                ensure!(!cancelled(), "channel request cancelled");
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                (generation, _) = self
                    .changed
                    .wait_timeout(generation, remaining.min(CANCEL_CHECK))
                    .unwrap();
            }
        }
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
