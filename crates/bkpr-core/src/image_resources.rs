use crate::{Pane, Shared, registry::sessions::ClientLease};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{Image, ImageDescriptor, ImageEncoding, ImageKey, encode_image};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const CACHE_BYTES: usize = 128 * 1024 * 1024;
const CACHE_ENTRIES: usize = 64;
const INPUT_BYTES: usize = 128 * 1024 * 1024;
const PREPARATIONS: usize = 6;
const WORKERS: usize = 2;

static POOL: std::sync::OnceLock<std::result::Result<Arc<PreparationPool>, String>> =
    std::sync::OnceLock::new();
/// Return the host-shared pool, reporting any retained worker-startup failure.
pub(crate) fn shared_pool() -> Result<Arc<PreparationPool>> {
    POOL.get_or_init(|| PreparationPool::new().map_err(|error| format!("{error:#}")))
        .clone()
        .map_err(anyhow::Error::msg)
}
/// Purge reuse and cancel preparation for one pane incarnation without starting an unused pool.
pub(crate) fn retire(incarnation: uuid::Uuid) {
    if let Some(Ok(pool)) = POOL.get() {
        pool.retire(incarnation);
    }
}

/// Retains the original client lease and pane incarnation through the completion fence.
pub(crate) struct Observation {
    pane: Weak<Shared>,
    incarnation: uuid::Uuid,
    client: Arc<ClientLease>,
    view_id: String,
    epoch: u64,
    /// Unpinned canonical pixels; preparation must upgrade them within its byte admission.
    pub(crate) image: Weak<Image>,
    /// Validated shape announced before receiving any encoded bytes.
    pub(crate) descriptor: ImageDescriptor,
}
impl Observation {
    /// Recheck the original live client/view, visibility lifetime and exact current image generation.
    pub(crate) fn validate(&self) -> Result<()> {
        self.client.ensure_active()?;
        let pane = self.pane.upgrade().context("image pane retired")?;
        ensure!(
            !pane.forgotten.load(Ordering::Acquire),
            "image pane retired"
        );
        let authority = pane.authority.lock().unwrap();
        authority.check_client(&self.view_id, Some(&self.client))?;
        let view = authority
            .views
            .get(&self.view_id)
            .context("image view retired")?;
        ensure!(
            view.spec.visible && view.inspection_epoch == self.epoch,
            "image view inspection retired"
        );
        let published = pane.published.lock().unwrap();
        let frame = published
            .scroll_frames
            .get(&self.view_id)
            .or(published.frame.as_ref())
            .context("image scene unavailable")?;
        ensure!(
            frame
                .images
                .iter()
                .any(|image| image.key() == self.descriptor.key
                    && self.image.as_ptr() == Arc::as_ptr(image)),
            "image generation replaced or removed"
        );
        Ok(())
    }
}
impl Observation {
    /// Capture an authorized visible image without pinning its pane or pixels; reject stale authority or shape.
    pub(crate) fn new(
        pane: Pane,
        client: Arc<ClientLease>,
        view_id: String,
        key: ImageKey,
    ) -> Result<Self> {
        let authority = pane.shared.authority.lock().unwrap();
        authority.check_client(&view_id, Some(&client))?;
        let view = authority
            .views
            .get(&view_id)
            .context("image view unavailable")?;
        ensure!(
            view.spec.visible && view.live(),
            "image view is not visible or live"
        );
        let epoch = view.inspection_epoch;
        let published = pane.shared.published.lock().unwrap();
        let frame = published
            .scroll_frames
            .get(&view_id)
            .or(published.frame.as_ref())
            .context("image scene unavailable")?;
        let image = frame
            .images
            .iter()
            .find(|image| image.key() == key)
            .cloned()
            .context("image generation unavailable in view")?;
        let descriptor = ImageDescriptor::from_image(&image)?;
        drop(published);
        drop(authority);
        let observation = Observation {
            pane: Arc::downgrade(&pane.shared),
            incarnation: pane.shared.image_incarnation,
            client,
            view_id,
            epoch,
            image: Arc::downgrade(&image),
            descriptor,
        };
        observation.validate()?;
        Ok(observation)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    runtime: uuid::Uuid,
    incarnation: uuid::Uuid,
    image: ImageKey,
}
struct Pixels(Arc<Image>);
impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0.pixels
    }
}
/// Immutable encoded pixels shared across observers and retained independently of cache eviction.
pub(crate) struct Encoded {
    /// Lossless representation announced by the stream's Start frame.
    pub(crate) encoding: ImageEncoding,
    /// Shared encoded body, or retained canonical pixels for Raw.
    pub(crate) bytes: bytes::Bytes,
    retained_bytes: usize,
}
struct Job {
    result: Mutex<Option<std::result::Result<Arc<Encoded>, String>>>,
    changed: Condvar,
    cancelled: std::sync::atomic::AtomicBool,
}
struct Work {
    key: CacheKey,
    pane: Weak<Shared>,
    image: Arc<Image>,
    job: Arc<Job>,
}
struct State {
    inflight: HashMap<CacheKey, Weak<Job>>,
    inputs: usize,
    cache: VecDeque<(CacheKey, Arc<Encoded>)>,
    bytes: usize,
}
/// One host-shared pool; fresh pane incarnations also bind their owning runtime.
/// Reuse <=128MiB/64 entries; <=6 jobs retain <=128MiB canonical input capacity.
/// At most two active streams pin canonical inputs and encoded results separately.
pub(crate) struct PreparationPool {
    state: Mutex<State>,
    queue: crossbeam_channel::Sender<Work>,
    encoded: AtomicUsize,
    streams: Mutex<StreamState>,
    stream_changed: Condvar,
    #[cfg(test)]
    before_encode: Mutex<VecDeque<Arc<std::sync::Barrier>>>,
}
#[derive(Default)]
struct StreamState {
    next: u64,
    waiting: VecDeque<(u64, String)>,
    active: HashMap<u64, String>,
}
/// Bounded FIFO stream admission; dropping it releases queued or active capacity.
pub(crate) struct StreamPermit {
    pool: Arc<PreparationPool>,
    ticket: u64,
}
impl StreamPermit {
    /// Wait at most `timeout` for an active slot; `false` remains queued and requires cancellation/deadline checks.
    pub(crate) fn activate(&self, timeout: Duration) -> bool {
        let mut state = self.pool.streams.lock().unwrap();
        if state.active.contains_key(&self.ticket) {
            return true;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if state.active.len() < 2
                && state
                    .waiting
                    .front()
                    .is_some_and(|(ticket, _)| *ticket == self.ticket)
            {
                let (ticket, client) = state.waiting.pop_front().unwrap();
                state.active.insert(ticket, client);
                self.pool.stream_changed.notify_all();
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = self
                .pool
                .stream_changed
                .wait_timeout(state, remaining)
                .unwrap()
                .0;
        }
    }
}
impl Drop for StreamPermit {
    fn drop(&mut self) {
        let mut state = self.pool.streams.lock().unwrap();
        state.waiting.retain(|(ticket, _)| *ticket != self.ticket);
        state.active.remove(&self.ticket);
        self.pool.stream_changed.notify_all();
    }
}
/// One preparation observer; dropping the last observer cancels unneeded preparation work.
pub(crate) struct Preparation(Arc<Job>);
impl Preparation {
    /// Wait at most `timeout` for publication; `None` requires another cancellation/deadline check.
    pub(crate) fn wait(
        &self,
        timeout: Duration,
    ) -> Option<std::result::Result<Arc<Encoded>, String>> {
        let result = self.0.result.lock().unwrap();
        let result = if result.is_none() {
            self.0.changed.wait_timeout(result, timeout).unwrap().0
        } else {
            result
        };
        result.clone()
    }
}
impl PreparationPool {
    /// Start an independent bounded two-worker pool; report worker-startup failures.
    pub(crate) fn new() -> Result<Arc<Self>> {
        let (queue, receiver) = crossbeam_channel::bounded(4);
        let pool = Arc::new(Self {
            state: Mutex::new(State {
                inflight: HashMap::new(),
                inputs: 0,
                cache: VecDeque::new(),
                bytes: 0,
            }),
            queue,
            encoded: AtomicUsize::new(0),
            streams: Mutex::new(StreamState::default()),
            stream_changed: Condvar::new(),
            #[cfg(test)]
            before_encode: Mutex::new(VecDeque::new()),
        });
        for _ in 0..WORKERS {
            let receiver = receiver.clone();
            let owner = Arc::downgrade(&pool);
            std::thread::Builder::new()
                .name("bkpr-image-prepare".into())
                .spawn(move || {
                    while let Ok(work) = receiver.recv() {
                        let Some(pool) = owner.upgrade() else { break };
                        pool.prepare(work);
                    }
                })?;
        }
        Ok(pool)
    }
    /// Reserve FIFO admission using an authenticated lease ID; reject above six total or two per client.
    pub(crate) fn admit_stream(self: &Arc<Self>, client_id: &str) -> Result<StreamPermit> {
        let mut state = self.streams.lock().unwrap();
        ensure!(
            state.waiting.len() + state.active.len() < 6,
            "image stream admission capacity busy"
        );
        let owned = state
            .waiting
            .iter()
            .filter(|(_, client)| client == client_id)
            .count()
            + state
                .active
                .values()
                .filter(|client| client.as_str() == client_id)
                .count();
        ensure!(owned < 2, "client image stream capacity busy");
        let ticket = state.next;
        state.next = state
            .next
            .checked_add(1)
            .context("image stream ticket exhausted")?;
        state.waiting.push_back((ticket, client_id.to_owned()));
        Ok(StreamPermit {
            pool: self.clone(),
            ticket,
        })
    }
    fn retire(&self, incarnation: uuid::Uuid) {
        let mut state = self.state.lock().unwrap();
        for (key, job) in &state.inflight {
            if key.incarnation == incarnation
                && let Some(job) = job.upgrade()
            {
                job.cancelled.store(true, Ordering::Release);
                job.changed.notify_all();
            }
        }
        state
            .cache
            .retain(|(key, _)| key.incarnation != incarnation);
        state.bytes = state
            .cache
            .iter()
            .map(|(_, encoded)| encoded.retained_bytes)
            .sum();
    }
    /// Revalidate authority and reuse/join bounded preparation; reject retired resources or exhausted capacity.
    pub(crate) fn request(&self, observation: &Observation) -> Result<Preparation> {
        observation.validate()?;
        let key = CacheKey {
            runtime: observation.client.runtime,
            incarnation: observation.incarnation,
            image: observation.descriptor.key,
        };
        let image = observation
            .image
            .upgrade()
            .context("image generation retired before preparation")?;
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state.cache.iter().position(|(cached, _)| *cached == key) {
            let entry = state.cache.remove(index).unwrap();
            let encoded = entry.1.clone();
            state.cache.push_back(entry);
            return Ok(Preparation(Arc::new(Job {
                result: Mutex::new(Some(Ok(encoded))),
                changed: Condvar::new(),
                cancelled: std::sync::atomic::AtomicBool::new(false),
            })));
        }
        state.inflight.retain(|_, job| job.strong_count() > 0);
        if let Some(job) = state.inflight.get(&key).and_then(Weak::upgrade) {
            return Ok(Preparation(job));
        }
        let length = image.pixels.capacity();
        ensure!(
            state.inflight.len() < PREPARATIONS && length <= INPUT_BYTES - state.inputs,
            "image preparation capacity busy"
        );
        let job = Arc::new(Job {
            result: Mutex::new(None),
            changed: Condvar::new(),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        });
        let work = Work {
            key,
            pane: observation.pane.clone(),
            image,
            job: job.clone(),
        };
        state.inputs += length;
        state.inflight.insert(key, Arc::downgrade(&job));
        if self.queue.try_send(work).is_err() {
            state.inputs -= length;
            state.inflight.remove(&key);
            anyhow::bail!("image preparation queue busy");
        }
        Ok(Preparation(job))
    }
    fn prepare(&self, work: Work) {
        let mut result = (|| -> Result<Arc<Encoded>> {
            ensure!(
                !work.job.cancelled.load(Ordering::Acquire)
                    && Arc::strong_count(&work.job) > 1
                    && current(&work),
                "image preparation cancelled or retired"
            );
            #[cfg(test)]
            {
                let barrier = self.before_encode.lock().unwrap().pop_front();
                if let Some(barrier) = barrier {
                    barrier.wait();
                    barrier.wait();
                }
            }
            let (encoding, encoded) = encode_image(&work.image)?;
            let retained_bytes = match &encoded {
                std::borrow::Cow::Owned(bytes) => bytes.capacity(),
                std::borrow::Cow::Borrowed(_) => work.image.pixels.capacity(),
            };
            let bytes = match encoded {
                std::borrow::Cow::Owned(bytes) => bytes::Bytes::from(bytes),
                std::borrow::Cow::Borrowed(_) => {
                    bytes::Bytes::from_owner(Pixels(work.image.clone()))
                }
            };
            self.encoded.fetch_add(1, Ordering::Relaxed);
            ensure!(
                !work.job.cancelled.load(Ordering::Acquire)
                    && Arc::strong_count(&work.job) > 1
                    && current(&work),
                "image preparation cancelled or retired"
            );
            Ok(Arc::new(Encoded {
                encoding,
                bytes,
                retained_bytes,
            }))
        })()
        .map_err(|error| format!("{error:#}"));
        let pane = work.pane.upgrade();
        let published = pane.as_ref().map(|pane| pane.published.lock().unwrap());
        let still_current = pane
            .as_ref()
            .is_some_and(|pane| !pane.forgotten.load(Ordering::Acquire))
            && published.as_ref().is_some_and(|published| {
                published
                    .frame
                    .iter()
                    .chain(published.scroll_frames.values())
                    .any(|frame| {
                        frame.images.iter().any(|image| {
                            image.key() == work.key.image && Arc::ptr_eq(image, &work.image)
                        })
                    })
            });
        let mut state = self.state.lock().unwrap();
        state.inputs -= work.image.pixels.capacity();
        state.inflight.remove(&work.key);
        if work.job.cancelled.load(Ordering::Acquire) || !still_current {
            result = Err("image preparation cancelled or retired".to_owned());
        }
        if let Ok(encoded) = &result {
            while state.cache.len() >= CACHE_ENTRIES
                || state.bytes + encoded.retained_bytes > CACHE_BYTES
            {
                let Some((_, retired)) = state.cache.pop_front() else {
                    break;
                };
                state.bytes -= retired.retained_bytes;
            }
            state.bytes += encoded.retained_bytes;
            state.cache.push_back((work.key, encoded.clone()));
        }
        *work.job.result.lock().unwrap() = Some(result);
        work.job.changed.notify_all();
    }
}
fn current(work: &Work) -> bool {
    let Some(pane) = work.pane.upgrade() else {
        return false;
    };
    if pane.forgotten.load(Ordering::Acquire) {
        return false;
    }
    let published = pane.published.lock().unwrap();
    published
        .frame
        .iter()
        .chain(published.scroll_frames.values())
        .any(|frame| {
            frame
                .images
                .iter()
                .any(|image| image.key() == work.key.image && Arc::ptr_eq(image, &work.image))
        })
}

#[cfg(test)]
pub(crate) mod tests;
