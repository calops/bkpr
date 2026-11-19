//! Endpoint-owned image reception, residency and truthful scene receipts.
use crate::transport::{Endpoint, ImageResourcePhase};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{IMAGE_MAX_BYTES, Image, ImageDescriptor, ImageKey};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const WORKERS: usize = 2;
const MAX_VIEWS: usize = 1024;
const MAX_VIEW_IMAGES: usize = 1024;
const INACTIVE_BYTES: usize = 8 * 1024 * 1024;
const INACTIVE_ENTRIES: usize = 64;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Reception state; only Ready is eligible for resource receipts or viewing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourcePhase {
    /// Waiting for fair, bounded bulk admission.
    Queued,
    /// Backend preparation has accepted this exact descriptor.
    Preparing,
    /// Receiving an announced encoded body.
    Receiving,
    /// End arrived; validation and decoding have not yet completed.
    Decoding,
    /// Complete immutable pixels are retained locally.
    Ready,
    /// Terminal failure; unchanged desired manifests never retry it.
    Failed,
}

/// Body-free latest resource state, retaining the original typed error chain.
#[derive(Clone)]
pub(crate) struct ResourceStatus {
    /// Pane-local immutable generation.
    pub key: ImageKey,
    /// Current bounded worker stage.
    pub phase: ResourcePhase,
    /// Validated encoded bytes received, not decoded or presented bytes.
    pub received: u64,
    /// Announced encoded length; unknown during preparation.
    pub total: Option<u64>,
    /// Terminal error with original typed rejection/outcome details.
    pub error: Option<Arc<anyhow::Error>>,
}

impl std::fmt::Debug for ResourceStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceStatus")
            .field("key", &self.key)
            .field("phase", &self.phase)
            .field("received", &self.received)
            .field("total", &self.total)
            .field("failed", &self.error.is_some())
            .finish()
    }
}

type View = (String, String);
type Key = (String, ImageKey);

struct Entry {
    descriptor: ImageDescriptor,
    desired: HashSet<View>,
    owner: Option<View>,
    ticket: u64,
    cancellation: CancellationToken,
    status: ResourceStatus,
    image: Option<Arc<Image>>,
    inactive_since: u64,
}

#[derive(Default)]
struct State {
    views: HashMap<View, Vec<ImageKey>>,
    round_robin: VecDeque<(View, usize)>,
    entries: HashMap<String, HashMap<ImageKey, Entry>>,
    serial: u64,
    dirty: bool,
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    available: Condvar,
    notify: UnixStream,
    endpoint: Endpoint,
}

impl Shared {
    fn notify(&self, state: &mut State) {
        if !state.dirty {
            state.dirty = true;
            let _ = (&self.notify).write(&[1]);
        }
    }
}

/// Cheap endpoint-scoped residency reader for background scene observations.
#[derive(Clone)]
pub(crate) struct Receipts(Arc<Shared>);

impl Receipts {
    /// Return only complete retained generations still desired by this exact view.
    pub(crate) fn received(&self, pane: &str, view: &str) -> Vec<ImageKey> {
        let state = self.0.state.lock().unwrap();
        if state.stopped || self.0.endpoint.is_revoked() {
            return Vec::new();
        }
        let identity = (pane.to_owned(), view.to_owned());
        state
            .views
            .get(&identity)
            .into_iter()
            .flatten()
            .copied()
            .filter(|key| {
                state
                    .entries
                    .get(pane)
                    .and_then(|entries| entries.get(key))
                    .is_some_and(|entry| {
                        entry.image.is_some() && entry.status.phase == ResourcePhase::Ready
                    })
            })
            .collect()
    }
}

/// One endpoint/session cache with two dedicated cancellable fetch workers.
/// Active manifests retain the protocol's 1024-image/64-MiB per-view support;
/// only unreferenced reuse is limited to 64 entries and 8 MiB. At most two
/// transfers reserve encoded and decoded bodies, each bounded to 64 MiB.
/// Dropping the manager cancels workers without joining on the UI thread.
/// External immutable image Arcs are active consumer pins, not inactive cache bytes.
pub(crate) struct Resources {
    shared: Arc<Shared>,
    wake: UnixStream,
    workers_started: bool,
}

impl Resources {
    /// Create an idle manager; workers and backend I/O begin only for desired images.
    pub(crate) fn new(endpoint: Endpoint) -> Result<Self> {
        let (wake, notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                available: Condvar::new(),
                notify,
                endpoint,
            }),
            wake,
            workers_started: false,
        })
    }

    fn start_workers(&mut self) -> Result<()> {
        let runtimes: Vec<_> = match (0..WORKERS)
            .map(|_| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
            })
            .collect::<std::io::Result<_>>()
        {
            Ok(runtimes) => runtimes,
            Err(error) => {
                self.stop_workers(
                    anyhow::Error::from(error).context("creating image worker runtime"),
                );
                anyhow::bail!("image resource workers unavailable");
            }
        };
        for (index, runtime) in runtimes.into_iter().enumerate() {
            let shared = self.shared.clone();
            if let Err(error) = thread::Builder::new()
                .name(format!("bkpr-images-{index}"))
                .spawn(move || fetch_worker(shared, runtime))
            {
                self.stop_workers(
                    anyhow::Error::from(error).context("starting image resource worker"),
                );
                anyhow::bail!("image resource workers unavailable");
            }
        }
        self.workers_started = true;
        Ok(())
    }

    fn stop_workers(&self, error: anyhow::Error) {
        let error = Arc::new(error);
        let mut state = self.shared.state.lock().unwrap();
        state.stopped = true;
        for entry in state
            .entries
            .values_mut()
            .flat_map(|entries| entries.values_mut())
        {
            entry.cancellation.cancel();
            entry.status.phase = ResourcePhase::Failed;
            entry.status.error = Some(error.clone());
        }
        self.shared.notify(&mut state);
        self.shared.available.notify_all();
    }

    /// Diff a validated desired manifest. The resident callback may only clone
    /// current frame Arcs: it runs under the manager lock and must not do I/O.
    /// Validated retained pixels seed missing entries before any fetch is queued.
    /// Removed jobs cancel; unchanged failed resources never automatically retry.
    pub(crate) fn sync_view(
        &mut self,
        pane: &str,
        view: &str,
        descriptors: &[ImageDescriptor],
        mut resident: impl FnMut(ImageKey) -> Option<Arc<Image>>,
    ) -> Result<()> {
        ensure!(
            !pane.is_empty() && !view.is_empty() && pane.len() <= 4096 && view.len() <= 4096,
            "image resource pane/view identity limit"
        );
        ensure!(
            descriptors.len() <= MAX_VIEW_IMAGES,
            "image resource manifest count limit"
        );
        let mut seen = HashSet::with_capacity(descriptors.len());
        let mut bytes = 0u64;
        for descriptor in descriptors {
            descriptor.validate()?;
            ensure!(
                seen.insert(descriptor.key),
                "duplicate image resource descriptor"
            );
            bytes = bytes
                .checked_add(descriptor.decoded_len)
                .context("image manifest byte overflow")?;
        }
        ensure!(
            bytes <= IMAGE_MAX_BYTES as u64,
            "image resource manifest byte limit"
        );
        let identity = (pane.to_owned(), view.to_owned());
        let mut state = self.shared.state.lock().unwrap();
        ensure!(!state.stopped, "image resource manager retired");
        ensure!(
            state.views.contains_key(&identity) || state.views.len() < MAX_VIEWS,
            "image resource view limit"
        );
        let mut resident_images = HashMap::new();
        for descriptor in descriptors {
            if let Some(entry) = state
                .entries
                .get(pane)
                .and_then(|entries| entries.get(&descriptor.key))
            {
                ensure!(
                    entry.descriptor == *descriptor,
                    "immutable image descriptor changed"
                );
            }
            if state
                .entries
                .get(pane)
                .and_then(|entries| entries.get(&descriptor.key))
                .is_none_or(|entry| entry.image.is_none())
                && let Some(image) = resident(descriptor.key)
            {
                descriptor.validate_image(&image)?;
                resident_images.insert(descriptor.key, image);
            }
        }
        let previous = state.views.insert(
            identity.clone(),
            descriptors.iter().map(|d| d.key).collect(),
        );
        if let Some(previous) = previous {
            for key in previous {
                if !seen.contains(&key)
                    && let Some(entry) = state
                        .entries
                        .get_mut(pane)
                        .and_then(|entries| entries.get_mut(&key))
                {
                    entry.desired.remove(&identity);
                }
            }
        } else {
            state.round_robin.push_back((identity.clone(), 0));
        }
        let pane_entries = state.entries.entry(pane.to_owned()).or_default();
        let mut queued = false;
        for descriptor in descriptors {
            let entry = pane_entries.entry(descriptor.key).or_insert_with(|| Entry {
                descriptor: *descriptor,
                desired: HashSet::new(),
                owner: None,
                ticket: 0,
                cancellation: CancellationToken::new(),
                status: ResourceStatus {
                    key: descriptor.key,
                    phase: ResourcePhase::Queued,
                    received: 0,
                    total: None,
                    error: None,
                },
                image: None,
                inactive_since: 0,
            });
            if let Some(image) = resident_images.remove(&descriptor.key) {
                entry.cancellation.cancel();
                entry.ticket = entry.ticket.wrapping_add(1);
                entry.owner = None;
                entry.image = Some(image);
                entry.status.phase = ResourcePhase::Ready;
                entry.status.received = 0;
                entry.status.total = None;
                entry.status.error = None;
            }
            entry.desired.insert(identity.clone());
            queued |= entry.status.phase == ResourcePhase::Queued;
        }
        reconcile(&mut state);
        drop(state);
        if queued && !self.workers_started {
            self.start_workers()?;
        }
        self.shared.available.notify_all();
        Ok(())
    }

    /// Withdraw this view's authority and cancel its unneeded or reassigned jobs.
    pub(crate) fn retire_view(&mut self, pane: &str, view: &str) {
        let identity = (pane.to_owned(), view.to_owned());
        let mut state = self.shared.state.lock().unwrap();
        if let Some(keys) = state.views.remove(&identity) {
            for key in keys {
                if let Some(entry) = state
                    .entries
                    .get_mut(pane)
                    .and_then(|entries| entries.get_mut(&key))
                {
                    entry.desired.remove(&identity);
                }
            }
            state
                .round_robin
                .retain(|(current, _)| current != &identity);
            reconcile(&mut state);
            self.shared.available.notify_all();
        }
    }

    /// Share ready immutable pixels without copying; pending/failed keys return None.
    pub(crate) fn image(&self, pane: &str, key: ImageKey) -> Option<Arc<Image>> {
        if self.shared.endpoint.is_revoked() {
            return None;
        }
        self.shared
            .state
            .lock()
            .unwrap()
            .entries
            .get(pane)
            .and_then(|entries| entries.get(&key))
            .and_then(|entry| entry.image.clone())
    }

    /// Return complete retained generations for one current authorized view.
    pub(crate) fn received(&self, pane: &str, view: &str) -> Vec<ImageKey> {
        self.receipts().received(pane, view)
    }

    /// Snapshot latest bounded phase/progress/error metadata, never image bytes.
    pub(crate) fn statuses(&self, pane: &str, view: &str) -> Vec<ResourceStatus> {
        let state = self.shared.state.lock().unwrap();
        state
            .views
            .get(&(pane.to_owned(), view.to_owned()))
            .into_iter()
            .flatten()
            .filter_map(|key| {
                state
                    .entries
                    .get(pane)
                    .and_then(|entries| entries.get(key))
                    .map(|entry| entry.status.clone())
            })
            .collect()
    }

    /// Drain coalesced wake readiness without blocking or decoding on the UI thread.
    pub(crate) fn poll(&mut self) -> Result<bool> {
        let mut bytes = [0; 128];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        let mut state = self.shared.state.lock().unwrap();
        Ok(std::mem::take(&mut state.dirty))
    }

    /// Pollable descriptor for meaningful phase changes and coalesced progress.
    pub(crate) fn wake(&self) -> &UnixStream {
        &self.wake
    }

    /// Share only the scoped residency reader with background scene observers.
    pub(crate) fn receipts(&self) -> Receipts {
        Receipts(self.shared.clone())
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.stopped = true;
        for entry in state.entries.values().flat_map(|entries| entries.values()) {
            entry.cancellation.cancel();
        }
        state.entries.clear();
        state.views.clear();
        state.round_robin.clear();
        self.shared.available.notify_all();
    }
}

fn reconcile(state: &mut State) {
    state.serial = state.serial.wrapping_add(1);
    for entry in state
        .entries
        .values_mut()
        .flat_map(|entries| entries.values_mut())
    {
        if entry
            .owner
            .as_ref()
            .is_some_and(|owner| !entry.desired.contains(owner))
        {
            entry.cancellation.cancel();
            entry.owner = None;
            entry.ticket = entry.ticket.wrapping_add(1);
            if entry.image.is_none() && entry.status.phase != ResourcePhase::Failed {
                entry.status.phase = ResourcePhase::Queued;
                entry.status.received = 0;
                entry.status.total = None;
                entry.cancellation = CancellationToken::new();
            }
        }
        if entry.desired.is_empty() && entry.inactive_since == 0 {
            entry.inactive_since = state.serial;
        } else if !entry.desired.is_empty() {
            entry.inactive_since = 0;
        }
    }
    state.entries.retain(|_, entries| {
        entries.retain(|_, entry| !entry.desired.is_empty() || entry.image.is_some());
        !entries.is_empty()
    });
    let mut inactive: Vec<_> = state
        .entries
        .iter()
        .flat_map(|(pane, entries)| {
            entries
                .iter()
                .filter(|(_, entry)| entry.desired.is_empty())
                .map(move |(key, entry)| {
                    (
                        (pane.clone(), *key),
                        entry.inactive_since,
                        entry.image.as_ref().map_or(0, |image| image.pixels.len()),
                    )
                })
        })
        .collect();
    let mut bytes: usize = inactive.iter().map(|(_, _, bytes)| bytes).sum();
    let mut count = inactive.len();
    inactive.sort_unstable_by_key(|(_, age, _)| *age);
    for (key, _, size) in inactive {
        if bytes <= INACTIVE_BYTES && count <= INACTIVE_ENTRIES {
            break;
        }
        if let Some(entries) = state.entries.get_mut(&key.0) {
            entries.remove(&key.1);
        }
        bytes -= size;
        count -= 1;
    }
    state.entries.retain(|_, entries| !entries.is_empty());
}

struct Job {
    key: Key,
    view: String,
    descriptor: ImageDescriptor,
    ticket: u64,
    cancellation: CancellationToken,
}

fn next_job(state: &mut State) -> Option<Job> {
    for _ in 0..state.round_robin.len() {
        let (view, cursor) = state.round_robin.pop_front()?;
        state.round_robin.push_back((view.clone(), cursor));
        let keys = state.views.get(&view)?;
        for offset in 0..keys.len() {
            let index = (cursor + offset) % keys.len();
            let key = keys[index];
            let entry = state.entries.get_mut(&view.0)?.get_mut(&key)?;
            if entry.status.phase != ResourcePhase::Queued || entry.owner.is_some() {
                continue;
            }
            entry.ticket = entry.ticket.wrapping_add(1);
            entry.owner = Some(view.clone());
            if let Some((_, cursor)) = state.round_robin.back_mut() {
                *cursor = (index + 1) % keys.len();
            }
            return Some(Job {
                key: (view.0.clone(), key),
                view: view.1.clone(),
                descriptor: entry.descriptor,
                ticket: entry.ticket,
                cancellation: entry.cancellation.clone(),
            });
        }
    }
    None
}

fn current<'a>(state: &'a mut State, job: &Job) -> Option<&'a mut Entry> {
    if state.stopped || job.cancellation.is_cancelled() {
        return None;
    }
    state
        .entries
        .get_mut(&job.key.0)
        .and_then(|entries| entries.get_mut(&job.key.1))
        .filter(|entry| {
            entry.ticket == job.ticket
                && entry
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.1 == job.view)
                && !entry.desired.is_empty()
        })
}

fn fetch_worker(shared: Arc<Shared>, runtime: tokio::runtime::Runtime) {
    loop {
        let job = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.stopped {
                    return;
                }
                if let Some(job) = next_job(&mut state) {
                    break job;
                }
                state = shared.available.wait(state).unwrap();
            }
        };
        let mut last_wake = Instant::now();
        let result = runtime.block_on(shared.endpoint.image_resource(
            &job.key.0,
            &job.view,
            &job.descriptor,
            &job.cancellation,
            |resource| {
                let mut state = shared.state.lock().unwrap();
                let Some(entry) = current(&mut state, &job) else {
                    return;
                };
                let previous = entry.status.phase;
                entry.status.phase = match resource.phase {
                    ImageResourcePhase::Preparing => ResourcePhase::Preparing,
                    ImageResourcePhase::Receiving => ResourcePhase::Receiving,
                    ImageResourcePhase::Decoding => ResourcePhase::Decoding,
                };
                entry.status.received = resource.received;
                entry.status.total = resource.total;
                if previous != entry.status.phase || last_wake.elapsed() >= PROGRESS_INTERVAL {
                    last_wake = Instant::now();
                    shared.notify(&mut state);
                }
            },
        ));
        let mut state = shared.state.lock().unwrap();
        if let Some(entry) = current(&mut state, &job) {
            match result {
                Ok(image) if !shared.endpoint.is_revoked() => {
                    entry.image = Some(Arc::new(image));
                    entry.status.phase = ResourcePhase::Ready;
                }
                Ok(_) => {
                    entry.status.phase = ResourcePhase::Failed;
                    entry.status.error =
                        Some(Arc::new(anyhow::anyhow!("image attachment revoked")));
                }
                Err(error) => {
                    entry.status.phase = ResourcePhase::Failed;
                    entry.status.error = Some(Arc::new(error));
                }
            }
            shared.notify(&mut state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::test_wire::{read_request, write_response};
    use bkpr_protocol::{
        ImageEncoding, ImageResource, Operation, PROTOCOL_VERSION, PaneRequest, Payload,
        PixelFormat, Response,
    };
    use std::{
        os::{fd::AsRawFd, unix::net::UnixListener},
        sync::mpsc,
    };

    fn descriptor(generation: u64) -> ImageDescriptor {
        ImageDescriptor {
            key: ImageKey { id: 7, generation },
            width: 1,
            height: 1,
            format: PixelFormat::Rgb,
            decoded_len: 3,
        }
    }

    fn send(stream: &mut UnixStream, resource: ImageResource) {
        write_response(
            stream,
            &Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(Payload::ImageResource(resource)),
                error: None,
                failure: None,
            },
        )
        .unwrap();
    }

    fn start(stream: &mut UnixStream, descriptor: ImageDescriptor) {
        send(stream, ImageResource::Preparing { descriptor });
        send(
            stream,
            ImageResource::Start {
                descriptor,
                encoding: ImageEncoding::Raw,
                encoded_len: 3,
            },
        );
        send(
            stream,
            ImageResource::Data {
                key: descriptor.key,
                offset: 0,
                data: vec![1, 2, 3].into(),
            },
        );
    }

    fn end(stream: &mut UnixStream, descriptor: ImageDescriptor) {
        send(
            stream,
            ImageResource::End {
                key: descriptor.key,
                encoded_len: 3,
            },
        );
    }

    fn request(listener: &UnixListener, expected: ImageKey) -> UnixStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let request = read_request(&mut stream).unwrap();
        assert!(matches!(request.operation, Operation::Pane {
            pane_id, request: PaneRequest::ImageResource { view_id, key },
        } if pane_id == "pane" && view_id == "view" && key == expected));
        stream
    }

    fn until(resources: &mut Resources, phase: ResourcePhase) {
        loop {
            resources.poll().unwrap();
            if resources
                .statuses("pane", "view")
                .iter()
                .any(|status| status.phase == phase)
            {
                return;
            }
            let mut fd = libc::pollfd {
                fd: resources.wake().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: fd points to one initialized pollfd for the duration of poll.
            assert_eq!(
                unsafe { libc::poll(&mut fd, 1, 3000) },
                1,
                "resource wake timed out"
            );
        }
    }

    #[test]
    fn multiframe_completion_receipts_warm_reuse_and_drop_are_truthful() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resource.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let desc = descriptor(1);
        let (finish, finish_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = request(&listener, desc.key);
            start(&mut stream, desc);
            finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            end(&mut stream, desc);
            listener
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        let receipts = resources.receipts();
        resources
            .sync_view("pane", "view", &[desc], |_| None)
            .unwrap();
        until(&mut resources, ResourcePhase::Receiving);
        assert!(receipts.received("pane", "view").is_empty());
        assert!(resources.image("pane", desc.key).is_none());
        finish.send(()).unwrap();
        until(&mut resources, ResourcePhase::Ready);
        let pinned = resources.image("pane", desc.key).unwrap();
        assert_eq!(pinned.pixels, [1, 2, 3]);
        assert_eq!(resources.received("pane", "view"), [desc.key]);
        resources.retire_view("pane", "view");
        assert!(receipts.received("pane", "view").is_empty());
        resources
            .sync_view("pane", "view", &[desc], |_| None)
            .unwrap();
        assert_eq!(resources.received("pane", "view"), [desc.key]);
        assert!(Arc::ptr_eq(
            &pinned,
            &resources.image("pane", desc.key).unwrap()
        ));
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        drop(resources);
        assert!(receipts.received("pane", "view").is_empty());
        assert_eq!(pinned.pixels, [1, 2, 3]);
    }

    #[test]
    fn truncated_transfer_is_terminal_under_an_unchanged_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resource.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let desc = descriptor(1);
        let server = thread::spawn(move || {
            let mut stream = request(&listener, desc.key);
            start(&mut stream, desc);
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        resources
            .sync_view("pane", "view", &[desc], |_| None)
            .unwrap();
        until(&mut resources, ResourcePhase::Failed);
        server.join().unwrap();
        for _ in 0..3 {
            resources
                .sync_view("pane", "view", &[desc], |_| None)
                .unwrap();
        }
        assert!(resources.received("pane", "view").is_empty());
        assert!(resources.image("pane", desc.key).is_none());
        assert!(resources.statuses("pane", "view")[0].error.is_some());
        assert!(next_job(&mut resources.shared.state.lock().unwrap()).is_none());
    }

    #[test]
    fn generation_replacement_cancels_old_stream_without_installing_or_acknowledging_it() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resource.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let old = descriptor(1);
        let new = descriptor(2);
        let (started, started_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stale = request(&listener, old.key);
            send(&mut stale, ImageResource::Preparing { descriptor: old });
            started.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(stale.read(&mut byte).unwrap(), 0);
            let mut fresh = request(&listener, new.key);
            start(&mut fresh, new);
            end(&mut fresh, new);
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        resources
            .sync_view("pane", "view", &[old], |_| None)
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        resources
            .sync_view("pane", "view", &[new], |_| None)
            .unwrap();
        until(&mut resources, ResourcePhase::Ready);
        assert!(resources.image("pane", old.key).is_none());
        assert_eq!(resources.received("pane", "view"), [new.key]);
        assert_eq!(resources.image("pane", new.key).unwrap().pixels, [1, 2, 3]);
        server.join().unwrap();
    }

    fn entry(desc: ImageDescriptor, views: &[View], image: Option<Arc<Image>>, age: u64) -> Entry {
        Entry {
            descriptor: desc,
            desired: views.iter().cloned().collect(),
            owner: None,
            ticket: 0,
            cancellation: CancellationToken::new(),
            status: ResourceStatus {
                key: desc.key,
                phase: if image.is_some() {
                    ResourcePhase::Ready
                } else {
                    ResourcePhase::Queued
                },
                received: 0,
                total: None,
                error: None,
            },
            image,
            inactive_since: age,
        }
    }

    #[test]
    fn queued_views_are_round_robin_and_late_cancelled_jobs_cannot_be_current() {
        let a: View = ("pane-a".into(), "view-a".into());
        let b: View = ("pane-b".into(), "view-b".into());
        let first = descriptor(1);
        let second = descriptor(2);
        let mut state = State::default();
        state.views.insert(a.clone(), vec![first.key, second.key]);
        state.views.insert(b.clone(), vec![first.key]);
        state.round_robin.extend([(a.clone(), 0), (b.clone(), 0)]);
        state
            .entries
            .entry(a.0.clone())
            .or_default()
            .insert(first.key, entry(first, std::slice::from_ref(&a), None, 0));
        state
            .entries
            .entry(a.0.clone())
            .or_default()
            .insert(second.key, entry(second, std::slice::from_ref(&a), None, 0));
        state
            .entries
            .entry(b.0.clone())
            .or_default()
            .insert(first.key, entry(first, std::slice::from_ref(&b), None, 0));
        let old = next_job(&mut state).unwrap();
        assert_eq!(old.key.0, a.0);
        assert_eq!(next_job(&mut state).unwrap().key.0, b.0);
        let replaced = state
            .entries
            .get_mut(&old.key.0)
            .unwrap()
            .get_mut(&old.key.1)
            .unwrap();
        replaced.cancellation.cancel();
        replaced.cancellation = CancellationToken::new();
        replaced.owner = None;
        replaced.ticket += 1;
        assert_eq!(next_job(&mut state).unwrap().key.1, second.key);
        assert_eq!(next_job(&mut state).unwrap().key, old.key);
        assert!(next_job(&mut state).is_none());
        state
            .entries
            .get_mut(&old.key.0)
            .unwrap()
            .get_mut(&old.key.1)
            .unwrap()
            .desired
            .clear();
        reconcile(&mut state);
        assert!(old.cancellation.is_cancelled());
        assert!(current(&mut state, &old).is_none());
    }

    #[test]
    fn inactive_cache_bounds_do_not_cap_active_images_or_external_pins() {
        let mut state = State::default();
        for generation in 1..=65 {
            let desc = descriptor(generation);
            let image = Arc::new(Image {
                id: desc.key.id,
                generation,
                width: 1,
                height: 1,
                format: PixelFormat::Rgb,
                pixels: vec![1, 2, 3],
            });
            state
                .entries
                .entry("pane".into())
                .or_default()
                .insert(desc.key, entry(desc, &[], Some(image), generation));
        }
        reconcile(&mut state);
        assert_eq!(state.entries["pane"].len(), INACTIVE_ENTRIES);
        assert!(!state.entries["pane"].contains_key(&descriptor(1).key));
        let pixels = vec![5; INACTIVE_BYTES + 1];
        let image = Arc::new(Image {
            id: 99,
            generation: 1,
            width: pixels.len() as u32,
            height: 1,
            format: PixelFormat::Gray,
            pixels,
        });
        let desc = ImageDescriptor::from_image(&image).unwrap();
        let view: View = ("pane".into(), "view".into());
        let key: Key = ("pane".into(), desc.key);
        state
            .entries
            .entry(key.0.clone())
            .or_default()
            .insert(key.1, entry(desc, &[view], Some(image.clone()), 0));
        reconcile(&mut state);
        assert!(state.entries["pane"].contains_key(&key.1));
        state
            .entries
            .get_mut(&key.0)
            .unwrap()
            .get_mut(&key.1)
            .unwrap()
            .desired
            .clear();
        reconcile(&mut state);
        assert!(
            !state
                .entries
                .get("pane")
                .is_some_and(|entries| entries.contains_key(&key.1))
        );
        assert_eq!(image.pixels.len(), INACTIVE_BYTES + 1);
    }

    #[test]
    fn retiring_view_cancels_a_pending_stream_and_withdraws_its_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resource.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let desc = descriptor(1);
        let (started, started_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = request(&listener, desc.key);
            send(&mut stream, ImageResource::Preparing { descriptor: desc });
            started.send(()).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        resources
            .sync_view("pane", "view", &[desc], |_| None)
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        resources.retire_view("pane", "view");
        server.join().unwrap();
        assert!(resources.received("pane", "view").is_empty());
        assert!(resources.image("pane", desc.key).is_none());
        assert!(resources.statuses("pane", "view").is_empty());
    }

    #[test]
    fn current_frame_pin_reseeds_an_evicted_large_image_without_refetching() {
        let pixels = vec![5; 32 * 1024 * 1024];
        let pinned = Arc::new(Image {
            id: 99,
            generation: 1,
            width: pixels.len() as u32,
            height: 1,
            format: PixelFormat::Gray,
            pixels,
        });
        let descriptor = ImageDescriptor::from_image(&pinned).unwrap();
        let mut resources =
            Resources::new(Endpoint::unmanaged(std::path::Path::new("unused.sock"))).unwrap();
        resources
            .sync_view("pane", "view", &[descriptor], |key| {
                assert_eq!(key, pinned.key());
                Some(pinned.clone())
            })
            .unwrap();
        assert_eq!(resources.received("pane", "view"), [descriptor.key]);
        resources
            .sync_view("pane", "view", &[descriptor], |_| {
                panic!("already retained")
            })
            .unwrap();
        resources
            .sync_view("pane", "view", &[], |_| panic!("no desired images"))
            .unwrap();
        assert!(resources.image("pane", descriptor.key).is_none());
        assert!(resources.received("pane", "view").is_empty());
        resources
            .sync_view("pane", "view", &[descriptor], |_| Some(pinned.clone()))
            .unwrap();
        assert!(Arc::ptr_eq(
            &pinned,
            &resources.image("pane", descriptor.key).unwrap()
        ));
        assert_eq!(resources.received("pane", "view"), [descriptor.key]);
        assert!(next_job(&mut resources.shared.state.lock().unwrap()).is_none());
    }

    #[test]
    fn invalid_resident_generation_does_not_mutate_the_desired_manifest_or_receipts() {
        let pinned = Arc::new(Image {
            id: 7,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgb,
            pixels: vec![1, 2, 3],
        });
        let original = descriptor(1);
        let replacement = descriptor(2);
        let mut resources =
            Resources::new(Endpoint::unmanaged(std::path::Path::new("unused.sock"))).unwrap();
        resources
            .sync_view("pane", "view", &[original], |_| Some(pinned.clone()))
            .unwrap();
        assert!(
            resources
                .sync_view("pane", "view", &[replacement], |_| Some(pinned.clone()))
                .is_err()
        );
        assert_eq!(resources.received("pane", "view"), [original.key]);
        assert!(Arc::ptr_eq(
            &pinned,
            &resources.image("pane", original.key).unwrap()
        ));
        assert!(resources.image("pane", replacement.key).is_none());
    }
}
