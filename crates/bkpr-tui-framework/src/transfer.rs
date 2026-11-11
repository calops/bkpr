//! Resource-backed Kitty uploads for explicitly registered frontend chrome.
//!
//! The terminal writer and APC demultiplexer remain owned by the application.
//! Only the existing chrome worker calls the preparation/service/shutdown APIs;
//! all other methods operate on metadata and never touch the filesystem.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bkpr_protocol::{Image, PixelFormat};
use std::{
    collections::VecDeque,
    ffi::CString,
    fmt::Write as _,
    fs::File,
    io::{self, Write},
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_SOURCES: usize = 64;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESOURCES: usize = MAX_SOURCES + 3;
const MAX_IN_FLIGHT: usize = 32;
const LEASE: Duration = Duration::from_secs(2);
const QUERY_START: u32 = 0x4000_0000;
const QUERY_END: u32 = 0x6fff_ffff;
static NEXT_QUERY: AtomicU32 = AtomicU32::new(QUERY_START);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Medium {
    Shared,
    Temporary,
    File,
}
impl Medium {
    const ALL: [Self; 3] = [Self::Shared, Self::Temporary, Self::File];
    fn code(self) -> char {
        match self {
            Self::Shared => 's',
            Self::Temporary => 't',
            Self::File => 'f',
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Shared => 0,
            Self::Temporary => 1,
            Self::File => 2,
        }
    }
}

// Deliberately no I/O Drop: these records may be released on the terminal thread.
// The worker owns cleanup, including during shutdown before other Arc owners die.
struct Resource {
    medium: Medium,
    name: CString,
    bytes: usize,
}
impl Resource {
    fn create(medium: Medium, pixels: &[u8]) -> io::Result<Self> {
        if medium == Medium::Shared {
            // macOS limits POSIX shm names to 31 bytes including the slash.
            let name = format!("/bkpr-{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
            let c_name = CString::new(name).expect("UUID contains no NUL");
            // SAFETY: a valid NUL-terminated private name and ordinary POSIX flags.
            let fd = unsafe {
                libc::shm_open(
                    c_name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: shm_open returned a fresh owned descriptor.
            let mut file = unsafe { File::from_raw_fd(fd) };
            let result = file
                .set_len(pixels.len() as u64)
                .and_then(|()| file.write_all(pixels));
            drop(file);
            let resource = Self {
                medium,
                name: c_name,
                bytes: pixels.len(),
            };
            if let Err(error) = result {
                resource.cleanup();
                return Err(error);
            }
            Ok(resource)
        } else {
            let mut file = tempfile::Builder::new()
                .prefix("tty-graphics-protocol-bkpr-")
                .tempfile()?;
            // Local-medium payloads are one APC command, limited to 4096
            // base64 bytes by KGP just like direct chunks.
            if file.path().as_os_str().as_bytes().len() > 3072 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "graphics resource path exceeds KGP payload limit",
                ));
            }
            let name = CString::new(file.path().as_os_str().as_bytes())
                .expect("filesystem path contains no NUL");
            file.write_all(pixels)?;
            let (file, _) = file.keep().map_err(|error| error.error)?;
            drop(file);
            Ok(Self {
                medium,
                name,
                bytes: pixels.len(),
            })
        }
    }
    fn cleanup(&self) {
        if self.medium == Medium::Shared {
            // SAFETY: this is our own unique NUL-terminated shm name. The host
            // may already have unlinked it; ENOENT is successful cleanup too.
            unsafe {
                libc::shm_unlink(self.name.as_ptr());
            }
        } else {
            let _ = std::fs::remove_file(std::path::Path::new(std::ffi::OsStr::from_bytes(
                self.name.as_bytes(),
            )));
        }
    }
    fn command(&self, action: char, id: u32, width: u32, height: u32, format: u8) -> Vec<u8> {
        let mut command = String::with_capacity(128 + self.name.as_bytes().len().div_ceil(3) * 4);
        write!(
            command,
            "\x1b_Ga={action},t={},f={format},s={width},v={height},i={id},q=0,S={};",
            self.medium.code(),
            self.bytes
        )
        .expect("writing into String cannot fail");
        BASE64.encode_string(self.name.as_bytes(), &mut command);
        command.push_str("\x1b\\");
        command.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceStatus {
    Waiting,
    Preparing,
    Ready,
    Used,
}
struct Source {
    image: Weak<Image>,
    bytes: usize,
    status: SourceStatus,
    ready: Option<Resource>,
}
struct Query {
    id: u32,
    medium: Medium,
    resource: Option<Resource>,
    deadline: Option<Instant>,
    done: bool,
}
struct Upload {
    id: u32,
    resource: Resource,
    deadline: Instant,
}
#[derive(Default)]
struct State {
    closed: bool,
    probes_requested: bool,
    probes_prepared: bool,
    evidence: [bool; 3],
    sources: VecDeque<Source>,
    queries: Vec<Query>,
    uploads: Vec<Upload>,
    cleanup: Vec<Resource>,
    failures: Vec<u32>,
    resource_count: usize,
    resource_bytes: usize,
    work: bool,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl State {
    fn reserve(&mut self, bytes: usize) -> bool {
        if self.resource_count >= MAX_RESOURCES || bytes > MAX_BYTES + 9 - self.resource_bytes {
            return false;
        }
        self.resource_count += 1;
        self.resource_bytes += bytes;
        true
    }
    fn release(&mut self, bytes: usize) {
        self.resource_count -= 1;
        self.resource_bytes -= bytes;
    }
    fn medium(&self) -> Option<Medium> {
        Medium::ALL
            .into_iter()
            .find(|medium| self.evidence[medium.index()])
    }
    fn wake(&mut self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.work = true;
        self.wake.clone()
    }
    fn retire_ready(&mut self) {
        for source in &mut self.sources {
            if let Some(resource) = source.ready.take() {
                self.cleanup.push(resource);
            }
            source.status = SourceStatus::Used;
        }
    }
    fn invalidate_medium(&mut self, medium: Medium) {
        self.evidence[medium.index()] = false;
        for source in &mut self.sources {
            if source
                .ready
                .as_ref()
                .is_some_and(|resource| resource.medium == medium)
            {
                if let Some(resource) = source.ready.take() {
                    self.cleanup.push(resource);
                }
                source.status = SourceStatus::Waiting;
            }
        }
    }
}

/// One TUI's bounded chrome transport cache and correlated transfer evidence.
///
/// Clones share ownership, but not with other TUI instances. Register only chrome
/// images on the existing worker. Exact Arc allocation identity (not protocol IDs
/// or scene aliases) determines eligibility; native pane images remain inline.
///
/// A single chrome worker serially performs all preparation and cleanup. The
/// combined ready/preparing/in-flight/cleanup budget is 67 resources and
/// 16 MiB + 9 bytes, with at most 64 weak source identities and 32 upload leases.
/// Query/upload leases expire after two seconds without keepalive polling.
#[derive(Clone, Default)]
pub struct ChromeTransfers {
    inner: Arc<Mutex<State>>,
}
impl ChromeTransfers {
    /// Create an owner without starting a thread or performing resource I/O.
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn notify(wake: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Some(wake) = wake {
            wake();
        }
    }
    /// Install a metadata-only wake callback for the existing chrome worker.
    /// Use a weak worker reference to avoid an ownership cycle. Called unlocked.
    pub fn set_worker_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.lock().wake = Some(wake);
    }
    /// Request one set of resource-backed medium probes. Worker-only; idempotent.
    pub fn prepare_probes(&self) -> bool {
        let mut state = self.lock();
        if state.closed || state.probes_prepared {
            return false;
        }
        state.probes_requested = true;
        state.work = true;
        drop(state);
        self.service()
    }
    /// Register and prepare an exact chrome Arc allocation on the chrome worker.
    /// Unsupported formats, exhausted budgets and absent evidence stay inline.
    pub fn prepare(&self, image: &Arc<Image>) {
        if !matches!(image.format, PixelFormat::Rgb | PixelFormat::Rgba)
            || image.pixels.is_empty()
            || image.pixels.len() > MAX_BYTES
        {
            return;
        }
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if !state
            .sources
            .iter()
            .any(|source| std::ptr::eq(source.image.as_ptr(), Arc::as_ptr(image)))
        {
            if state.sources.len() >= MAX_SOURCES {
                if let Some(index) = state.sources.iter().position(|source| {
                    source.image.strong_count() == 0 && source.status != SourceStatus::Preparing
                }) {
                    let source = state.sources.remove(index).expect("known index");
                    if let Some(resource) = source.ready {
                        state.cleanup.push(resource);
                    }
                } else {
                    return;
                }
            }
            state.sources.push_back(Source {
                image: Arc::downgrade(image),
                bytes: image.pixels.len(),
                status: SourceStatus::Waiting,
                ready: None,
            });
        }
        state.work = true;
        drop(state);
        self.service();
    }
    /// Whether metadata work has been queued for the chrome worker.
    pub fn has_worker_work(&self) -> bool {
        self.lock().work
    }
    /// Service queued cleanup and prepare eligible resources on the chrome worker.
    /// Never call on the input/render thread. No resource I/O holds the shared lock.
    /// Returns whether the terminal owner should drain commands or repaint.
    pub fn service(&self) -> bool {
        let (cleanup, probes, mut changed) = {
            let mut state = self.lock();
            let changed = state.work;
            state.work = false;
            for index in (0..state.sources.len()).rev() {
                if state.sources[index].image.strong_count() == 0
                    && state.sources[index].status != SourceStatus::Preparing
                {
                    let source = state.sources.remove(index).expect("known index");
                    if let Some(resource) = source.ready {
                        state.cleanup.push(resource);
                    }
                }
            }
            let probes = !state.closed && state.probes_requested && !state.probes_prepared;
            if probes {
                state.probes_prepared = true;
            }
            (std::mem::take(&mut state.cleanup), probes, changed)
        };
        for resource in cleanup {
            resource.cleanup();
            self.lock().release(resource.bytes);
        }
        if probes {
            for medium in Medium::ALL {
                if !self.lock().reserve(3) {
                    break;
                }
                let id = NEXT_QUERY
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                        (id <= QUERY_END).then(|| id + 1)
                    })
                    .ok();
                let Some(id) = id else {
                    self.lock().release(3);
                    continue;
                };
                let resource = Resource::create(medium, &[0, 0, 0])
                    .ok()
                    .map(|resource| (id, resource));
                let mut state = self.lock();
                if let Some((id, resource)) = resource {
                    if state.closed {
                        state.cleanup.push(resource);
                        state.work = true;
                    } else {
                        state.queries.push(Query {
                            id,
                            medium,
                            resource: Some(resource),
                            deadline: None,
                            done: false,
                        });
                        changed = true;
                    }
                } else {
                    state.release(3);
                }
            }
        }
        loop {
            let request = {
                let mut state = self.lock();
                if state.closed {
                    None
                } else if let Some(medium) = state.medium() {
                    let index = state
                        .sources
                        .iter()
                        .position(|source| source.status == SourceStatus::Waiting);
                    index.and_then(|index| {
                        let bytes = state.sources[index].bytes;
                        if !state.reserve(bytes) {
                            return None;
                        }
                        state.sources[index].status = SourceStatus::Preparing;
                        Some((state.sources[index].image.clone(), bytes, medium))
                    })
                } else {
                    None
                }
            };
            let Some((image, bytes, medium)) = request else {
                break;
            };
            let resource = image
                .upgrade()
                .and_then(|image| Resource::create(medium, &image.pixels).ok());
            let mut state = self.lock();
            let eligible = !state.closed && state.evidence[medium.index()];
            let source = state
                .sources
                .iter()
                .position(|source| std::ptr::eq(source.image.as_ptr(), image.as_ptr()));
            if let Some(resource) = resource {
                if eligible
                    && source
                        .is_some_and(|index| state.sources[index].status == SourceStatus::Preparing)
                {
                    let source = &mut state.sources[source.expect("eligible source")];
                    source.status = SourceStatus::Ready;
                    source.ready = Some(resource);
                    changed = true;
                } else {
                    if let Some(index) = source
                        && state.sources[index].status == SourceStatus::Preparing
                    {
                        state.sources[index].status = SourceStatus::Waiting;
                    }
                    state.cleanup.push(resource);
                    state.work = true;
                }
            } else {
                if let Some(index) = source {
                    state.sources[index].status = SourceStatus::Used;
                }
                state.release(bytes);
            }
        }
        // Retirement during unlocked creation (inline claim, disconnect or stale
        // evidence) is cleaned in this same worker turn, without another wake.
        let cleanup = std::mem::take(&mut self.lock().cleanup);
        for resource in cleanup {
            resource.cleanup();
            self.lock().release(resource.bytes);
        }
        changed
    }
    /// Drain prepared probe commands through the application's sole terminal writer.
    /// Call only after the independent direct graphics query succeeded. Dispatch
    /// starts bounded leases; an ambiguous writer failure must disconnect, not retry.
    pub fn take_query_commands(&self, now: Instant) -> Vec<Vec<u8>> {
        let mut state = self.lock();
        if state.closed {
            return Vec::new();
        }
        state
            .queries
            .iter_mut()
            .filter_map(|query| {
                if query.done || query.deadline.is_some() {
                    return None;
                }
                let command = query.resource.as_ref()?.command('q', query.id, 1, 1, 24);
                query.deadline = Some(now + LEASE);
                Some(command)
            })
            .collect()
    }
    /// Consume only exact, owned APC query/upload replies from the existing demux.
    /// Unknown IDs and placement ACKs remain available to the ordinary reply route.
    /// Late/duplicate owned query replies cannot create fresh medium evidence.
    pub fn consume_tagged(&self, bytes: &[u8], now: Instant) -> bool {
        let Some((id, status, plain)) = crate::render::parse_graphics_reply(bytes) else {
            return false;
        };
        if !plain
            || status.len() > 256
            || status.is_empty()
            || !status.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return false;
        }
        let mut state = self.lock();
        if state.closed {
            return false;
        }
        let consumed;
        if let Some(index) = state.queries.iter().position(|query| query.id == id) {
            consumed = true;
            let query = &mut state.queries[index];
            if !query.done && query.deadline.is_some_and(|deadline| now < deadline) {
                query.done = true;
                let medium = query.medium;
                let resource = query.resource.take();
                state.evidence[medium.index()] = status == "OK";
                if let Some(resource) = resource {
                    state.cleanup.push(resource);
                }
            }
        } else if let Some(index) = state.uploads.iter().position(|upload| upload.id == id) {
            consumed = true;
            let upload = state.uploads.swap_remove(index);
            if status != "OK" || now >= upload.deadline {
                state.invalidate_medium(upload.resource.medium);
                // Recover presentation with a NEW physical ID and inline bytes.
                // Never replay the old local command or claim a successful ACK.
                state.failures.push(id);
            }
            state.cleanup.push(upload.resource);
        } else {
            return false;
        }
        let wake = state.wake();
        drop(state);
        Self::notify(wake);
        consumed
    }
    /// Expire bounded leases and retire unconfirmed physical uploads.
    /// The next synchronized draw replaces only still-required sources inline,
    /// with new physical IDs. Resource cleanup itself remains worker-only.
    pub fn expire(&self, now: Instant) {
        let mut state = self.lock();
        let mut retired = Vec::new();
        for query in &mut state.queries {
            if !query.done && query.deadline.is_some_and(|deadline| now >= deadline) {
                query.done = true;
                if let Some(resource) = query.resource.take() {
                    retired.push(resource);
                }
            }
        }
        for index in (0..state.uploads.len()).rev() {
            if now >= state.uploads[index].deadline {
                let upload = state.uploads.swap_remove(index);
                state.invalidate_medium(upload.resource.medium);
                state.failures.push(upload.id);
                retired.push(upload.resource);
            }
        }
        if retired.is_empty() {
            return;
        }
        state.cleanup.extend(retired);
        let wake = state.wake();
        drop(state);
        Self::notify(wake);
    }
    /// Earliest in-flight lease deadline, with no periodic keepalive polling.
    pub fn deadline(&self) -> Option<Instant> {
        let state = self.lock();
        state
            .queries
            .iter()
            .filter(|query| !query.done)
            .filter_map(|query| query.deadline)
            .chain(state.uploads.iter().map(|upload| upload.deadline))
            .min()
    }
    /// Disconnect this TUI and queue every resource for worker cleanup.
    /// Main-thread safe and idempotent, including after worker shutdown.
    pub fn disconnect(&self) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        state.evidence = [false; 3];
        state.retire_ready();
        let resources: Vec<_> = state
            .queries
            .iter_mut()
            .filter_map(|query| {
                query.done = true;
                query.resource.take()
            })
            .collect();
        state.cleanup.extend(resources);
        let uploads = std::mem::take(&mut state.uploads);
        state
            .cleanup
            .extend(uploads.into_iter().map(|upload| upload.resource));
        let wake = state.wake();
        drop(state);
        Self::notify(wake);
    }
    /// Finish resource cleanup on the chrome worker before it exits.
    /// Disconnects all remaining clones first, so later guard drops cannot enqueue
    /// resources after the receiver is gone. Must run on every worker exit path.
    pub fn shutdown_worker(&self) {
        self.disconnect();
        self.service();
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn take_failures(&self) -> Vec<u32> {
        std::mem::take(&mut self.lock().failures)
    }
    pub(crate) fn note_inline(&self, image: &Image) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        let Some(source) = state
            .sources
            .iter_mut()
            .find(|source| std::ptr::eq(source.image.as_ptr(), image))
        else {
            return;
        };
        if source.status == SourceStatus::Used && source.ready.is_none() {
            return;
        }
        source.status = SourceStatus::Used;
        let resource = source.ready.take();
        if let Some(resource) = resource {
            state.cleanup.push(resource);
        }
        let wake = state.wake();
        drop(state);
        Self::notify(wake);
    }
    pub(crate) fn upload(&self, image: &Image, id: u32, now: Instant) -> Option<Vec<u8>> {
        let mut state = self.lock();
        if state.closed || state.uploads.len() >= MAX_IN_FLIGHT {
            return None;
        }
        let index = state.sources.iter().position(|source| {
            std::ptr::eq(source.image.as_ptr(), image) && source.status == SourceStatus::Ready
        })?;
        let medium = state.sources[index].ready.as_ref()?.medium;
        if !state.evidence[medium.index()] {
            return None;
        }
        let source = &mut state.sources[index];
        let resource = source.ready.take()?;
        source.status = SourceStatus::Used;
        let format = if image.format == PixelFormat::Rgb {
            24
        } else {
            32
        };
        let command = resource.command('t', id, image.width, image.height, format);
        state.uploads.push(Upload {
            id,
            resource,
            deadline: now + LEASE,
        });
        Some(command)
    }
}

pub(crate) fn reserved_id(id: u32) -> bool {
    (QUERY_START..0x8000_0000).contains(&id)
}

#[cfg(test)]
pub(crate) fn proven_file_for_test() -> ChromeTransfers {
    let transfers = ChromeTransfers::new();
    transfers.prepare_probes();
    let now = Instant::now();
    transfers.take_query_commands(now);
    let id = transfers
        .lock()
        .queries
        .iter()
        .find(|query| query.medium == Medium::File)
        .expect("regular file probe")
        .id;
    assert!(transfers.consume_tagged(format!("graphics;Gi={id};OK").as_bytes(), now));
    transfers.expire(now + LEASE);
    transfers.service();
    transfers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Worker(ChromeTransfers);
    impl Drop for Worker {
        fn drop(&mut self) {
            self.0.shutdown_worker();
        }
    }
    fn image(id: u32) -> Arc<Image> {
        Arc::new(Image {
            id,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgb,
            pixels: vec![5, 17, 29],
        })
    }
    fn file_name(transfers: &ChromeTransfers, pending: bool) -> PathBuf {
        let state = transfers.lock();
        let resource = if pending {
            &state.uploads[0].resource
        } else {
            state.sources[0].ready.as_ref().expect("prepared file")
        };
        PathBuf::from(std::ffi::OsStr::from_bytes(resource.name.as_bytes()))
    }

    #[test]
    fn resource_backed_proof_is_exact_correlated_and_separate_from_direct_query() {
        let worker = Worker(ChromeTransfers::new());
        let owner = &worker.0;
        assert!(owner.prepare_probes());
        assert!(!owner.prepare_probes());
        let now = Instant::now();
        let commands = owner.take_query_commands(now);
        assert!(!commands.is_empty());
        assert!(owner.take_query_commands(now).is_empty());
        let (id, resource_path) = {
            let state = owner.lock();
            let query = state
                .queries
                .iter()
                .find(|query| query.medium == Medium::File)
                .unwrap();
            (
                query.id,
                PathBuf::from(std::ffi::OsStr::from_bytes(
                    query.resource.as_ref().unwrap().name.as_bytes(),
                )),
            )
        };
        assert!(resource_path.exists());
        assert!(!owner.consume_tagged(b"graphics;Gi=999;OK", now));
        assert!(!owner.consume_tagged(format!("graphics;Gi={id},p=1;OK").as_bytes(), now));
        assert!(!owner.consume_tagged(format!("graphics;Gi=0{id};OK").as_bytes(), now));
        assert_eq!(owner.lock().evidence, [false; 3]);
        let mut direct = crate::GraphicsProbe::default();
        direct.begin();
        assert!(!owner.consume_tagged(b"graphics;Gi=1879048192;OK", now));
        assert!(!direct.consume_tagged(format!("graphics;Gi={id};OK").as_bytes()));
        assert!(owner.consume_tagged(format!("graphics;Gi={id};OK").as_bytes(), now));
        assert!(owner.lock().evidence[Medium::File.index()]);
        assert!(resource_path.exists(), "reply handling only queues cleanup");
        owner.service();
        assert!(!resource_path.exists());
        // A duplicate error cannot revoke previously completed query evidence.
        assert!(owner.consume_tagged(format!("graphics;Gi={id};ENOENT").as_bytes(), now));
        assert!(owner.lock().evidence[Medium::File.index()]);
    }

    #[test]
    fn only_the_registered_arc_is_eligible_even_with_identical_image_keys_and_pixels() {
        let worker = Worker(proven_file_for_test());
        let chrome = image(8);
        let pane = image(8);
        worker.0.prepare(&chrome);
        let now = Instant::now();
        assert!(worker.0.upload(&pane, 1001, now).is_none());
        let command = worker.0.upload(&chrome, 1002, now).unwrap();
        let text = std::str::from_utf8(&command).unwrap();
        assert!(text.starts_with("\x1b_Ga=t,t=f,f=24,s=1,v=1,i=1002,q=0,S=3;"));
        assert!(
            worker.0.upload(&chrome, 1003, now).is_none(),
            "one-shot resource must not be reused"
        );
        worker.0.service();
        assert!(worker.0.lock().sources[0].ready.is_none());
    }

    #[test]
    fn leases_retain_files_until_worker_cleanup_and_timeout_requests_new_id_recovery() {
        let worker = Worker(proven_file_for_test());
        let chrome = image(9);
        worker.0.prepare(&chrome);
        let now = Instant::now();
        let path = file_name(&worker.0, false);
        worker.0.upload(&chrome, 1001, now).unwrap();
        worker.0.expire(now + LEASE - Duration::from_millis(1));
        worker.0.service();
        assert!(path.exists(), "pending host read retains resource");
        assert_eq!(worker.0.deadline(), Some(now + LEASE));
        worker.0.expire(now + LEASE);
        assert!(path.exists(), "main expiry performs no unlink");
        assert_eq!(worker.0.deadline(), None);
        assert_eq!(
            worker.0.take_failures(),
            [1001],
            "missing ACK requires presentation recovery, never fabricated success"
        );
        worker.0.service();
        assert!(!path.exists());
        assert_eq!(worker.0.lock().resource_count, 0);
        assert_eq!(worker.0.lock().resource_bytes, 0);
        assert!(!worker.0.consume_tagged(b"graphics;Gi=1001;OK", now + LEASE));
    }

    #[test]
    fn missing_query_reply_never_proves_medium_and_late_reply_is_owned_but_inert() {
        let worker = Worker(ChromeTransfers::new());
        worker.0.prepare_probes();
        let now = Instant::now();
        worker.0.take_query_commands(now);
        let id = worker.0.lock().queries[0].id;
        worker.0.expire(now + LEASE);
        assert!(
            worker
                .0
                .consume_tagged(format!("graphics;Gi={id};OK").as_bytes(), now + LEASE)
        );
        assert_eq!(worker.0.lock().evidence, [false; 3]);
        worker.0.service();
        assert_eq!(worker.0.lock().resource_count, 0);
    }

    #[test]
    fn inline_claim_prevents_late_evidence_from_preparing_unused_payloads() {
        let worker = Worker(ChromeTransfers::new());
        let chrome = image(1);
        worker.0.prepare(&chrome);
        worker.0.note_inline(&chrome);
        worker.0.prepare_probes();
        let now = Instant::now();
        worker.0.take_query_commands(now);
        let id = worker
            .0
            .lock()
            .queries
            .iter()
            .find(|query| query.medium == Medium::File)
            .unwrap()
            .id;
        worker
            .0
            .consume_tagged(format!("graphics;Gi={id};OK").as_bytes(), now);
        worker.0.service();
        assert_eq!(worker.0.lock().sources[0].status, SourceStatus::Used);
        assert!(worker.0.lock().sources[0].ready.is_none());
        assert!(worker.0.upload(&chrome, 1001, now).is_none());
    }

    #[test]
    fn worker_shutdown_cleans_pending_leases_before_surviving_main_owner_drops() {
        let worker = Worker(proven_file_for_test());
        let owner = worker.0.clone();
        let chrome = image(1);
        owner.prepare(&chrome);
        owner.upload(&chrome, 1001, Instant::now()).unwrap();
        let path = file_name(&owner, true);
        assert!(path.exists());
        drop(worker);
        assert!(!path.exists());
        owner.disconnect();
        assert_eq!(owner.lock().resource_count, 0);
        assert!(owner.upload(&chrome, 1002, Instant::now()).is_none());
    }

    #[test]
    fn host_unlink_race_is_safe_and_all_counts_include_queued_cleanup() {
        let worker = Worker(proven_file_for_test());
        let chrome = image(1);
        worker.0.prepare(&chrome);
        worker.0.upload(&chrome, 1001, Instant::now()).unwrap();
        let path = file_name(&worker.0, true);
        std::fs::remove_file(&path).unwrap();
        assert!(
            worker
                .0
                .consume_tagged(b"graphics;Gi=1001;OK", Instant::now())
        );
        assert_eq!(
            worker.0.lock().resource_count,
            1,
            "queued cleanup still occupies budget"
        );
        worker.0.service();
        assert_eq!(worker.0.lock().resource_count, 0);
        assert_eq!(worker.0.lock().resource_bytes, 0);
    }

    #[test]
    fn ready_pending_and_cleanup_reservations_share_one_byte_and_count_budget() {
        let worker = Worker(proven_file_for_test());
        let images: Vec<_> = (0..MAX_SOURCES as u32 + 8).map(image).collect();
        for image in &images {
            worker.0.prepare(image);
        }
        assert_eq!(worker.0.lock().sources.len(), MAX_SOURCES);
        let now = Instant::now();
        for (index, image) in images.iter().enumerate() {
            let upload = worker.0.upload(image, 1000 + index as u32, now);
            assert_eq!(upload.is_some(), index < MAX_IN_FLIGHT);
        }
        let state = worker.0.lock();
        assert_eq!(state.uploads.len(), MAX_IN_FLIGHT);
        assert_eq!(state.resource_count, MAX_SOURCES);
        assert_eq!(state.resource_bytes, MAX_SOURCES * 3);
        drop(state);
        worker.0.disconnect();
        assert_eq!(worker.0.lock().resource_count, MAX_SOURCES);
        worker.0.service();
        assert_eq!(worker.0.lock().resource_count, 0);
    }

    #[test]
    fn payload_byte_budget_stays_reserved_through_pending_and_queued_cleanup() {
        let worker = Worker(proven_file_for_test());
        let large = Arc::new(Image {
            id: 1,
            generation: 1,
            width: 2048,
            height: 2048,
            format: PixelFormat::Rgba,
            pixels: vec![0; MAX_BYTES],
        });
        let small = Arc::new(Image {
            id: 2,
            generation: 1,
            width: 4,
            height: 1,
            format: PixelFormat::Rgb,
            pixels: vec![0; 12],
        });
        worker.0.prepare(&large);
        worker.0.prepare(&small);
        assert_eq!(worker.0.lock().resource_bytes, MAX_BYTES);
        let now = Instant::now();
        worker.0.upload(&large, 1001, now).unwrap();
        assert!(worker.0.upload(&small, 1002, now).is_none());
        worker.0.consume_tagged(b"graphics;Gi=1001;OK", now);
        assert_eq!(
            worker.0.lock().resource_bytes,
            MAX_BYTES,
            "queued unlink still owns the reservation"
        );
        assert!(worker.0.upload(&small, 1002, now).is_none());
        worker.0.service();
        assert_eq!(worker.0.lock().resource_bytes, 12);
        assert!(worker.0.upload(&small, 1002, now).is_some());
    }
}
