//! One owned OpenSSH transport to one existing native session. No reconnect or replay.
use crate::transport::client_lifetime::{ConnectionPurpose, Connector, Lifecycle};
use crate::transport::{Endpoint, RegisteredClient};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{
    PROTOCOL_VERSION, SessionInfo, SpawnDefaults,
    remote_wire::{self, Control, Hello, Welcome},
    wire,
};
use futures::{
    future::poll_fn,
    io::{AsyncRead, AsyncWrite},
};
use std::{
    collections::VecDeque,
    io::Read,
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex, mpsc as sync},
    task::{Context as TaskContext, Poll},
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadBuf},
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
    time::timeout,
};
use tokio_util::{
    compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt},
    sync::CancellationToken,
};

const STREAMS: usize = 96;
const RECEIVE_WINDOW: usize = 24 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
const STDERR_LIMIT: usize = 16 * 1024;
const HEARTBEAT_MS: u32 = 1000;
const LEASE_MS: u32 = 5000;

#[derive(Clone, Debug)]
pub struct RemoteTarget {
    pub destination: String,
    pub session_name: String,
    pub helper: Option<PathBuf>,
    pub ssh_config: Option<PathBuf>,
    pub forward_agent: bool,
    pub detach_others: bool,
}

/// Only the remote helper resolves paths. These paths never become caller identity.
pub struct ResolvedSession {
    pub socket: PathBuf,
    pub info: SessionInfo,
    pub agent_socket: Option<PathBuf>,
}

struct OpenPair {
    stream: UnixStream,
    permit: OwnedSemaphorePermit,
    reply: sync::SyncSender<Result<()>>,
}

struct RemoteConnector {
    sender: mpsc::Sender<OpenPair>,
    lifetime: Arc<Semaphore>,
    control: Arc<Semaphore>,
    observations: Arc<Semaphore>,
}

impl RemoteConnector {
    async fn reserve(&self, purpose: ConnectionPurpose) -> Result<OwnedSemaphorePermit> {
        let pool = match purpose {
            ConnectionPurpose::Lifetime => &self.lifetime,
            ConnectionPurpose::Control => &self.control,
            ConnectionPurpose::Observation => &self.observations,
        };
        pool.clone()
            .acquire_owned()
            .await
            .context("remote native transport closed")
    }
}

impl Connector for RemoteConnector {
    fn remote(&self) -> bool {
        true
    }
    fn connect(&self, purpose: ConnectionPurpose) -> Result<UnixStream> {
        // Called on request workers: queue fairly without imposing a view limit.
        let permit = futures::executor::block_on(self.reserve(purpose))?;
        let (client, relay) = UnixStream::pair().context("creating remote native socket pair")?;
        socket_bounds(&client)?;
        socket_bounds(&relay)?;
        let cancellation = relay.try_clone()?;
        let (tx, rx) = sync::sync_channel(1);
        self.sender
            .try_send(OpenPair {
                stream: relay,
                permit,
                reply: tx,
            })
            .map_err(|_| anyhow::anyhow!("remote connection closed or admission queue full"))?;
        match rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(client),
            Ok(Err(error)) => Err(error),
            Err(error) => {
                let _ = cancellation.shutdown(Shutdown::Both);
                Err(error).context("opening remote native stream; request not dispatched")
            }
        }
    }
}

struct Supervisor {
    stop: watch::Sender<bool>,
    thread: Option<thread::JoinHandle<()>>,
    cancellation: CancellationToken,
    failure: Arc<Mutex<Option<String>>>,
}
impl Supervisor {
    fn cancel(&self) {
        self.cancellation.cancel();
    }
    fn retire(&self) {
        self.stop.send_replace(true);
    }
    fn cancel_and_join(&mut self) {
        self.cancel();
        self.join();
    }
    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
    fn stop(&mut self) {
        self.retire();
        self.join();
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        // Explicit native retirement drains accepted mutation receipts on the
        // transport thread. Stale admission/force cancellation closes promptly.
        // Neither path joins child/bridge teardown from the terminal thread.
        if !*self.stop.borrow() {
            self.cancel();
        }
    }
}

/// Owns the SSH child and sole native lifetime. Endpoint clones cannot keep an
/// attachment alive and cannot reconnect after this object is disconnected.
pub struct RemoteSession {
    client: RegisteredClient,
    supervisor: Supervisor,
}
impl RemoteSession {
    pub fn connect(target: RemoteTarget) -> Result<Self> {
        let detach_others = target.detach_others;
        let mut session = Self::register(target)?;
        session.activate_once(detach_others)?;
        Ok(session)
    }

    /// Headless admission. Interactive attachments use cancellable admission
    /// and activate only after coordinator identity/alias approval.
    fn register(target: RemoteTarget) -> Result<Self> {
        let (welcome, connector, mut supervisor) =
            Self::establish(target, CancellationToken::new())?;
        match RegisteredClient::register_connector(connector, welcome.session, welcome.agent_socket)
        {
            Ok(client) => Ok(Self { client, supervisor }),
            Err(error) => {
                supervisor.cancel_and_join();
                Err(error)
            }
        }
    }

    /// Establish and register on an admission worker. The token remains bound
    /// to this owned SSH transport through activation and the attachment's life.
    /// Cancellation closes all relays, including a blocked registration read.
    pub(crate) fn register_cancellable(
        target: RemoteTarget,
        cancellation: CancellationToken,
    ) -> Result<(Self, SpawnDefaults)> {
        let (welcome, connector, mut supervisor) = Self::establish(target, cancellation.clone())?;
        let admission: Result<_> = (|| {
            ensure!(!cancellation.is_cancelled(), "remote admission cancelled");
            let client = RegisteredClient::register_connector(
                connector,
                welcome.session,
                welcome.agent_socket,
            )?;
            let defaults = spawn_defaults(client.endpoint(), client.session())?;
            ensure!(!cancellation.is_cancelled(), "remote admission cancelled");
            Ok((client, defaults))
        })();
        match admission {
            Ok((client, defaults)) => Ok((Self { client, supervisor }, defaults)),
            Err(error) => {
                // This API runs only on a connection worker. Its completion
                // must not outlive an owned SSH child on failed admission.
                supervisor.cancel_and_join();
                Err(error)
            }
        }
    }

    pub(crate) fn activate_once(&mut self, detach_others: bool) -> Result<()> {
        let result = self.client.activate_once(detach_others);
        if result.is_err() {
            self.client.cancel();
            self.supervisor.cancel_and_join();
        }
        result
    }
    pub(crate) fn activate_cancellable(
        &mut self,
        detach_others: bool,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let result = self
            .client
            .activate_cancellable(detach_others, cancellation);
        if result.is_err() {
            self.client.cancel();
            self.supervisor.cancel_and_join();
        }
        result
    }

    /// Discover one existing remote session without registering or activating a
    /// native client, changing takeover state, or publishing an agent source.
    pub fn inspect(target: RemoteTarget) -> Result<SessionInfo> {
        ensure!(
            !target.detach_others,
            "remote inspection cannot detach other clients"
        );
        ensure!(
            !target.forward_agent,
            "remote inspection cannot forward an SSH agent"
        );
        let (welcome, _connector, mut supervisor) =
            Self::establish(target, CancellationToken::new())?;
        supervisor.stop();
        Ok(welcome.session)
    }

    fn establish(
        target: RemoteTarget,
        cancellation: CancellationToken,
    ) -> Result<(Welcome, Arc<RemoteConnector>, Supervisor)> {
        ensure!(!cancellation.is_cancelled(), "remote admission cancelled");
        validate_target(&target)?;
        let command = ssh_command(&target)?;
        Self::establish_command(target, cancellation, command)
    }

    fn establish_command(
        target: RemoteTarget,
        cancellation: CancellationToken,
        command: Command,
    ) -> Result<(Welcome, Arc<RemoteConnector>, Supervisor)> {
        // Parent cancellation retires this attempt, but transport cleanup must
        // never cancel its caller's generation token. Otherwise a real error
        // completion is mistaken for a stale attempt and silently discarded.
        let cancellation = cancellation.child_token();
        let mut nonce = [0; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let hello = Hello {
            session_name: target.session_name,
            native_family: PROTOCOL_VERSION,
            required_capabilities: wire::SUPPORTED_CAPABILITIES,
            connection_nonce: nonce,
            forward_agent: target.forward_agent,
        };
        let (sender, receiver) = mpsc::channel(STREAMS - 1);
        let connector = Arc::new(RemoteConnector {
            sender,
            lifetime: Arc::new(Semaphore::new(1)),
            control: Arc::new(Semaphore::new(15)),
            observations: Arc::new(Semaphore::new(79)),
        });
        let (stop, stop_rx) = watch::channel(false);
        let (ready, ready_rx) = sync::sync_channel(1);
        let failure = Arc::new(Mutex::new(None));
        let failure_task = failure.clone();
        let transport_cancellation = cancellation.clone();
        let thread = thread::Builder::new()
            .name("bkpr-remote-ssh".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("creating remote transport runtime")
                    .and_then(|runtime| {
                        runtime.block_on(run_ssh(
                            command,
                            hello,
                            receiver,
                            stop_rx,
                            ready,
                            transport_cancellation,
                        ))
                    });
                if let Err(error) = result {
                    *failure_task.lock().unwrap() = Some(format!("{error:#}"));
                }
            })?;
        let mut supervisor = Supervisor {
            stop,
            thread: Some(thread),
            cancellation,
            failure,
        };
        let handshake = ready_rx
            .recv_timeout(HANDSHAKE_TIMEOUT + Duration::from_secs(2))
            .context("waiting for remote session handshake")
            .and_then(|reply| reply);
        let welcome = match handshake {
            Ok(welcome) => welcome,
            Err(error) => {
                supervisor.cancel_and_join();
                let failure = supervisor.failure.lock().unwrap();
                if let Some(cause) = failure.as_ref() {
                    return Err(error.context(cause.clone()));
                }
                return Err(error);
            }
        };
        Ok((welcome, connector, supervisor))
    }
    pub fn info(&self) -> &SessionInfo {
        self.client.session()
    }
    /// Native registration identity only; never exposes the capability token.
    pub fn client_id(&self) -> &str {
        self.client.client_id()
    }
    pub fn endpoint(&self) -> Endpoint {
        self.client.endpoint().clone()
    }
    /// Lifecycle consumption never waits for SSH teardown. Explicit native
    /// detach events take precedence over a coincident transport failure.
    pub fn poll_lifecycle(&mut self) -> Result<Lifecycle> {
        let result = self.client.poll_lifecycle();
        if !matches!(result, Ok(Lifecycle::Active)) {
            if matches!(result, Ok(Lifecycle::Detached { .. })) {
                self.supervisor.retire();
            } else {
                self.supervisor.cancel();
            }
            if matches!(result, Ok(Lifecycle::TransportClosed))
                && let Some(error) = self.supervisor.failure.lock().unwrap().as_ref()
            {
                bail!("remote transport disconnected: {error}");
            }
        }
        result
    }
    /// Close this attachment's authority/SSH without joining or detaching any
    /// other native client. No daemon or application process is terminated.
    pub fn cancel(&mut self) {
        self.client.cancel();
        self.supervisor.cancel();
    }
    /// Intentional nonblocking detach: stop new authority immediately, but keep
    /// accepted mutation streams alive until their original receipts drain.
    pub fn retire(&mut self) {
        self.client.cancel();
        self.supervisor.retire();
    }
    pub fn disconnect(&mut self) -> Result<()> {
        let result = self.client.detach();
        self.supervisor.stop();
        result
    }
    pub fn lifetime_fd(&self) -> std::os::fd::RawFd {
        self.client.lifetime_fd()
    }

    pub(crate) fn close_for_shutdown(&mut self) {
        self.client.close_for_shutdown();
        self.supervisor.retire();
    }
}

#[derive(serde::Deserialize)]
struct DefaultsReply {
    session: SessionInfo,
    #[serde(default)]
    spawn_defaults: Option<SpawnDefaults>,
}

pub(crate) fn spawn_defaults(endpoint: &Endpoint, expected: &SessionInfo) -> Result<SpawnDefaults> {
    use std::os::unix::ffi::OsStrExt;
    let reply: DefaultsReply =
        serde_json::from_value(endpoint.checked(bkpr_protocol::Operation::SessionInfo)?)
            .context("reading authoritative remote spawn defaults before activation")?;
    ensure!(
        reply.session == *expected,
        "remote session or daemon changed while reading spawn defaults"
    );
    let defaults = reply.spawn_defaults.context(
        "remote daemon does not provide authoritative spawn defaults; a compatible remote bkprd is required",
    )?;
    ensure!(
        !defaults.argv.is_empty()
            && !defaults.argv[0].is_empty()
            && defaults.argv.iter().all(|arg| !arg.contains('\0'))
            && defaults.cwd.is_absolute()
            && defaults.home.is_absolute()
            && !defaults.cwd.as_os_str().as_bytes().contains(&0)
            && !defaults.home.as_os_str().as_bytes().contains(&0),
        "remote daemon returned invalid spawn defaults"
    );
    Ok(defaults)
}

fn validate_target(target: &RemoteTarget) -> Result<()> {
    ensure!(
        !target.destination.is_empty()
            && !target.destination.starts_with('-')
            && !target
                .destination
                .chars()
                .any(|c| c.is_control() || c.is_whitespace()),
        "invalid SSH destination"
    );
    bkpr_protocol::valid_session_name(&target.session_name)?;
    let _ = helper_command(target)?;
    Ok(())
}
fn helper_command(target: &RemoteTarget) -> Result<String> {
    let executable = target
        .helper
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("bkpr"));
    let executable = executable
        .to_str()
        .context("remote helper executable must be UTF-8")?;
    ensure!(
        !executable.is_empty() && !executable.chars().any(char::is_control),
        "invalid remote helper executable"
    );
    Ok(format!(
        "'{}' __remote-helper --stdio",
        executable.replace('\'', "'\\''")
    ))
}
fn ssh_command(target: &RemoteTarget) -> Result<Command> {
    let mut command = Command::new("ssh");
    // Launch authority and pane snapshots belong to the local process tree.
    // A user's SendEnv configuration must not carry them to another machine.
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"BKPR_") {
            command.env_remove(name);
        }
    }
    if let Some(config) = &target.ssh_config {
        command.arg("-F").arg(config);
    }
    command.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "RequestTTY=no",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ControlPersist=no",
        "-o",
        "UpdateHostKeys=no",
    ]);
    if target.forward_agent {
        command.arg("-A");
    } else {
        command.args(["-o", "ForwardAgent=no"]);
    }
    command
        .arg("--")
        .arg(&target.destination)
        .arg(helper_command(target)?);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

fn mux_config() -> yamux::Config {
    let mut config = yamux::Config::default();
    // Set streams first: yamux checks receive_window >= streams * 256 KiB.
    config
        .set_max_num_streams(STREAMS)
        .set_max_connection_receive_window(Some(RECEIVE_WINDOW))
        .set_split_send_size(CHUNK)
        .set_read_after_close(false);
    config
}

struct Duplex<R, W> {
    read: R,
    write: W,
}
impl<R: tokio::io::AsyncRead + Unpin, W: Unpin> tokio::io::AsyncRead for Duplex<R, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
    }
}
impl<R: Unpin, W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Duplex<R, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().write).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().write).poll_shutdown(cx)
    }
}

type MuxOpen = oneshot::Sender<Result<yamux::Stream>>;
async fn drive_mux<T: AsyncRead + AsyncWrite + Unpin>(
    mut connection: yamux::Connection<T>,
    mut opens: mpsc::Receiver<MuxOpen>,
    inbound: mpsc::Sender<yamux::Stream>,
) -> Result<()> {
    let mut pending: Option<MuxOpen> = None;
    loop {
        enum Event {
            Inbound(Option<yamux::Result<yamux::Stream>>),
            Outbound(yamux::Result<yamux::Stream>),
        }
        tokio::select! {
            open = opens.recv(), if pending.is_none() => { pending = open; }
            event = poll_fn(|cx| {
                if let Poll::Ready(stream) = connection.poll_next_inbound(cx) { return Poll::Ready(Event::Inbound(stream)); }
                if pending.is_some()
                    && let Poll::Ready(stream) = connection.poll_new_outbound(cx)
                {
                    return Poll::Ready(Event::Outbound(stream));
                }
                Poll::Pending
            }) => match event {
                Event::Inbound(Some(Ok(stream))) => inbound.try_send(stream).map_err(|_| anyhow::anyhow!("remote peer exceeded inbound admission bound"))?,
                Event::Inbound(Some(Err(error))) => return Err(error.into()),
                Event::Inbound(None) => bail!("remote multiplexed connection reached EOF"),
                Event::Outbound(result) => { if let Some(reply) = pending.take() { let _ = reply.send(result.map_err(Into::into)); } }
            }
        }
    }
}
async fn open_mux(opens: &mpsc::Sender<MuxOpen>) -> Result<yamux::Stream> {
    let (reply, rx) = oneshot::channel();
    opens
        .send(reply)
        .await
        .context("remote multiplexer stopped")?;
    rx.await
        .context("remote multiplexer stopped opening stream")?
}
async fn read_control(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<Control> {
    let mut header = [0; remote_wire::HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .context("reading remote control header")?;
    let length = remote_wire::frame_payload_len(&header)?;
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .context("reading remote control payload")?;
    remote_wire::decode(&bytes)
}
async fn write_control(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    control: &Control,
) -> Result<()> {
    let mut codec = remote_wire::Codec::new();
    let frame = remote_wire::encode_frame(codec.encode(control)?)?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

/// The unreaped group leader pins its numeric process-group identity. Descendant
/// proxy/jump processes inherit that group; daemon/application processes run on
/// the remote host and are never members of this local transport group.
struct OwnedSsh {
    child: tokio::process::Child,
}
impl OwnedSsh {
    fn terminate(&mut self) {
        if let Some(pid) = self.child.id() {
            // Signal before wait: reaping first would permit PID/PGID reuse.
            // process_group(0) made this child, not the client, the group leader.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            let _ = self.child.start_kill();
        }
    }
}
impl Drop for OwnedSsh {
    fn drop(&mut self) {
        self.terminate();
    }
}

async fn run_ssh(
    mut command: Command,
    hello: Hello,
    receiver: mpsc::Receiver<OpenPair>,
    stop: watch::Receiver<bool>,
    ready: sync::SyncSender<Result<Welcome>>,
    cancellation: CancellationToken,
) -> Result<()> {
    ensure!(!cancellation.is_cancelled(), "remote admission cancelled");
    let mut owned = OwnedSsh {
        child: command
            .process_group(0)
            .spawn()
            .context("launching system OpenSSH")?,
    };
    let io = Duplex {
        read: owned.child.stdout.take().unwrap(),
        write: owned.child.stdin.take().unwrap(),
    };
    let mut stderr = owned.child.stderr.take().unwrap();
    let diagnostics = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LIMIT)));
    let diagnostics_task = diagnostics.clone();
    let mut stderr_task = tokio::spawn(async move {
        let mut buffer = [0; 4096];
        while let Ok(count) = stderr.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            let mut ring = diagnostics_task.lock().unwrap();
            for byte in &buffer[..count] {
                if ring.len() == STDERR_LIMIT {
                    ring.pop_front();
                }
                ring.push_back(*byte);
            }
        }
    });
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(anyhow::anyhow!("owned SSH transport cancelled")),
        result = client_transport(io, hello, receiver, stop, ready) => result,
    };
    owned.terminate();
    let _ = owned.child.wait().await;
    // Process exit doesn't establish that its stderr pipe has been drained.
    // An inherited writer must not keep attachment cleanup waiting forever.
    if timeout(Duration::from_millis(250), &mut stderr_task)
        .await
        .is_err()
    {
        stderr_task.abort();
        let _ = stderr_task.await;
    }
    result.map_err(|error| {
        let ring = diagnostics.lock().unwrap();
        let bytes: Vec<u8> = ring.iter().copied().collect();
        let text: String = String::from_utf8_lossy(&bytes)
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    '\u{fffd}'
                } else {
                    c
                }
            })
            .collect();
        error.context(format!("owned SSH transport ended; bounded stderr: {text}"))
    })
}

async fn client_transport<R, W>(
    io: Duplex<R, W>,
    hello: Hello,
    mut receiver: mpsc::Receiver<OpenPair>,
    mut stop: watch::Receiver<bool>,
    ready: sync::SyncSender<Result<Welcome>>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (opens, open_rx) = mpsc::channel(STREAMS);
    let (incoming, mut incoming_rx) = mpsc::channel(1);
    let mut workers = JoinSet::new();
    let mut bridges = JoinSet::new();
    workers.spawn(drive_mux(
        yamux::Connection::new(io.compat(), mux_config(), yamux::Mode::Client),
        open_rx,
        incoming,
    ));
    let handshake = async {
        let mut control = open_mux(&opens).await?.compat();
        write_control(&mut control, &Control::Hello(hello.clone())).await?;
        let welcome = match read_control(&mut control).await? {
            Control::Welcome(welcome) => welcome,
            Control::Failure(error) => {
                bail!("remote helper rejected connection: {}", error.message)
            }
            _ => bail!("remote helper omitted Welcome"),
        };
        welcome.validate_for(&hello)?;
        Ok::<_, anyhow::Error>((control, welcome))
    };
    let handshake = tokio::select! {
        result = timeout(HANDSHAKE_TIMEOUT, handshake) => result.context("remote handshake deadline")?,
        _ = stop.changed() => bail!("remote connection abandoned during handshake"),
    };
    let (control, welcome) = match handshake {
        Ok(result) => result,
        Err(error) => {
            let _ = ready.send(Err(anyhow::anyhow!("{error:#}")));
            return Err(error);
        }
    };
    let heartbeat = Duration::from_millis(welcome.heartbeat_interval_ms.into());
    let lease = Duration::from_millis(welcome.lease_timeout_ms.into());
    ready
        .send(Ok(welcome))
        .map_err(|_| anyhow::anyhow!("remote caller abandoned connection"))?;
    workers.spawn(client_lease(control, heartbeat, lease));
    loop {
        tokio::select! {
            _ = stop.changed() => {
                // RegisteredClient already revoked new operations and closed its
                // lifetime/observations. Keep the mux and heartbeat alive while
                // accepted mutations deliver their original replies; never retry.
                receiver.close();
                while let Ok(pair) = receiver.try_recv() { let _ = pair.reply.send(Err(anyhow::anyhow!("remote attachment disconnected before dispatch"))); }
                timeout(DRAIN_TIMEOUT, async { while bridges.join_next().await.is_some() {} })
                    .await.context("remote accepted-mutation drain deadline; remaining outcomes unknown")?;
                return Ok(());
            }
            _ = bridges.join_next(), if !bridges.is_empty() => {}
            incoming = incoming_rx.recv() => { drop(incoming); bail!("remote helper opened an unsolicited stream"); }
            worker = workers.join_next() => {
                match worker { Some(Ok(Ok(()))) => bail!("remote transport worker unexpectedly ended"), Some(Ok(Err(error))) => return Err(error), Some(Err(error)) => return Err(error.into()), None => bail!("remote transport workers stopped") }
            }
            pair = receiver.recv(), if bridges.len() < STREAMS - 1 => {
                let Some(pair) = pair else { return Ok(()); };
                let stream = match timeout(OPEN_TIMEOUT, open_mux(&opens)).await {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(error)) => { let _ = pair.reply.send(Err(error)); continue; }
                    Err(error) => { let _ = pair.reply.send(Err(error.into())); continue; }
                };
                let OpenPair { stream: local, permit, reply } = pair;
                local.set_nonblocking(true)?;
                let local = tokio::net::UnixStream::from_std(local)?;
                if reply.send(Ok(())).is_err() { continue; }
                // Per-request failures remain native EOF/errors, not a whole-transport failure.
                bridges.spawn(async move { let _permit = permit; let _ = bridge(local, stream, false).await; });
            }
        }
    }
}

/// Cancel the other direction on EOF. Native clients do not half-close requests:
/// client-side EOF means cancellation; daemon-side EOF follows the final reply.
/// Calling close only on the response direction sends FIN after queued bytes.
/// Dropping an open yamux stream instead sends RST (yamux on_drop_stream).
async fn bridge(local: tokio::net::UnixStream, remote: yamux::Stream, helper: bool) -> Result<()> {
    let (mut local_read, mut local_write) = local.into_split();
    let (mut remote_read, mut remote_write) = tokio::io::split(remote.compat());
    let local_to_remote = async {
        copy_chunks(&mut local_read, &mut remote_write).await?;
        if helper {
            remote_write.shutdown().await?;
        }
        Ok::<_, std::io::Error>(())
    };
    let remote_to_local = async {
        copy_chunks(&mut remote_read, &mut local_write).await?;
        if !helper {
            local_write.shutdown().await?;
        }
        Ok::<_, std::io::Error>(())
    };
    tokio::select! { result = local_to_remote => result?, result = remote_to_local => result? }
    Ok(())
}
async fn copy_chunks<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    read: &mut R,
    write: &mut W,
) -> std::io::Result<()> {
    let mut bytes = [0; CHUNK];
    loop {
        let count = read.read(&mut bytes).await?;
        if count == 0 {
            return Ok(());
        }
        write.write_all(&bytes[..count]).await?;
        // yamux's bounded per-stream command queue and 16 KiB split size bound
        // outbound storage; yield even when all writes are immediately ready.
        tokio::task::yield_now().await;
    }
}

/// Run only in the dedicated helper process. All native UDS connections therefore
/// have the same authenticated kernel peer PID; no local authority is synthesized.
pub fn serve_helper(resolver: impl FnOnce(&Hello) -> Result<ResolvedSession>) -> Result<()> {
    use std::io::IsTerminal;
    ensure!(
        !std::io::stdin().is_terminal() && !std::io::stdout().is_terminal(),
        "remote helper requires piped binary stdin and stdout"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(helper_transport(resolver))
}
async fn helper_transport(resolver: impl FnOnce(&Hello) -> Result<ResolvedSession>) -> Result<()> {
    use std::os::fd::AsFd;
    let input = Pipe::new(std::io::stdin().as_fd().try_clone_to_owned()?.into())?;
    let output = Pipe::new(std::io::stdout().as_fd().try_clone_to_owned()?.into())?;
    let io = Duplex {
        read: input,
        write: output,
    };
    let (opens, open_rx) = mpsc::channel(STREAMS);
    let (incoming, mut incoming_rx) = mpsc::channel(STREAMS);
    let mut workers = JoinSet::new();
    let mut bridges = JoinSet::new();
    workers.spawn(drive_mux(
        yamux::Connection::new(io.compat(), mux_config(), yamux::Mode::Server),
        open_rx,
        incoming,
    ));
    let _opens = opens; // No outbound native streams; keep driver channel live.
    let mut control = timeout(HANDSHAKE_TIMEOUT, incoming_rx.recv())
        .await
        .context("remote Hello stream deadline")?
        .context("remote transport closed before Hello")?
        .compat();
    let hello = match timeout(HANDSHAKE_TIMEOUT, read_control(&mut control))
        .await
        .context("remote Hello deadline")??
    {
        Control::Hello(hello) => hello,
        _ => bail!("first remote control must be Hello"),
    };
    let resolved = match resolver(&hello) {
        Ok(resolved) => resolved,
        Err(error) => {
            let message: String = format!("{error:#}")
                .chars()
                .filter(|c| !c.is_control())
                .take(1024)
                .collect();
            let _ = timeout(
                OPEN_TIMEOUT,
                write_control(
                    &mut control,
                    &Control::Failure(remote_wire::Failure { message }),
                ),
            )
            .await;
            return Err(error);
        }
    };
    let welcome = Welcome {
        session: resolved.info,
        connection_nonce: hello.connection_nonce,
        capabilities: wire::SUPPORTED_CAPABILITIES,
        agent_socket: if hello.forward_agent {
            resolved.agent_socket
        } else {
            None
        },
        heartbeat_interval_ms: HEARTBEAT_MS,
        lease_timeout_ms: LEASE_MS,
    };
    welcome.validate_for(&hello)?;
    timeout(
        OPEN_TIMEOUT,
        write_control(&mut control, &Control::Welcome(welcome)),
    )
    .await
    .context("remote Welcome deadline")??;
    workers.spawn(helper_lease(
        control,
        Duration::from_millis(LEASE_MS.into()),
    ));
    let permits = Arc::new(Semaphore::new(STREAMS - 1));
    loop {
        tokio::select! {
            _ = bridges.join_next(), if !bridges.is_empty() => {}
            worker = workers.join_next() => match worker {
                Some(Ok(Err(error))) => return Err(error), Some(Err(error)) => return Err(error.into()),
                Some(Ok(Ok(()))) => return Ok(()), None => return Ok(()),
            },
            stream = incoming_rx.recv(), if bridges.len() < STREAMS - 1 => {
                let stream = stream.context("remote transport closed")?;
                let permit = permits.clone().try_acquire_owned().context("remote native stream limit exceeded")?;
                let socket = resolved.socket.clone();
                bridges.spawn(async move {
                    let _permit = permit;
                    if let Ok(Ok(local)) = timeout(OPEN_TIMEOUT, tokio::net::UnixStream::connect(socket)).await
                        && socket_bounds(&local).is_ok()
                    {
                        let _ = bridge(local, stream, true).await;
                    }
                });
            }
        }
    }
}

fn socket_bounds(socket: &impl std::os::fd::AsRawFd) -> Result<()> {
    let size: libc::c_int = (2 * CHUNK) as libc::c_int;
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: valid owned socket descriptor, correctly sized integer pointer.
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        };
        ensure!(
            result == 0,
            "setting remote stream socket buffer: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Tokio stdin uses a non-cancellable blocking thread. The helper instead owns
/// nonblocking pipe descriptors so parent EOF/deadline can always terminate it.
struct Pipe(tokio::io::unix::AsyncFd<std::fs::File>);
impl Pipe {
    fn new(file: std::fs::File) -> Result<Self> {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl acts on the valid owned descriptor; no pointers involved.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0,
            "reading helper pipe flags: {}",
            std::io::Error::last_os_error()
        );
        let result =
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        ensure!(
            result == 0,
            "setting helper pipe nonblocking: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: File owns this descriptor. Pipe never exposes, replaces or
        // closes it before AsyncFd deregisters and drops the owned file.
        Ok(Self(unsafe { tokio::io::unix::AsyncFd::register(file)? }))
    }
}
impl tokio::io::AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_read_ready(cx))?;
            match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                file.read(buf.initialize_unfilled())
            }) {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => continue,
            }
        }
    }
}
impl tokio::io::AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::io::Write;
        loop {
            let mut ready = std::task::ready!(self.0.poll_write_ready(cx))?;
            match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                file.write(bytes)
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn client_lease(
    control: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    heartbeat: Duration,
    lease: Duration,
) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    async fn send(
        write: &mut (impl tokio::io::AsyncWrite + Unpin),
        sent: &AtomicU64,
        heartbeat: Duration,
        lease: Duration,
    ) -> Result<()> {
        let mut timer = tokio::time::interval(heartbeat);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sequence = 0u64;
        loop {
            timer.tick().await;
            sequence = sequence
                .checked_add(1)
                .context("remote heartbeat sequence exhausted")?;
            sent.store(sequence, Ordering::Release);
            timeout(lease, write_control(write, &Control::Ping { sequence }))
                .await
                .context("remote heartbeat write deadline")??;
        }
    }
    async fn receive(
        read: &mut (impl tokio::io::AsyncRead + Unpin),
        sent: &AtomicU64,
        lease: Duration,
    ) -> Result<()> {
        let mut expected = 1u64;
        loop {
            match timeout(lease, read_control(read))
                .await
                .context("remote helper receive lease expired")??
            {
                Control::Pong { sequence }
                    if sequence == expected && sequence <= sent.load(Ordering::Acquire) =>
                {
                    expected = expected
                        .checked_add(1)
                        .context("remote heartbeat sequence exhausted")?;
                }
                _ => bail!("remote helper sent invalid heartbeat control"),
            }
        }
    }
    let (mut read, mut write) = tokio::io::split(control);
    let sent = AtomicU64::new(0);
    tokio::try_join!(
        send(&mut write, &sent, heartbeat, lease),
        receive(&mut read, &sent, lease)
    )?;
    Ok(())
}

async fn helper_lease(
    mut control: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    lease: Duration,
) -> Result<()> {
    let mut previous = 0u64;
    loop {
        match timeout(lease, read_control(&mut control))
            .await
            .context("remote parent receive lease expired")??
        {
            Control::Ping { sequence }
                if sequence
                    == previous
                        .checked_add(1)
                        .context("heartbeat sequence exhausted")? =>
            {
                previous = sequence;
                timeout(
                    lease,
                    write_control(&mut control, &Control::Pong { sequence }),
                )
                .await
                .context("remote heartbeat write deadline")??;
            }
            Control::Close => return Ok(()),
            _ => bail!("invalid remote parent control"),
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
