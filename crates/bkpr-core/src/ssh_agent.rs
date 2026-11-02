//! A process-local, opaque SSH-agent transport. Only attachment selection knows
//! authority; this worker never parses agent messages or discovers another route.
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender};
use mio::{
    Events, Interest, Poll, Registry, Token, Waker,
    net::{UnixListener, UnixStream},
};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// These are transport resource bounds, not agent packet or signing deadlines.
const MAX_CONNECTIONS: usize = 32;
const BUFFER_BYTES: usize = 32 * 1024;
const TURN_BYTES: usize = 32 * 1024;
const TURN_OPERATIONS: usize = 16;
const ACCEPTS_PER_TURN: usize = 16;
const ADMISSION_WORKERS: usize = 2;
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const LISTENER: Token = Token(0);
const WAKE: Token = Token(1);

pub(crate) struct Source {
    pub(crate) path: PathBuf,
    /// Nonblocking, and must not acquire registry/client/relay locks. The owner
    /// also publishes a new selection when an idle lease dies; no idle polling
    /// or approval timeout is necessary in the transport.
    pub(crate) live: Arc<dyn Fn() -> bool + Send + Sync>,
}

struct Selection {
    sequence: Option<u64>,
    source: Option<Arc<Source>>,
    stopping: bool,
    failure: Option<String>,
}

struct Shared {
    // The sole publication/send fence. Held only for liveness checks and one
    // nonblocking syscall, never across poll, path lookup, connect or join.
    selection: Mutex<Selection>,
    wake: Waker,
}

impl Shared {
    fn io<T>(
        &self,
        sequence: u64,
        source: &Source,
        f: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let selected = self.selection.lock().unwrap();
        if selected.stopping || selected.sequence != Some(sequence) || !(source.live)() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "agent attachment retired",
            ));
        }
        f()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

struct OwnedSocket {
    path: PathBuf,
    identity: Identity,
    cleaned: AtomicBool,
}

impl OwnedSocket {
    fn validate_source_path(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() || path.file_name().is_none() {
            bail!("agent source must be an absolute socket path");
        }
        if path == self.path {
            bail!("refusing recursive agent forwarding");
        }
        Ok(())
    }

    fn cleanup(&self) -> io::Result<()> {
        if self.cleaned.load(Ordering::Acquire) {
            return Ok(());
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata)
                if metadata.file_type().is_socket() && Identity::of(&metadata) == self.identity =>
            {
                fs::remove_file(&self.path)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Never re-examine a successfully retired pathname: even inode numbers
        // can be reused by a replacement after our lease has been released.
        self.cleaned.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

struct SocketLease(fs::File);

impl SocketLease {
    fn acquire(path: &Path) -> Result<Self> {
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(lock_path)
            .context("opening forwarding socket lease")?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("forwarding socket lease must be a private current-user-owned regular file");
        }
        fs2::FileExt::try_lock_exclusive(&file)
            .context("forwarding socket is owned by another daemon")?;
        Ok(Self(file))
    }
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        // A fork can retain the open description; close alone does not revoke
        // our lease while that child still holds its inherited descriptor.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn remove_stale(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspecting forwarding socket pathname"),
    };
    let safe = |metadata: &fs::Metadata| {
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
    };
    if !safe(&metadata) {
        bail!("refusing to replace a foreign, non-socket or non-private forwarding pathname");
    }
    match UnixStream::connect(path) {
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
        Ok(_) => bail!("forwarding socket already has a live or connecting listener"),
        Err(error) => return Err(error).context("refusing ambiguous forwarding socket recovery"),
    }
    let current = fs::symlink_metadata(path).context("rechecking stale forwarding socket")?;
    if !safe(&current) || Identity::of(&metadata) != Identity::of(&current) {
        bail!("forwarding socket changed during stale recovery");
    }
    fs::remove_file(path).context("removing lease-owned stale forwarding socket")
}

#[cfg(test)]
type AdmissionHook = Arc<dyn Fn(&Path) + Send + Sync>;

struct Admission {
    id: usize,
    sequence: u64,
    source: Arc<Source>,
    deadline: Instant,
}

impl Admission {
    fn check(&self, shared: &Shared) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "agent admission deadline elapsed",
            ));
        }
        shared.io(self.sequence, &self.source, || Ok(()))
    }

    fn connect(&self, shared: &Shared, own_identity: Identity) -> io::Result<UnixStream> {
        let validate = || {
            let metadata = fs::metadata(&self.source.path)?;
            if !metadata.file_type().is_socket() || Identity::of(&metadata) == own_identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid or recursive agent source",
                ));
            }
            Ok(())
        };
        self.check(shared)?;
        validate()?;
        self.check(shared)?;
        // Nonblocking socket operations still perform potentially blocking
        // pathname resolution. Only these fixed admission threads do that.
        let stream = UnixStream::connect(&self.source.path)?;
        validate()?;
        self.check(shared)?;
        Ok(stream)
    }
}

struct AdmissionResult {
    id: usize,
    sequence: u64,
    stream: io::Result<UnixStream>,
}

struct Admissions {
    jobs: Sender<Admission>,
    results: Receiver<AdmissionResult>,
}

impl Admissions {
    fn start(
        shared: Arc<Shared>,
        own_identity: Identity,
        #[cfg(test)] hook: Option<AdmissionHook>,
    ) -> io::Result<Self> {
        let (jobs, receive_jobs) = crossbeam_channel::bounded::<Admission>(MAX_CONNECTIONS);
        let (send_results, results) =
            crossbeam_channel::bounded::<AdmissionResult>(MAX_CONNECTIONS);
        for index in 0..ADMISSION_WORKERS {
            let receive_jobs = receive_jobs.clone();
            let send_results = send_results.clone();
            let shared = shared.clone();
            #[cfg(test)]
            let hook = hook.clone();
            // Deliberately detach: a kernel pathname lookup cannot be
            // cancelled or safely joined. There are never replacement workers
            // in this daemon-lifetime relay. Stuck threads own no downstream,
            // owned-socket guard, registry or client lock; at most two may
            // outlive shutdown until their lookup returns.
            let _ = thread::Builder::new()
                .name(format!("bkpr-agent-admit-{index}"))
                .spawn(move || {
                    while let Ok(job) = receive_jobs.recv() {
                        if shared.selection.lock().unwrap().stopping {
                            break;
                        }
                        #[cfg(test)]
                        if job.check(&shared).is_ok()
                            && let Some(hook) = &hook
                        {
                            hook(&job.source.path);
                        }
                        let stream = job.connect(&shared, own_identity);
                        if send_results
                            .send(AdmissionResult {
                                id: job.id,
                                sequence: job.sequence,
                                stream,
                            })
                            .is_err()
                        {
                            break;
                        }
                        let _ = shared.wake.wake();
                    }
                })?;
        }
        Ok(Self { jobs, results })
    }
}

struct PendingAdmission {
    downstream: UnixStream,
    sequence: u64,
    deadline: Instant,
}

pub(crate) struct Relay {
    socket: Arc<OwnedSocket>,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<io::Result<()>>>>,
    lease: Mutex<Option<SocketLease>>,
}

impl Relay {
    /// Own a dedicated advisory lease before probing/replacing a definitively
    /// stale private socket. A live or ambiguous listener is never replaced.
    pub(crate) fn bind(path: &Path) -> Result<Self> {
        Self::bind_inner(
            path,
            #[cfg(test)]
            None,
        )
    }

    fn bind_inner(path: &Path, #[cfg(test)] admission_hook: Option<AdmissionHook>) -> Result<Self> {
        if !path.is_absolute()
            || path.file_name().is_none()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            bail!("forwarding socket needs an absolute, non-traversing pathname");
        }
        let parent = path
            .parent()
            .context("forwarding socket needs a private parent")?;
        let parent_metadata =
            fs::symlink_metadata(parent).context("inspecting forwarding socket parent")?;
        if !parent_metadata.is_dir()
            || parent_metadata.uid() != unsafe { libc::geteuid() }
            || parent_metadata.mode() & 0o777 != 0o700
        {
            bail!("forwarding socket parent must be current-user-owned, non-symlink and mode 0700");
        }
        let lease = SocketLease::acquire(path)?;
        remove_stale(path)?;
        let mut listener = UnixListener::bind(path).context("binding forwarding socket")?;
        let metadata = fs::symlink_metadata(path).context("identifying owned forwarding socket")?;
        let socket = Arc::new(OwnedSocket {
            path: path.to_owned(),
            identity: Identity::of(&metadata),
            cleaned: AtomicBool::new(false),
        });
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        let shared = Arc::new(Shared {
            selection: Mutex::new(Selection {
                sequence: None,
                source: None,
                stopping: false,
                failure: None,
            }),
            wake: Waker::new(poll.registry(), WAKE)?,
        });
        let admissions = Admissions::start(
            shared.clone(),
            socket.identity,
            #[cfg(test)]
            admission_hook,
        )
        .context("starting bounded agent admission workers")?;
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("bkpr-agent-relay".into())
            .spawn(move || {
                let result = Reactor::new(poll, listener, worker_shared.clone(), admissions).run();
                let mut selected = worker_shared.selection.lock().unwrap();
                selected.stopping = true;
                if let Err(error) = &result {
                    selected.failure = Some(error.to_string());
                }
                result
            })
            .context("starting forwarding socket worker")?;
        Ok(Self {
            socket,
            shared,
            worker: Mutex::new(Some(worker)),
            lease: Mutex::new(Some(lease)),
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket.path
    }

    /// Delayed/equal publications are ignored. An invalid newer route still
    /// revokes its predecessor, but reports failure instead of selecting it.
    pub(crate) fn select(&self, sequence: u64, source: Option<Source>) -> Result<()> {
        {
            let selected = self.shared.selection.lock().unwrap();
            if selected.stopping {
                bail!("agent forwarding is stopped");
            }
            if selected.sequence.is_some_and(|current| sequence <= current) {
                return Ok(());
            }
        }
        // Pure lexical checks only. Filesystem/alias validation belongs to the
        // bounded admission workers and must never delay publication.
        let validation = source
            .as_ref()
            .map(|source| self.socket.validate_source_path(&source.path))
            .transpose();
        let mut selected = self.shared.selection.lock().unwrap();
        if selected.stopping {
            bail!("agent forwarding is stopped");
        }
        if selected.sequence.is_some_and(|current| sequence <= current) {
            return Ok(());
        }
        selected.sequence = Some(sequence);
        selected.source = if validation.is_ok() {
            source.map(Arc::new)
        } else {
            None
        };
        drop(selected);
        self.shared
            .wake
            .wake()
            .context("waking agent forwarding selection")?;
        validation.map(|_| ())
    }

    /// Wakes a blocked reactor, joins it, then removes only its original inode.
    pub(crate) fn shutdown(&self) -> Result<()> {
        self.shared.selection.lock().unwrap().stopping = true;
        let wake = self.shared.wake.wake();
        let mut worker = self.worker.lock().unwrap();
        let joined = worker.take().map(|worker| worker.join());
        let cleanup = self.socket.cleanup();
        if cleanup.is_ok() {
            self.lease.lock().unwrap().take();
        }
        if let Some(joined) = joined {
            joined.map_err(|_| anyhow::anyhow!("agent forwarding worker panicked"))??;
        }
        cleanup.context("removing owned forwarding socket")?;
        wake.context("waking agent forwarding shutdown")?;
        if let Some(failure) = &self.shared.selection.lock().unwrap().failure {
            bail!("agent forwarding failed: {failure}");
        }
        Ok(())
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Buffer {
    bytes: Box<[u8; BUFFER_BYTES]>,
    head: usize,
    len: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            bytes: Box::new([0; BUFFER_BYTES]),
            head: 0,
            len: 0,
        }
    }
}

struct Direction {
    buffer: Buffer,
    eof: bool,
    shut: bool,
}

impl Direction {
    fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            eof: false,
            shut: false,
        }
    }

    /// Each direction gets a byte and syscall quota, including interrupted
    /// syscalls. A quota-limited stream is rescheduled rather than waiting for
    /// another edge; a blocked stream waits for actual readiness.
    fn transfer(
        &mut self,
        from: &mut UnixStream,
        to: &mut UnixStream,
        shared: &Shared,
        sequence: u64,
        source: &Source,
    ) -> io::Result<bool> {
        let mut bytes_left = TURN_BYTES;
        let mut operations_left = TURN_OPERATIONS;
        while bytes_left != 0 && operations_left != 0 {
            let mut progressed = false;
            if self.buffer.len != 0 {
                let end = (self.buffer.head + self.buffer.len).min(BUFFER_BYTES);
                let end = end.min(self.buffer.head + bytes_left);
                operations_left -= 1;
                match shared.io(sequence, source, || {
                    to.write(&self.buffer.bytes[self.buffer.head..end])
                }) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "agent stream accepted zero bytes",
                        ));
                    }
                    Ok(count) => {
                        self.buffer.head = (self.buffer.head + count) % BUFFER_BYTES;
                        self.buffer.len -= count;
                        bytes_left -= count;
                        progressed = true;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            if self.eof && self.buffer.len == 0 {
                if !self.shut {
                    shared.io(sequence, source, || to.shutdown(Shutdown::Write))?;
                    self.shut = true;
                }
                return Ok(false);
            }
            if !self.eof
                && self.buffer.len < BUFFER_BYTES
                && bytes_left != 0
                && operations_left != 0
            {
                let tail = (self.buffer.head + self.buffer.len) % BUFFER_BYTES;
                let count = (BUFFER_BYTES - self.buffer.len)
                    .min(BUFFER_BYTES - tail)
                    .min(bytes_left);
                operations_left -= 1;
                match shared.io(sequence, source, || {
                    from.read(&mut self.buffer.bytes[tail..tail + count])
                }) {
                    Ok(0) => {
                        self.eof = true;
                        progressed = true;
                    }
                    Ok(count) => {
                        self.buffer.len += count;
                        bytes_left -= count;
                        progressed = true;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            if !progressed {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct Endpoint {
    stream: UnixStream,
    interest: Option<Interest>,
}

impl Endpoint {
    fn interest(
        &mut self,
        registry: &Registry,
        token: Token,
        interest: Option<Interest>,
    ) -> io::Result<()> {
        if self.interest == interest {
            return Ok(());
        }
        match (self.interest, interest) {
            (None, Some(interest)) => registry.register(&mut self.stream, token, interest)?,
            (Some(_), Some(interest)) => registry.reregister(&mut self.stream, token, interest)?,
            (Some(_), None) => registry.deregister(&mut self.stream)?,
            (None, None) => {}
        }
        self.interest = interest;
        Ok(())
    }
}

struct Connection {
    downstream: Endpoint,
    upstream: Endpoint,
    request: Direction,
    reply: Direction,
    connecting: Option<Instant>,
    queued: bool,
}

impl Connection {
    fn interests(&mut self, registry: &Registry, id: usize) -> io::Result<()> {
        if self.connecting.is_some() {
            self.downstream.interest(registry, Token(id), None)?;
            return self
                .upstream
                .interest(registry, Token(id + 1), Some(Interest::WRITABLE));
        }
        let interest = |read, write| match (read, write) {
            (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
            (true, false) => Some(Interest::READABLE),
            (false, true) => Some(Interest::WRITABLE),
            (false, false) => None,
        };
        self.downstream.interest(
            registry,
            Token(id),
            interest(
                !self.request.eof && self.request.buffer.len < BUFFER_BYTES,
                self.reply.buffer.len != 0,
            ),
        )?;
        self.upstream.interest(
            registry,
            Token(id + 1),
            interest(
                !self.reply.eof && self.reply.buffer.len < BUFFER_BYTES,
                self.request.buffer.len != 0,
            ),
        )
    }

    fn pump(&mut self, shared: &Shared, sequence: u64, source: &Source) -> io::Result<bool> {
        if let Some(deadline) = self.connecting {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "agent socket connect deadline elapsed",
                ));
            }
            shared
                .io(sequence, source, || {
                    if let Some(error) = self.upstream.stream.take_error()? {
                        return Err(error);
                    }
                    match self.upstream.stream.peer_addr() {
                        Ok(_) => Ok(true),
                        Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(false),
                        Err(error) => Err(error),
                    }
                })
                .map(|connected| {
                    if connected {
                        self.connecting = None;
                    }
                })?;
            if self.connecting.is_some() {
                return Ok(false);
            }
        }
        let request_more = self.request.transfer(
            &mut self.downstream.stream,
            &mut self.upstream.stream,
            shared,
            sequence,
            source,
        )?;
        let reply_more = self.reply.transfer(
            &mut self.upstream.stream,
            &mut self.downstream.stream,
            shared,
            sequence,
            source,
        )?;
        Ok(request_more || reply_more)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.downstream.stream.shutdown(Shutdown::Both);
        let _ = self.upstream.stream.shutdown(Shutdown::Both);
    }
}

struct Reactor {
    poll: Poll,
    listener: UnixListener,
    admissions: Admissions,
    pending: HashMap<usize, PendingAdmission>,
    shared: Arc<Shared>,
    connections: HashMap<usize, Connection>,
    ready: VecDeque<usize>,
    next_id: usize,
    sequence: Option<u64>,
    source: Option<Arc<Source>>,
    accepting: bool,
}

impl Reactor {
    fn new(
        poll: Poll,
        listener: UnixListener,
        shared: Arc<Shared>,
        admissions: Admissions,
    ) -> Self {
        Self {
            poll,
            listener,
            admissions,
            pending: HashMap::with_capacity(MAX_CONNECTIONS),
            shared,
            connections: HashMap::with_capacity(MAX_CONNECTIONS),
            ready: VecDeque::with_capacity(MAX_CONNECTIONS),
            next_id: 2,
            sequence: None,
            source: None,
            accepting: false,
        }
    }

    fn synchronize(&mut self) -> bool {
        let selected = self.shared.selection.lock().unwrap();
        if selected.stopping {
            return false;
        }
        let changed = self.sequence != selected.sequence;
        if changed {
            self.sequence = selected.sequence;
            self.source.clone_from(&selected.source);
        }
        drop(selected);
        if changed {
            self.connections.clear();
            self.pending.clear();
            self.ready.clear();
        }
        true
    }

    fn schedule(&mut self, id: usize) {
        if let Some(connection) = self.connections.get_mut(&id)
            && !connection.queued
        {
            connection.queued = true;
            self.ready.push_back(id);
        }
    }

    fn accept(&mut self) -> io::Result<()> {
        for _ in 0..ACCEPTS_PER_TURN {
            let downstream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.accepting = false;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let accepted_at = Instant::now();
            let (Some(source), Some(sequence)) = (&self.source, self.sequence) else {
                continue;
            };
            if self.connections.len() + self.pending.len() >= MAX_CONNECTIONS
                || self.shared.io(sequence, source, || Ok(())).is_err()
            {
                continue;
            }
            let Some(next_id) = self.next_id.checked_add(2) else {
                continue;
            };
            let id = self.next_id;
            self.next_id = next_id;
            let deadline = accepted_at + CONNECT_DEADLINE;
            let job = Admission {
                id,
                sequence,
                source: source.clone(),
                deadline,
            };
            if self.admissions.jobs.try_send(job).is_err() {
                continue;
            }
            self.pending.insert(
                id,
                PendingAdmission {
                    downstream,
                    sequence,
                    deadline,
                },
            );
        }
        Ok(())
    }

    fn complete_admissions(&mut self) {
        for _ in 0..MAX_CONNECTIONS {
            let result = match self.admissions.results.try_recv() {
                Ok(result) => result,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.pending.clear();
                    break;
                }
            };
            let Some(pending) = self.pending.remove(&result.id) else {
                continue;
            };
            let Some(source) = &self.source else {
                continue;
            };
            if pending.sequence != result.sequence
                || self.sequence != Some(result.sequence)
                || Instant::now() >= pending.deadline
                || self.shared.io(result.sequence, source, || Ok(())).is_err()
            {
                continue;
            }
            let Ok(upstream) = result.stream else {
                continue;
            };
            let mut connection = Connection {
                downstream: Endpoint {
                    stream: pending.downstream,
                    interest: None,
                },
                upstream: Endpoint {
                    stream: upstream,
                    interest: None,
                },
                request: Direction::new(),
                reply: Direction::new(),
                connecting: Some(pending.deadline),
                queued: false,
            };
            if connection
                .interests(self.poll.registry(), result.id)
                .is_err()
            {
                continue;
            }
            self.connections.insert(result.id, connection);
            self.schedule(result.id);
        }
    }

    fn run(mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(MAX_CONNECTIONS * 2 + 2);
        loop {
            if !self.synchronize() {
                return Ok(());
            }
            let now = Instant::now();
            self.connections.retain(|_, connection| {
                connection.connecting.is_none_or(|deadline| deadline > now)
            });
            self.pending.retain(|_, pending| pending.deadline > now);
            let timeout = if self.accepting || !self.ready.is_empty() {
                Some(Duration::ZERO)
            } else {
                // Only admission/pending connect has a deadline. Established,
                // approval-blocked and idle connections have no timer at all.
                self.connections
                    .values()
                    .filter_map(|connection| connection.connecting)
                    .chain(self.pending.values().map(|pending| pending.deadline))
                    .min()
                    .map(|deadline| deadline.saturating_duration_since(now))
            };
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            if !self.synchronize() {
                return Ok(());
            }
            self.complete_admissions();
            for event in &events {
                match event.token() {
                    LISTENER => self.accepting = true,
                    WAKE => {}
                    Token(token) => self.schedule(token & !1),
                }
            }
            if self.accepting {
                self.accept()?;
            }
            // Snapshot the length so a busy connection gets at most one turn
            // before poll and before every other ready connection's next turn.
            for _ in 0..self.ready.len().min(MAX_CONNECTIONS) {
                let id = self.ready.pop_front().unwrap();
                let Some(connection) = self.connections.get_mut(&id) else {
                    continue;
                };
                connection.queued = false;
                let (Some(source), Some(sequence)) = (&self.source, self.sequence) else {
                    continue;
                };
                let outcome = connection.pump(&self.shared, sequence, source);
                let keep = outcome.is_ok()
                    && !(connection.request.shut && connection.reply.shut)
                    && connection.interests(self.poll.registry(), id).is_ok();
                if !keep {
                    self.connections.remove(&id);
                } else if matches!(outcome, Ok(true)) {
                    self.schedule(id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
