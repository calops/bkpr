//! Session-local SSH-agent forwarding owned by managed launcher attachment leases.
//! No process-global environment writes, per-key hooks, socket polling, or native patches.
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStringExt,
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener as StdListener, UnixStream as StdStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, Interest, unix::AsyncFd},
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
    time::timeout,
};

const MAX_PACKET: usize = 256 * 1024;
const MAX_CONNECTIONS: usize = 128;
const MAX_SOURCE: usize = 4096;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const FAILURE: &[u8] = &[0, 0, 0, 1, 5];
const ROUTE_FILE: &str = ".bkpr-agent-route";
const CONTROL: &str = "agent-control.sock";
const SOCKET: &str = "agent.sock";
const CONTROL_FD: i32 = 3;
const AGENT_FD: i32 = 4;

type ClientId = u64;

pub(crate) struct Lease {
    _stream: StdStream,
    socket: PathBuf,
}

impl Lease {
    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn release(mut self) -> Result<()> {
        self._stream.set_read_timeout(Some(IO_TIMEOUT))?;
        self._stream.set_write_timeout(Some(IO_TIMEOUT))?;
        self._stream.write_all(&[0])?;
        let mut released = [0];
        self._stream.read_exact(&mut released)?;
        if released != [1] {
            bail!("invalid agent lease release acknowledgement");
        }
        Ok(())
    }
}

/// Caller holds this runtime's startup_lock through readiness, serializing both
/// helper bootstrap and the daemon's final-lease cleanup with the next attach.
pub(crate) fn acquire_locked(
    program: &Path,
    runtime: &Path,
    source: Option<OsString>,
) -> Result<Lease> {
    acquire_with(runtime, source, |control, agent, bootstrap| {
        let control = duplicate_for_exec(control.as_raw_fd())?;
        let agent = duplicate_for_exec(agent.as_raw_fd())?;
        let mut command = Command::new(program);
        command
            .arg("agent-relay-daemon")
            .arg("--runtime")
            .arg(runtime)
            .stdin(Stdio::from(OwnedFd::from(bootstrap)))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Only dup2/setsid run after fork. Sources are above all destination fds.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || libc::dup2(control.as_raw_fd(), CONTROL_FD) == -1
                    || libc::dup2(agent.as_raw_fd(), AGENT_FD) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(Some(
            command.spawn().context("start session SSH-agent helper")?,
        ))
    })
}

fn duplicate_for_exec(fd: i32) -> io::Result<OwnedFd> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn acquire_with(
    runtime: &Path,
    source: Option<OsString>,
    start: impl FnOnce(StdListener, StdListener, StdStream) -> Result<Option<Child>>,
) -> Result<Lease> {
    let control_path = runtime.join(CONTROL);
    let mut child = None;
    let mut stream = match StdStream::connect(&control_path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            remove_stale(runtime)?;
            let control = bind_private(&control_path)?;
            let agent = match bind_private(&runtime.join(SOCKET)) {
                Ok(agent) => agent,
                Err(error) => {
                    fs::remove_file(&control_path)?;
                    return Err(error.into());
                }
            };
            let (parent, bootstrap) = StdStream::pair()?;
            match start(control, agent, bootstrap) {
                Ok(started) => child = started,
                Err(error) => {
                    remove_stale(runtime)?;
                    return Err(error);
                }
            }
            parent
        }
        Err(error) => return Err(error).context("connect session SSH-agent helper"),
    };
    let result = register(&mut stream, source);
    if let Err(error) = result {
        drop(stream);
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
            remove_stale(runtime)?;
        }
        return Err(error).context("register SSH-agent attachment lease");
    }
    if let Some(mut child) = child {
        // wait(), not periodic try_wait(). If another launcher still has a lease,
        // the helper legitimately outlives its original parent and is reparented.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(Lease {
        _stream: stream,
        socket: runtime.join(SOCKET),
    })
}

fn register(stream: &mut StdStream, source: Option<OsString>) -> io::Result<()> {
    let source = match source.filter(|source| !source.is_empty()) {
        Some(source) => {
            let path = PathBuf::from(source);
            // Resolve relative socket names in the attaching launcher's cwd,
            // not the first helper/server's potentially unrelated directory.
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            };
            path.into_os_string().into_vec()
        }
        None => Vec::new(),
    };
    if source.len() > MAX_SOURCE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(b"SAG1")?;
    stream.write_all(&(source.len() as u32).to_be_bytes())?;
    stream.write_all(&source)?;
    let mut ready = [0];
    stream.read_exact(&mut ready)?;
    if ready != [1] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // No lease heartbeat or idle deadline: EOF is the lifetime signal.
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(())
}

fn bind_private(path: &Path) -> io::Result<StdListener> {
    let listener = StdListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn remove_stale(runtime: &Path) -> Result<()> {
    for name in [CONTROL, SOCKET, ROUTE_FILE] {
        let path = runtime.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let expected = if name == ROUTE_FILE {
            metadata.is_file()
        } else {
            metadata.file_type().is_socket()
        };
        if !expected || metadata.uid() != unsafe { libc::geteuid() } {
            bail!(
                "refusing unowned or unexpected agent helper path {}",
                path.display()
            );
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn daemon(runtime: &Path) -> Result<()> {
    // These are inherited only by the private launch path; never discover them
    // from process environment or accept an arbitrary caller-supplied fd number.
    for fd in [0, CONTROL_FD, AGENT_FD] {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            bail!("missing inherited SSH-agent helper descriptors");
        }
    }
    let control = unsafe { StdListener::from_raw_fd(CONTROL_FD) };
    let agent = unsafe { StdListener::from_raw_fd(AGENT_FD) };
    let bootstrap = unsafe { StdStream::from_raw_fd(0) };
    serve_helper(runtime, control, agent, bootstrap)
}

#[derive(Clone, Debug, PartialEq)]
struct Target {
    generation: u64,
    path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Route {
    device: u64,
    inode: u64,
    target: Option<Vec<u8>>,
}

struct Routing {
    clients: BTreeMap<ClientId, Option<PathBuf>>,
    target: watch::Sender<Target>,
    socket: PathBuf,
}

#[derive(Clone)]
struct RoutingHandle(Arc<Mutex<Routing>>);

impl RoutingHandle {
    fn attach(&self, id: ClientId, source: Vec<u8>) {
        let source = (!source.is_empty())
            .then(|| PathBuf::from(OsString::from_vec(source)))
            .filter(|path| path.is_absolute());
        let mut state = self.0.lock();
        state.clients.insert(id, source);
        state.publish();
    }
    fn remove(&self, id: ClientId) {
        let mut state = self.0.lock();
        let active = state.clients.last_key_value().map(|(client, _)| *client) == Some(id);
        state.clients.remove(&id);
        if active {
            state.publish();
        }
    }
    fn is_empty(&self) -> bool {
        self.0.lock().clients.is_empty()
    }
}

impl Routing {
    fn publish(&mut self) {
        let mut target = Target {
            generation: self.target.borrow().generation + 1,
            path: self
                .clients
                .last_key_value()
                .and_then(|(_, path)| path.clone()),
        };
        if let Err(error) = write_route(&self.socket, target.path.clone()) {
            eprintln!("bkpr: cannot publish SSH-agent route: {error}");
            target.path = None;
        }
        self.target.send_replace(target);
    }
}

#[derive(Clone)]
struct OwnedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl OwnedSocket {
    fn at(path: PathBuf) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    fn remove(&self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn serve_helper(
    runtime: &Path,
    control: StdListener,
    agent: StdListener,
    bootstrap: StdStream,
) -> Result<()> {
    let directory = fs::symlink_metadata(runtime)?;
    if !directory.is_dir()
        || directory.uid() != unsafe { libc::geteuid() }
        || directory.mode() & 0o077 != 0
    {
        bail!("SSH-agent helper requires a private current-user-owned runtime directory");
    }
    if control.local_addr()?.as_pathname() != Some(runtime.join(CONTROL).as_path())
        || agent.local_addr()?.as_pathname() != Some(runtime.join(SOCKET).as_path())
    {
        bail!("invalid inherited SSH-agent helper listeners");
    }
    let control_owner = OwnedSocket::at(runtime.join(CONTROL))?;
    let agent_owner = OwnedSocket::at(runtime.join(SOCKET))?;
    control.set_nonblocking(true)?;
    agent.set_nonblocking(true)?;
    bootstrap.set_nonblocking(true)?;
    write_route(&agent_owner.path, None)?;
    let (target, receiver) = watch::channel(Target {
        generation: 0,
        path: None,
    });
    let routing = RoutingHandle(Arc::new(Mutex::new(Routing {
        clients: BTreeMap::new(),
        target,
        socket: agent_owner.path.clone(),
    })));
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome: Result<File> = executor.block_on(async {
        let control = UnixListener::from_std(control)?;
        let agent = UnixListener::from_std(agent)?;
        let bootstrap = UnixStream::from_std(bootstrap)?;
        let mut leases = JoinSet::new();
        let mut connections = JoinSet::new();
        let mut next_id: ClientId = 1;
        leases.spawn(attachment(bootstrap, next_id, routing.clone()));
        let mut cleanup = None;
        let result = loop {
            if routing.is_empty() && leases.is_empty() && cleanup.is_none() {
                let runtime = runtime.to_path_buf();
                // Keep accepting while flock blocks: a launcher holding the lock
                // may be waiting for readiness. Never deadlock it.
                cleanup = Some(tokio::task::spawn_blocking(move || crate::launcher::startup_lock(&runtime)));
            }
            tokio::select! {
                Some(completed) = leases.join_next(), if !leases.is_empty() => {
                    match completed {
                        Ok(id) => routing.remove(id),
                        Err(error) => break Err(anyhow::anyhow!("agent attachment task failed: {error}")),
                    }
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => {},
                incoming = control.accept(), if leases.len() < MAX_CONNECTIONS => {
                    match incoming {
                        Ok((stream, _)) => {
                            next_id += 1;
                            leases.spawn(attachment(stream, next_id, routing.clone()));
                        },
                        Err(error) => break Err(error.into()),
                    }
                },
                incoming = agent.accept() => {
                    match incoming {
                        Ok((stream, _)) if connections.len() < MAX_CONNECTIONS => {
                            connections.spawn(serve(stream, receiver.clone(), agent_owner.path.clone()));
                        },
                        Ok(_) => {},
                        Err(error) => break Err(error.into()),
                    }
                },
                locked = async { cleanup.as_mut().unwrap().await }, if cleanup.is_some() => {
                    cleanup = None;
                    match locked {
                        Ok(Ok(lock)) if routing.is_empty() && leases.is_empty() => break Ok(lock),
                        Ok(Ok(lock)) => drop(lock),
                        Ok(Err(error)) => break Err(error),
                        Err(error) => break Err(error.into()),
                    }
                },
            }
        };
        leases.abort_all();
        connections.abort_all();
        while leases.join_next().await.is_some() {}
        while connections.join_next().await.is_some() {}
        // A blocking flock cannot be aborted. Release it before runtime shutdown
        // rather than leave an orphan waiter that might lock a future helper.
        if let Some(cleanup) = cleanup
            && let Ok(Ok(lock)) = cleanup.await
        {
            drop(lock);
        }
        result
    });
    let cleanup_lock = match outcome {
        Ok(lock) => lock,
        Err(error) => {
            let _lock = crate::launcher::startup_lock(runtime)?;
            cleanup_paths(&control_owner, &agent_owner);
            return Err(error);
        }
    };
    cleanup_paths(&control_owner, &agent_owner);
    drop(cleanup_lock);
    Ok(())
}

fn cleanup_paths(control: &OwnedSocket, agent: &OwnedSocket) {
    let marker = agent.path.parent().unwrap().join(ROUTE_FILE);
    if fs::read(&marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Route>(&bytes).ok())
        .is_some_and(|route| route.device == agent.device && route.inode == agent.inode)
    {
        let _ = fs::remove_file(marker);
    }
    control.remove();
    agent.remove();
}

async fn attachment(mut stream: UnixStream, id: ClientId, routing: RoutingHandle) -> ClientId {
    let registered = timeout(IO_TIMEOUT, async {
        let mut magic = [0; 4];
        stream.read_exact(&mut magic).await?;
        if &magic != b"SAG1" {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let length = stream.read_u32().await? as usize;
        if length > MAX_SOURCE {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut source = vec![0; length];
        stream.read_exact(&mut source).await?;
        routing.attach(id, source);
        stream.write_all(&[1]).await?;
        Ok::<(), io::Error>(())
    })
    .await;
    if matches!(registered, Ok(Ok(()))) {
        // EOF handles abrupt launcher death. Orderly release is acknowledged
        // only after routing changes, making detach/fallback deterministic.
        let mut release = [0xff];
        let orderly = matches!(stream.read(&mut release).await, Ok(1)) && release == [0];
        routing.remove(id);
        if orderly {
            let _ = timeout(IO_TIMEOUT, stream.write_all(&[1])).await;
        }
    } else {
        routing.remove(id);
    }
    id
}

fn write_route(socket: &Path, target: Option<PathBuf>) -> io::Result<()> {
    let meta = fs::symlink_metadata(socket)?;
    let route = Route {
        device: meta.dev(),
        inode: meta.ino(),
        target: target.map(|p| p.into_os_string().into_vec()),
    };
    let dir = socket.parent().unwrap();
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(file.as_file_mut(), &route)?;
    file.flush()?;
    file.persist(dir.join(ROUTE_FILE)).map_err(|e| e.error)?;
    Ok(())
}

/// Metadata is a cycle guard, never a flattened fallback. Always contact the
/// immediate live relay, preserving its own generation and lifecycle checks.
fn resolve_target(path: &Path, own_socket: &Path) -> io::Result<PathBuf> {
    let own = fs::metadata(own_socket)?;
    let immediate = fs::canonicalize(path)?;
    let mut path = path.to_path_buf();
    let mut seen = HashSet::from([(own.dev(), own.ino())]);
    for _ in 0..32 {
        path = fs::canonicalize(path)?;
        let socket = fs::metadata(&path)?;
        if !seen.insert((socket.dev(), socket.ino())) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let dir = fs::symlink_metadata(parent)?;
        let route_file = parent.join(ROUTE_FILE);
        if !dir.is_dir()
            || dir.uid() != unsafe { libc::geteuid() }
            || dir.mode() & 0o077 != 0
            || !route_file.try_exists()?
        {
            return Ok(immediate);
        }
        let file = fs::symlink_metadata(&route_file)?;
        if !file.is_file()
            || file.uid() != dir.uid()
            || file.mode() & 0o077 != 0
            || file.len() > 16 * 1024
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let route: Route = serde_json::from_slice(&fs::read(route_file)?)?;
        if route.device != socket.dev() || route.inode != socket.ino() {
            return Ok(immediate);
        }
        path = PathBuf::from(OsString::from_vec(
            route.target.ok_or(io::ErrorKind::NotConnected)?,
        ));
    }
    Err(io::ErrorKind::InvalidInput.into())
}

async fn read_packet<R: AsyncRead + Unpin>(stream: &mut R) -> io::Result<Vec<u8>> {
    // The first byte may legitimately wait on human approval, a PIN, or touch.
    // Once a response/frame starts, its remainder has a bounded framing deadline.
    let first = stream.read_u8().await?;
    timeout(IO_TIMEOUT, async {
        let mut header = [first, 0, 0, 0];
        stream.read_exact(&mut header[1..]).await?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > MAX_PACKET {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut packet = vec![0; length + 4];
        packet[..4].copy_from_slice(&header);
        stream.read_exact(&mut packet[4..]).await?;
        Ok(packet)
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

async fn exchange(
    upstream: &mut Option<(PathBuf, UnixStream)>,
    path: &Path,
    request: &[u8],
) -> io::Result<Vec<u8>> {
    if upstream.as_ref().map(|(connected, _)| connected.as_path()) != Some(path) {
        let stream = timeout(IO_TIMEOUT, UnixStream::connect(path)).await??;
        *upstream = Some((path.to_path_buf(), stream));
    }
    // Preserve connection-scoped extensions. Never replay a failed signing request.
    let stream = &mut upstream.as_mut().unwrap().1;
    timeout(IO_TIMEOUT, stream.write_all(request)).await??;
    read_packet(stream).await
}

/// Observe only close notifications on an independent reactor registration.
/// Ignoring data events here never clears the actual stream reader's readiness,
/// consumes pipelined bytes, or needs a periodic liveness check.
async fn downstream_closed(observer: &AsyncFd<OwnedFd>) {
    loop {
        let mut event = match observer.readable().await {
            Ok(event) => event,
            Err(_) => return,
        };
        if event.ready().is_read_closed() || event.ready().is_error() {
            return;
        }
        // This registration is an observer, not the data reader. It intentionally
        // ignores normal data notifications and waits for the OS read-closed edge.
        event.clear_ready();
    }
}

async fn serve(
    mut downstream: UnixStream,
    mut target: watch::Receiver<Target>,
    own_socket: PathBuf,
) {
    let observer = match duplicate_for_exec(downstream.as_raw_fd())
        .and_then(|fd| AsyncFd::with_interest(fd, Interest::READABLE))
    {
        Ok(observer) => observer,
        Err(_) => return,
    };
    let mut upstream = None;
    let mut generation = None;
    loop {
        let request = match read_packet(&mut downstream).await {
            Ok(request) => request,
            Err(_) => return,
        };
        let selected = target.borrow_and_update().clone();
        if generation != Some(selected.generation) {
            upstream = None;
            generation = Some(selected.generation);
        }
        let response = match selected
            .path
            .as_deref()
            .and_then(|path| resolve_target(path, &own_socket).ok())
        {
            Some(path) => tokio::select! {
                biased;
                _ = target.changed() => None,
                _ = downstream_closed(&observer) => return,
                response = exchange(&mut upstream, &path, &request) => response.ok(),
            },
            None => None,
        };
        let current = target.borrow().generation == selected.generation;
        if !current || response.is_none() {
            upstream = None;
        }
        if !current {
            // No downstream response bytes have been written yet; a failure
            // frame is safe. Further switches during that write close the stream.
            target.borrow_and_update();
        }
        let reply = if current {
            response.as_deref().unwrap_or(FAILURE)
        } else {
            FAILURE
        };
        tokio::select! {
            biased;
            _ = target.changed() => return,
            result = timeout(IO_TIMEOUT, downstream.write_all(reply)) => {
                if !matches!(result, Ok(Ok(()))) { return; }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::Shutdown,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    const REQUEST: &[u8] = &[0, 0, 0, 1, 11];
    fn identity(id: u8) -> Vec<u8> {
        vec![0, 0, 0, 2, 12, id]
    }

    struct Agent {
        _directory: tempfile::TempDir,
        socket: PathBuf,
        stopped: Arc<AtomicBool>,
        streams: Arc<Mutex<Vec<StdStream>>>,
        worker: Option<thread::JoinHandle<()>>,
        connections: Arc<AtomicUsize>,
    }

    impl Agent {
        fn new(id: u8) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("agent");
            let listener = StdListener::bind(&socket).unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let streams = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(AtomicUsize::new(0));
            let flag = stopped.clone();
            let active = streams.clone();
            let accepted = connections.clone();
            let worker = thread::spawn(move || {
                let mut workers = Vec::new();
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                    accepted.fetch_add(1, Ordering::Relaxed);
                    active.lock().push(stream.try_clone().unwrap());
                    workers.push(thread::spawn(move || {
                        loop {
                            let mut request = [0; 5];
                            match stream.read_exact(&mut request) {
                                Ok(()) => {}
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::UnexpectedEof
                                            | io::ErrorKind::ConnectionReset
                                    ) =>
                                {
                                    break;
                                }
                                Err(error) => panic!("fixture agent read: {error}"),
                            }
                            assert_eq!(&request, REQUEST);
                            if stream.write_all(&identity(id)).is_err() {
                                break;
                            }
                        }
                    }));
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            });
            Self {
                _directory: directory,
                socket,
                stopped,
                streams,
                worker: Some(worker),
                connections,
            }
        }
        fn source(&self) -> Option<OsString> {
            Some(self.socket.as_os_str().to_owned())
        }
    }

    impl Drop for Agent {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            for stream in self.streams.lock().iter() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            let _ = StdStream::connect(&self.socket);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    struct Worker {
        thread: thread::JoinHandle<()>,
        done: mpsc::Receiver<std::result::Result<(), String>>,
    }

    struct Helper {
        directory: tempfile::TempDir,
        workers: Mutex<Vec<Worker>>,
    }

    impl Helper {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            crate::launcher::private_directory(directory.path()).unwrap();
            Self {
                directory,
                workers: Mutex::new(Vec::new()),
            }
        }
        fn runtime(&self) -> &Path {
            self.directory.path()
        }
        fn lease(&self, source: Option<OsString>) -> Lease {
            let lock = crate::launcher::startup_lock(self.runtime()).unwrap();
            let result = acquire_with(self.runtime(), source, |control, agent, bootstrap| {
                let runtime = self.runtime().to_owned();
                let (done, completed) = mpsc::channel();
                let thread = thread::spawn(move || {
                    let result = serve_helper(&runtime, control, agent, bootstrap);
                    done.send(result.map_err(|error| format!("{error:#}")))
                        .unwrap();
                });
                self.workers.lock().push(Worker {
                    thread,
                    done: completed,
                });
                Ok(None)
            });
            drop(lock);
            result.unwrap_or_else(|error| {
                let failures: Vec<_> = self
                    .workers
                    .lock()
                    .iter()
                    .map(|worker| worker.done.recv_timeout(Duration::from_secs(1)))
                    .collect();
                panic!("lease registration failed: {error:#}; helper exits: {failures:?}");
            })
        }
        fn finish(&self) {
            for worker in self.workers.lock().drain(..) {
                worker
                    .done
                    .recv_timeout(Duration::from_secs(5))
                    .expect("helper did not shut down")
                    .unwrap();
                worker.thread.join().unwrap();
            }
            assert!(!self.runtime().join(SOCKET).exists());
            assert!(!self.runtime().join(CONTROL).exists());
            assert!(!self.runtime().join(ROUTE_FILE).exists());
            assert!(self.runtime().join("startup.lock").exists());
        }
    }

    fn connect(path: &Path) -> StdStream {
        let stream = StdStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn request(stream: &mut StdStream) -> Vec<u8> {
        stream.write_all(REQUEST).unwrap();
        let mut header = [0; 4];
        stream.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header) as usize;
        assert!(length <= MAX_PACKET);
        let mut reply = vec![0; length + 4];
        reply[..4].copy_from_slice(&header);
        stream.read_exact(&mut reply[4..]).unwrap();
        reply
    }

    #[test]
    fn agent_relay_managed_protocol_switch_unset_and_detach_fallback() {
        let one = Agent::new(1);
        let two = Agent::new(2);
        let helper = Helper::new();
        let first = helper.lease(one.source());
        let socket = first.socket().to_owned();
        let mut persistent = connect(&socket);
        assert_eq!(request(&mut persistent), identity(1));
        assert_eq!(request(&mut persistent), identity(1));
        assert_eq!(one.connections.load(Ordering::Relaxed), 1);
        let second = helper.lease(two.source());
        assert_eq!(second.socket(), socket);
        assert_eq!(request(&mut persistent), identity(2));
        let unset = helper.lease(None);
        assert_eq!(request(&mut persistent), FAILURE);
        unset.release().unwrap();
        assert_eq!(request(&mut persistent), identity(2));
        let empty = helper.lease(Some(OsString::new()));
        assert_eq!(request(&mut persistent), FAILURE);
        empty.release().unwrap();
        second.release().unwrap();
        assert_eq!(request(&mut persistent), identity(1));
        first.release().unwrap();
        helper.finish();
        assert!(!socket.exists());
    }

    #[test]
    fn agent_relay_stable_old_process_path_survives_helper_restart_and_stale_sockets() {
        let one = Agent::new(1);
        let two = Agent::new(2);
        let helper = Helper::new();
        let first = helper.lease(one.source());
        let old_process_address = first.socket().to_owned();
        assert_eq!(request(&mut connect(&old_process_address)), identity(1));
        // Dropping without the release handshake models killed-launcher EOF.
        drop(first);
        helper.finish();
        let stale_control = StdListener::bind(helper.runtime().join(CONTROL)).unwrap();
        let stale_agent = StdListener::bind(&old_process_address).unwrap();
        #[cfg(target_os = "linux")]
        let inherited = {
            // Other tests can fork while these listeners are open: CLOEXEC only
            // closes inherited copies on exec, not when we drop our copies.
            // Linux shutdown affects every copy, making the fixture stale even
            // while these duplicates model descriptors inherited by a child.
            let inherited = [
                stale_control.try_clone().unwrap(),
                stale_agent.try_clone().unwrap(),
            ];
            for listener in [&stale_control, &stale_agent] {
                assert_eq!(
                    unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RDWR) },
                    0,
                    "shut down stale fixture listener: {}",
                    io::Error::last_os_error()
                );
            }
            inherited
        };
        drop((stale_control, stale_agent));
        let second = helper.lease(two.source());
        assert_eq!(second.socket(), old_process_address);
        assert_eq!(request(&mut connect(&old_process_address)), identity(2));
        #[cfg(target_os = "linux")]
        drop(inherited);
        second.release().unwrap();
        helper.finish();
    }

    #[test]
    fn agent_relay_final_detach_racing_bootstrap_never_unlinks_replacement() {
        let agent = Agent::new(1);
        let helper = Arc::new(Helper::new());
        for _ in 0..4 {
            let lease = helper.lease(agent.source());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_barrier = barrier.clone();
            let detaching = thread::spawn(move || {
                worker_barrier.wait();
                lease.release().unwrap();
            });
            barrier.wait();
            let next = helper.lease(agent.source());
            detaching.join().unwrap();
            assert_eq!(request(&mut connect(next.socket())), identity(1));
            next.release().unwrap();
            helper.finish();
        }
    }

    #[test]
    fn agent_relay_self_symlink_nested_cycles_and_source_bytes() {
        let agent = Agent::new(1);
        let a = Helper::new();
        let b = Helper::new();
        let b_origin = b.lease(agent.source());
        let a_origin = a.lease(Some(b_origin.socket().as_os_str().to_owned()));
        let mut downstream = connect(a_origin.socket());
        assert_eq!(request(&mut downstream), identity(1));
        let b_cycle = b.lease(Some(a_origin.socket().as_os_str().to_owned()));
        assert_eq!(request(&mut downstream), FAILURE);
        b_cycle.release().unwrap();
        assert_eq!(request(&mut downstream), identity(1));
        let alias = a.runtime().join(OsString::from_vec(vec![b'a', 0xff]));
        std::os::unix::fs::symlink(a_origin.socket(), &alias).unwrap();
        let self_cycle = a.lease(Some(alias.into_os_string()));
        assert_eq!(request(&mut downstream), FAILURE);
        self_cycle.release().unwrap();
        assert_eq!(request(&mut downstream), identity(1));
        a_origin.release().unwrap();
        a.finish();
        b_origin.release().unwrap();
        b.finish();
    }

    #[test]
    fn agent_relay_missing_upstream_failure_recovers_and_paths_are_private() {
        let helper = Helper::new();
        let socket = helper.runtime().join("late-agent");
        let lease = helper.lease(Some(socket.as_os_str().to_owned()));
        assert_eq!(fs::metadata(lease.socket()).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(helper.runtime()).unwrap().mode() & 0o777,
            0o700
        );
        let mut downstream = connect(lease.socket());
        assert_eq!(request(&mut downstream), FAILURE);
        let listener = StdListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0; 5];
            connection.read_exact(&mut request).unwrap();
            connection.write_all(&identity(7)).unwrap();
        });
        assert_eq!(request(&mut downstream), identity(7));
        worker.join().unwrap();
        assert_eq!(request(&mut downstream), FAILURE);
        let mut malformed = connect(lease.socket());
        malformed
            .write_all(&((MAX_PACKET + 1) as u32).to_be_bytes())
            .unwrap();
        assert_eq!(malformed.read(&mut [0]).unwrap(), 0);
        lease.release().unwrap();
        helper.finish();
    }

    #[test]
    fn agent_relay_refuses_symlink_control_without_touching_target() {
        let helper = Helper::new();
        let target = helper.runtime().join("untouched");
        fs::write(&target, b"sentinel").unwrap();
        std::os::unix::fs::symlink(&target, helper.runtime().join(CONTROL)).unwrap();
        let _lock = crate::launcher::startup_lock(helper.runtime()).unwrap();
        let result = acquire_with(helper.runtime(), None, |_, _, _| {
            panic!("must not start helper")
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
    }

    async fn core_fixture() -> (
        tempfile::TempDir,
        UnixListener,
        UnixStream,
        watch::Sender<Target>,
        tokio::task::JoinHandle<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let own_path = directory.path().join(SOCKET);
        let _own_listener = bind_private(&own_path).unwrap();
        let upstream_path = directory.path().join("fixture-upstream");
        let upstream = UnixListener::bind(&upstream_path).unwrap();
        let (target, receiver) = watch::channel(Target {
            generation: 1,
            path: Some(upstream_path),
        });
        let (peer, downstream) = UnixStream::pair().unwrap();
        let size: libc::c_int = 4096;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    downstream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of_val(&size) as libc::socklen_t,
                )
            },
            0
        );
        let serving = tokio::spawn(serve(downstream, receiver, own_path));
        (directory, upstream, peer, target, serving)
    }

    #[tokio::test(start_paused = true)]
    async fn agent_relay_human_approval_wait_is_not_a_ten_second_timeout() {
        let (_directory, upstream, mut peer, _target, serving) = core_fixture().await;
        peer.write_all(REQUEST).await.unwrap();
        let (mut agent, _) = upstream.accept().await.unwrap();
        let mut request = [0; 5];
        agent.read_exact(&mut request).await.unwrap();
        tokio::time::advance(IO_TIMEOUT * 3).await;
        agent.write_all(&identity(1)).await.unwrap();
        assert_eq!(read_packet(&mut peer).await.unwrap(), identity(1));
        drop(peer);
        serving.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn agent_relay_started_frame_still_has_a_bounded_deadline() {
        let (mut peer, mut stream) = UnixStream::pair().unwrap();
        let reading = tokio::spawn(async move { read_packet(&mut stream).await });
        peer.write_all(&[0]).await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(IO_TIMEOUT * 2).await;
        assert_eq!(
            reading.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test]
    async fn agent_relay_pending_approval_cancels_on_switch_or_downstream_eof() {
        for switch in [true, false] {
            let (_directory, upstream, mut peer, target, serving) = core_fixture().await;
            peer.write_all(REQUEST).await.unwrap();
            let (mut agent, _) = upstream.accept().await.unwrap();
            let mut request = [0; 5];
            agent.read_exact(&mut request).await.unwrap();
            if switch {
                target.send_replace(Target {
                    generation: 2,
                    path: None,
                });
                assert_eq!(read_packet(&mut peer).await.unwrap(), FAILURE);
            }
            drop(peer);
            timeout(IO_TIMEOUT, serving).await.unwrap().unwrap();
            assert_eq!(
                timeout(IO_TIMEOUT, agent.read(&mut [0]))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
    }

    #[tokio::test]
    async fn agent_relay_backpressured_reply_cannot_finish_after_identity_switch() {
        let (_directory, upstream, mut peer, target, serving) = core_fixture().await;
        peer.write_all(REQUEST).await.unwrap();
        let (mut agent, _) = upstream.accept().await.unwrap();
        let mut request = [0; 5];
        agent.read_exact(&mut request).await.unwrap();
        let mut large = vec![42; MAX_PACKET + 4];
        large[..4].copy_from_slice(&(MAX_PACKET as u32).to_be_bytes());
        let sending = tokio::spawn(async move {
            agent.write_all(&large).await.unwrap();
        });
        // Readability proves downstream delivery started; don't drain its small
        // buffer until the generation change has cancelled the pending write.
        peer.readable().await.unwrap();
        target.send_replace(Target {
            generation: 2,
            path: None,
        });
        timeout(IO_TIMEOUT, serving).await.unwrap().unwrap();
        let mut partial = Vec::new();
        peer.read_to_end(&mut partial).await.unwrap();
        assert!(!partial.is_empty());
        assert!(
            partial.len() < MAX_PACKET + 4,
            "old-generation reply completed after switch"
        );
        sending.await.unwrap();
    }
    #[test]
    fn agent_relay_simultaneous_initial_clients_share_one_reachable_helper() {
        let agent = Agent::new(1);
        let helper = Arc::new(Helper::new());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut attaching = Vec::new();
        for _ in 0..2 {
            let helper = helper.clone();
            let barrier = barrier.clone();
            let source = agent.source();
            attaching.push(std::thread::spawn(move || {
                barrier.wait();
                helper.lease(source)
            }));
        }
        barrier.wait();
        let mut leases: Vec<_> = attaching
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(helper.workers.lock().len(), 1);
        assert_eq!(leases[0].socket(), leases[1].socket());
        assert_eq!(request(&mut connect(leases[0].socket())), identity(1));
        for lease in leases.drain(..) {
            lease.release().unwrap();
        }
        helper.finish();
    }

    #[test]
    fn agent_relay_malformed_control_cannot_acquire_an_origin() {
        let agent = Agent::new(1);
        let helper = Helper::new();
        let lease = helper.lease(agent.source());
        for invalid in [
            b"BAD!".to_vec(),
            [b"SAG1".as_slice(), &((MAX_SOURCE + 1) as u32).to_be_bytes()].concat(),
        ] {
            let mut control = connect(&helper.runtime().join(CONTROL));
            control.write_all(&invalid).unwrap();
            assert_eq!(control.read(&mut [0]).unwrap(), 0);
            assert_eq!(request(&mut connect(lease.socket())), identity(1));
        }
        lease.release().unwrap();
        helper.finish();
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[tokio::test]
    async fn agent_relay_queued_requests_need_no_further_readiness_edge() {
        let directory = tempfile::tempdir().unwrap();
        let own_path = directory.path().join(SOCKET);
        let _owned = bind_private(&own_path).unwrap();
        let upstream_path = directory.path().join("upstream");
        let upstream = UnixListener::bind(&upstream_path).unwrap();
        let (_target, receiver) = watch::channel(Target {
            generation: 1,
            path: Some(upstream_path),
        });
        let (mut peer, downstream) = UnixStream::pair().unwrap();
        let serving = tokio::spawn(serve(downstream, receiver, own_path));
        // Queue four complete requests before the first agent reply, and never
        // write again. Clearing consumer readiness during approval would strand
        // the subsequent requests indefinitely; no artificial queue cap applies.
        peer.write_all(&[
            0, 0, 0, 1, 11, 0, 0, 0, 1, 11, 0, 0, 0, 1, 11, 0, 0, 0, 1, 11,
        ])
        .await
        .unwrap();
        let (mut agent, _) = upstream.accept().await.unwrap();
        for id in [1, 2, 3, 4] {
            let mut request = [0; 5];
            timeout(IO_TIMEOUT, agent.read_exact(&mut request))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request, [0, 0, 0, 1, 11]);
            agent.write_all(&[0, 0, 0, 2, 12, id]).await.unwrap();
            assert_eq!(
                timeout(IO_TIMEOUT, read_packet(&mut peer))
                    .await
                    .unwrap()
                    .unwrap(),
                [0, 0, 0, 2, 12, id]
            );
        }
        drop(peer);
        timeout(IO_TIMEOUT, serving).await.unwrap().unwrap();
    }

    #[test]
    fn agent_relay_control_preserves_non_utf8_source_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(OsString::from_vec(vec![b'a', 0xff]));
        let listener = StdListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 5];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(&[0, 0, 0, 1, 6]).unwrap();
        });
        let (mut sender, receiver) = StdStream::pair().unwrap();
        let registering = std::thread::spawn(move || {
            register(&mut sender, Some(path.into_os_string())).unwrap();
            sender
        });
        receiver.set_nonblocking(true).unwrap();
        let route_socket = directory.path().join(SOCKET);
        let _listener = bind_private(&route_socket).unwrap();
        let (target, _receiver) = watch::channel(Target {
            generation: 0,
            path: None,
        });
        let routing = RoutingHandle(Arc::new(Mutex::new(Routing {
            clients: BTreeMap::new(),
            target,
            socket: route_socket.clone(),
        })));
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async {
            let control = UnixStream::from_std(receiver).unwrap();
            let registration = tokio::spawn(attachment(control, 1, routing.clone()));
            let lease = tokio::task::spawn_blocking(move || registering.join().unwrap())
                .await
                .unwrap();
            let source = routing.0.lock().clients.get(&1).unwrap().clone().unwrap();
            let mut upstream = None;
            assert_eq!(
                exchange(&mut upstream, &source, &[0, 0, 0, 1, 11])
                    .await
                    .unwrap(),
                [0, 0, 0, 1, 6]
            );
            lease.shutdown(std::net::Shutdown::Write).unwrap();
            registration.await.unwrap();
        });
        worker.join().unwrap();
    }
}
