//! Shared private Unix-socket request serving for host and session endpoints.
//!
//! One framing, admission and response-drain implementation serves both roles.

use crate::registry::{Registry, RegistryBusy, SessionClosing};
mod administration;
mod capacity;
mod clients;
mod status;
use anyhow::{Context, Result, bail};
#[cfg(test)]
use bkpr_protocol::PaneRequest;
use bkpr_protocol::wire::{self, Codec, Kind, MAX_REQUEST, MAX_RESPONSE};
use bkpr_protocol::{
    AgentRequest, Operation, PROTOCOL_VERSION, PaneSpawnSpec, Payload, Request, RequestFailure,
    RequestOutcome, Response,
};
use capacity::{FrameBuffers, WorkStages};
pub(crate) use clients::{ClientOwner, lifetime_connected, send_lifetime_event};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
// These budgets cover transient wire work, never connected clients or observers.
const FRAME_WORKERS: usize = 4;

// Decoded requests, including startup waiters and known closing rejections, hold
// this guard until their reply is written. Idle sockets and lifetime watches do not.
#[derive(Default)]
struct ResponseDrain {
    pending: Mutex<usize>,
    changed: Condvar,
}
struct PendingResponse(Arc<ResponseDrain>);
impl PendingResponse {
    fn track(drain: &Arc<ResponseDrain>) -> Self {
        let mut pending = drain.pending.lock().unwrap();
        *pending += 1;
        Self(drain.clone())
    }
}
impl Drop for PendingResponse {
    fn drop(&mut self) {
        let mut pending = self.0.pending.lock().unwrap();
        *pending -= 1;
        self.0.changed.notify_all();
    }
}
#[derive(Default)]
struct SignalRegistrations(Vec<signal_hook::SigId>);
impl Drop for SignalRegistrations {
    fn drop(&mut self) {
        for registration in self.0.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}
struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct SocketLease(std::fs::File);
impl Drop for SocketLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[derive(Default)]
struct PredecodeReaders {
    state: Mutex<PredecodeState>,
    changed: Condvar,
}
#[derive(Default)]
struct PredecodeState {
    sockets: BTreeMap<i32, UnixStream>,
    workers: usize,
}
struct PredecodeReader {
    stop: Stop,
    id: i32,
    active: bool,
}
impl PredecodeReader {
    fn admit(&mut self, responses: &Arc<ResponseDrain>) -> Result<PendingResponse> {
        let mut state = self.stop.readers.state.lock().unwrap();
        anyhow::ensure!(
            !self.stop.flag.load(Ordering::Acquire),
            SessionClosing("endpoint is shutting down")
        );
        let socket = state.sockets.remove(&self.id);
        let pending = PendingResponse::track(responses);
        state.workers -= 1;
        self.active = false;
        self.stop.readers.changed.notify_all();
        drop(state);
        drop(socket);
        Ok(pending)
    }
}
impl Drop for PredecodeReader {
    fn drop(&mut self) {
        if self.active {
            let mut state = self.stop.readers.state.lock().unwrap();
            let socket = state.sockets.remove(&self.id);
            state.workers -= 1;
            self.stop.readers.changed.notify_all();
            drop(state);
            drop(socket);
        }
    }
}

#[derive(Clone)]
pub(crate) struct Stop {
    pub(crate) flag: Arc<AtomicBool>,
    pub(crate) closing: Arc<AtomicBool>,
    wake: Arc<UnixStream>,
    readers: Arc<PredecodeReaders>,
}
impl Stop {
    pub(crate) fn new() -> Result<(Self, UnixStream)> {
        let (read, write) = UnixStream::pair()?;
        write.set_nonblocking(true)?;
        Ok((
            Self {
                flag: Arc::new(AtomicBool::new(false)),
                closing: Arc::new(AtomicBool::new(false)),
                wake: Arc::new(write),
                readers: Arc::new(PredecodeReaders::default()),
            },
            read,
        ))
    }
    pub(crate) fn request(&self) {
        self.closing.store(true, Ordering::Release);
        self.flag.store(true, Ordering::Release);
        let _ = (&*self.wake).write_all(b"\0");
    }
    fn register_reader(&self, stream: &UnixStream) -> Result<PredecodeReader> {
        let mut state = self.readers.state.lock().unwrap();
        anyhow::ensure!(
            !self.flag.load(Ordering::Acquire),
            SessionClosing("endpoint is shutting down")
        );
        let socket = stream.try_clone()?;
        let id = socket.as_raw_fd();
        state.sockets.insert(id, socket);
        state.workers += 1;
        Ok(PredecodeReader {
            stop: self.clone(),
            id,
            active: true,
        })
    }
    fn cancel_readers(&self) {
        let sockets = std::mem::take(&mut self.readers.state.lock().unwrap().sockets);
        for socket in sockets.into_values() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
    fn wait_for_readers(&self) {
        let state = self.readers.state.lock().unwrap();
        drop(
            self.readers
                .changed
                .wait_while(state, |state| state.workers != 0)
                .unwrap(),
        );
    }
}
#[derive(Clone)]
enum Service {
    Session(Registry),
    Host(crate::daemon::HostDaemon),
}
impl Service {
    fn registry(&self) -> Option<&Registry> {
        match self {
            Self::Session(registry) => Some(registry),
            Self::Host(_) => None,
        }
    }
    fn shutdown(&self) -> Result<()> {
        match self {
            Self::Session(registry) => registry.shutdown(),
            Self::Host(host) => host.shutdown(),
        }
    }
}
pub(crate) type Ready = Arc<dyn Fn(std::result::Result<(), String>) + Send + Sync>;
// Native wrapper callbacks must run during bootstrap, but clients must never
// attach to a half-restored layout. Cancellation releases all waiting requests.
struct StartupGate {
    complete: Mutex<bool>,
    changed: Condvar,
    cancelled: AtomicBool,
}
impl StartupGate {
    fn finish(&self) {
        *self.complete.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn cancel(&self) {
        let _complete = self.complete.lock().unwrap();
        self.cancelled.store(true, Ordering::Release);
        self.changed.notify_all();
    }
    fn wait(&self, shutdown: &AtomicBool) -> Result<()> {
        let complete = self.complete.lock().unwrap();
        let _complete = self
            .changed
            .wait_while(complete, |complete| {
                !*complete
                    && !shutdown.load(Ordering::Acquire)
                    && !self.cancelled.load(Ordering::Acquire)
            })
            .unwrap();
        anyhow::ensure!(
            !shutdown.load(Ordering::Acquire) && !self.cancelled.load(Ordering::Acquire),
            SessionClosing("session is shutting down")
        );
        Ok(())
    }
}
fn startup_callback(operation: &Operation) -> bool {
    match operation {
        // Shared control can mutate another saved pane's ownership. It has the
        // same startup admission boundary as ordinary client control.
        Operation::Agent { request, .. } => !matches!(request, AgentRequest::Control { .. }),
        Operation::Context { .. }
        | Operation::HarnessBegin { .. }
        | Operation::HarnessEvent { .. }
        | Operation::HarnessContext { .. }
        | Operation::Shutdown => true,
        _ => false,
    }
}

struct DiagnosticOperation {
    name: &'static str,
    target: Option<crate::diagnostics::OpaqueId>,
    success: &'static str,
}
fn diagnostic_operation(operation: &Operation) -> Option<DiagnosticOperation> {
    // An allowlist, not Debug/serde: even failure paths cannot persist request
    // bodies, paths, titles, capabilities, input or provider error text.
    let (name, target, success) = match operation {
        Operation::Client { request, .. } => return diagnostic_operation(request),
        Operation::Agent {
            launch_id, request, ..
        } => match request {
            AgentRequest::Control { request, .. } => return diagnostic_operation(request),
            AgentRequest::SetTitle { .. } => ("agent.title", Some(launch_id.as_str()), "success"),
            AgentRequest::SendMessage { to, .. } => ("agent.send", Some(to.as_str()), "success"),
            AgentRequest::AckMessage { .. } => ("agent.ack", Some(launch_id.as_str()), "success"),
            AgentRequest::ReceiverRegister { .. } => {
                ("agent.receiver", Some(launch_id.as_str()), "success")
            }
            AgentRequest::WakeOutcome { outcome, .. } => {
                let outcome = match outcome {
                    bkpr_protocol::AgentWakeOutcome::Accepted => "accepted",
                    bkpr_protocol::AgentWakeOutcome::Failed { .. } => "failed",
                    bkpr_protocol::AgentWakeOutcome::Unknown { .. } => "unknown",
                };
                ("agent.wake", Some(launch_id.as_str()), outcome)
            }
            _ => return None,
        },
        Operation::ClientRegister { .. } => ("client.register", None, "success"),
        Operation::ClientActivate { .. } => ("client.activate", None, "success"),
        Operation::ClientDetach => ("client.detach", None, "success"),
        Operation::HarnessBegin { .. } => ("harness.begin", None, "success"),
        Operation::HarnessEvent {
            launch_id, event, ..
        } => {
            let name = match event {
                bkpr_protocol::CaptureEvent::Bind { .. } => "harness.bind",
                bkpr_protocol::CaptureEvent::Error { .. } => "harness.error",
                bkpr_protocol::CaptureEvent::End => "harness.end",
                _ => return None,
            };
            (name, Some(launch_id.as_str()), "success")
        }
        Operation::WorkspaceCreate { .. } => ("workspace.create", None, "success"),
        Operation::WorkspaceRename { workspace_id, .. } => {
            ("workspace.rename", Some(workspace_id.as_str()), "success")
        }
        Operation::WorkspaceClose { workspace_id, .. } => {
            ("workspace.close", Some(workspace_id.as_str()), "success")
        }
        Operation::WindowCreate { workspace_id, .. } => {
            ("window.create", Some(workspace_id.as_str()), "success")
        }
        Operation::WindowRename { window_id, .. } => {
            ("window.rename", Some(window_id.as_str()), "success")
        }
        Operation::WindowClose { window_id, .. } => {
            ("window.close", Some(window_id.as_str()), "success")
        }
        Operation::PaneSpawn { window_id, .. } => ("pane.create", window_id.as_deref(), "success"),
        Operation::PaneReopen { pane_id, .. } => ("pane.reopen", Some(pane_id.as_str()), "success"),
        Operation::PaneResume { pane_id, .. } => ("pane.resume", Some(pane_id.as_str()), "success"),
        Operation::PaneTerminate { pane_id } => {
            ("pane.terminate", Some(pane_id.as_str()), "success")
        }
        Operation::PaneForget { pane_id } => ("pane.forget", Some(pane_id.as_str()), "success"),
        Operation::PaneRename { pane_id, .. } => ("pane.rename", Some(pane_id.as_str()), "success"),
        Operation::LayoutUpdate { window_id, .. } => {
            ("layout.update", Some(window_id.as_str()), "success")
        }
        Operation::Shutdown => ("session.shutdown", None, "success"),
        Operation::DaemonShutdown { daemon_id } => {
            ("daemon.shutdown", Some(daemon_id.as_str()), "success")
        }
        Operation::SessionOpen { .. } => ("session.open", None, "success"),
        // Includes inspect/read, subscription/status and every pane input request.
        _ => return None,
    };
    Some(DiagnosticOperation {
        name,
        target: target.and_then(crate::diagnostics::OpaqueId::new),
        success,
    })
}
#[cfg(test)]
type DispatchObserver = Arc<dyn Fn(&Operation) + Send + Sync>;

/// Bound private socket and exclusive daemon lease, before request dispatch starts.
///
/// Dropping closes the listener, removes the socket path and releases the lease.
pub struct BoundSocket {
    listener: UnixListener,
    socket: PathBuf,
    _guard: SocketGuard,
    _lease: SocketLease,
    #[cfg(test)]
    dispatch_observer: Option<DispatchObserver>,
}
/// Acquire the exclusive socket lease and bind without dispatching requests.
///
/// Creates missing parent directories privately and replaces only a current-user
/// socket whose listener refuses connections.
///
/// # Errors
/// Returns an error for an unsafe parent or lock, an existing owner or live
/// listener, an unsafe stale socket, or filesystem/listener setup failure.
pub fn bind(socket: &Path) -> Result<BoundSocket> {
    let parent = socket.parent().context("socket needs parent directory")?;
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = std::fs::metadata(parent).context("create a private socket directory first")?;
    if !std::fs::symlink_metadata(parent)?.is_dir() {
        bail!("socket parent cannot be a symlink");
    }
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        bail!("socket parent must be owned by current user and mode 0700");
    }
    let lease = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket.with_extension("lock"))?;
    let metadata = lease.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        bail!("socket lock must be private and current-user-owned");
    }
    fs2::FileExt::try_lock_exclusive(&lease).context("socket already owned by another daemon")?;
    let _lease = SocketLease(lease);
    if socket.exists() {
        match UnixStream::connect(socket) {
            Ok(_) => bail!("socket already has a live listener"),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                let metadata = std::fs::symlink_metadata(socket)?;
                if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() }
                {
                    bail!("refusing to replace foreign or non-socket path");
                }
                std::fs::remove_file(socket)?;
            }
            Err(error) => return Err(error).context("checking existing daemon socket"),
        }
    }
    let listener = UnixListener::bind(socket).context("binding private registry socket")?;
    let guard = SocketGuard(socket.to_owned());
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(BoundSocket {
        listener,
        socket: socket.to_owned(),
        _guard: guard,
        _lease,
        #[cfg(test)]
        dispatch_observer: None,
    })
}
/// Bind and serve a registry without running an initial pane bootstrap.
///
/// Blocks until shutdown or a serving failure. See [`BoundSocket::serve`] for
/// shutdown behavior.
///
/// # Errors
/// Returns binding, serving or registry shutdown errors.
pub fn serve(socket: &Path, registry: Registry) -> Result<()> {
    bind(socket)?.serve(registry)
}
impl BoundSocket {
    /// Serves a lower-level session component without installing process signals.
    ///
    /// Session termination drains decoded replies and accepted spawns. Production
    /// callers use [`crate::daemon::HostDaemon`] to own this component.
    pub fn serve(self, registry: Registry) -> Result<()> {
        self.serve_bootstrap(registry, None)
    }
    /// Serves a session component with callback-safe bootstrap admission.
    ///
    /// Blocks until that session ends or shuts down; ordinary requests wait for
    /// bootstrap, while native callbacks remain available. No process signals
    /// or host readiness records are installed by session components.
    pub fn serve_bootstrap(self, registry: Registry, initial: Option<PaneSpawnSpec>) -> Result<()> {
        let (stop, wake) = Stop::new()?;
        self.serve_service(Service::Session(registry), initial, stop, wake, None, false)
    }
    pub(crate) fn serve_session(
        self,
        registry: Registry,
        initial: Option<PaneSpawnSpec>,
        stop: Stop,
        wake: UnixStream,
        ready: Ready,
    ) -> Result<()> {
        self.serve_service(
            Service::Session(registry),
            initial,
            stop,
            wake,
            Some(ready),
            false,
        )
    }
    pub(crate) fn serve_host(
        self,
        host: crate::daemon::HostDaemon,
        stop: Stop,
        wake: UnixStream,
    ) -> Result<()> {
        let mut signals = SignalRegistrations::default();
        for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
            signals
                .0
                .push(signal_hook::flag::register(signal, stop.flag.clone())?);
            signals.0.push(signal_hook::low_level::pipe::register(
                signal,
                stop.wake.try_clone()?,
            )?);
        }
        self.serve_service(Service::Host(host), None, stop, wake, None, true)
    }
    fn serve_service(
        self,
        service: Service,
        initial: Option<PaneSpawnSpec>,
        stop: Stop,
        wake_read: UnixStream,
        ready: Option<Ready>,
        announce: bool,
    ) -> Result<()> {
        let Self {
            listener,
            socket,
            _guard,
            _lease,
            #[cfg(test)]
            dispatch_observer,
        } = self;
        let service = Arc::new(service);
        let shutdown = stop.flag.clone();
        let request_buffers = Arc::new(FrameBuffers::default());
        let decode_work = Arc::new(WorkStages::new(FRAME_WORKERS));
        let response_work = Arc::new(WorkStages::new(FRAME_WORKERS));
        let response_buffers = Arc::new(FrameBuffers::new(
            8 * 1024 * 1024,
            FRAME_WORKERS * MAX_RESPONSE,
        ));
        let responses = Arc::new(ResponseDrain::default());
        let wake_write = stop.wake.clone();
        if let Some(registry) = service.registry() {
            let end_stop = stop.clone();
            registry.set_session_end_waker(Arc::new(move || end_stop.request()));
        }
        let startup = Arc::new(StartupGate {
            complete: Mutex::new(initial.is_none()),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });
        let bootstrap = initial
            .map(|spec| {
                let registry = service.registry().expect("session bootstrap").clone();
                let startup = startup.clone();
                let shutdown = shutdown.clone();
                let wake_write = wake_write.clone();
                let ready = ready.clone();
                thread::Builder::new()
                    .name("bkpr-bootstrap".into())
                    .spawn(move || {
                        // Include diagnostics/readiness in the worker failure boundary:
                        // a closed output fd must never strand the admission gate.
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                            || -> Result<()> {
                                let value = registry.bootstrap(spec)?;
                                if let Some(failures) =
                                    value.get("failures").and_then(|v| v.as_array())
                                {
                                    for failure in failures {
                                        writeln!(
                                            std::io::stderr().lock(),
                                            "bkprd: automatic restore: {failure}"
                                        )?;
                                    }
                                }
                                anyhow::ensure!(
                                    !shutdown.load(Ordering::Acquire)
                                        && !startup.cancelled.load(Ordering::Acquire),
                                    SessionClosing("session ended during bootstrap")
                                );
                                if let Some(ready) = &ready {
                                    ready(Ok(()));
                                }
                                Ok(())
                            },
                        ))
                        .map_err(|_| anyhow::anyhow!("bootstrap thread panicked"))
                        .and_then(|result| result);
                        let cancelled = shutdown.load(Ordering::Acquire)
                            || startup.cancelled.load(Ordering::Acquire);
                        if let Err(error) = &result
                            && let Some(ready) = &ready
                        {
                            ready(Err(format!("{error:#}")));
                        }
                        if result.is_err() && !cancelled {
                            shutdown.store(true, Ordering::Release);
                            let _ = (&*wake_write).write_all(b"\0");
                        }
                        startup.finish();
                        if cancelled { Ok(()) } else { result }
                    })
            })
            .transpose()?;
        if bootstrap.is_none() && announce {
            writeln!(
                std::io::stdout().lock(),
                "{}",
                json!({"version":PROTOCOL_VERSION,"ready":true,"socket":bkpr_protocol::path_wire::WirePath(&socket)})
            )?;
        }
        if bootstrap.is_none()
            && let Some(ready) = &ready
        {
            if shutdown.load(Ordering::Acquire) {
                ready(Err("session is shutting down".into()));
            } else {
                ready(Ok(()));
            }
        }
        let serving = (|| -> Result<()> {
            while !shutdown.load(Ordering::Acquire) {
                if !wait_for_accept(&listener, &wake_read, &shutdown)? {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        let mut reader = match stop.register_reader(&stream) {
                            Ok(reader) => reader,
                            Err(error) => {
                                if !shutdown.load(Ordering::Acquire) {
                                    let _ = writeln!(
                                        std::io::stderr().lock(),
                                        "bkprd: registering request reader: {error}"
                                    );
                                }
                                continue;
                            }
                        };
                        let endpoint = Arc::downgrade(&service);
                        let shutdown = shutdown.clone();
                        let closing = stop.closing.clone();
                        let request_buffers = request_buffers.clone();
                        let decode_work = decode_work.clone();
                        let response_work = response_work.clone();
                        let wake_write = wake_write.clone();
                        let responses = responses.clone();
                        let startup = startup.clone();
                        let response_buffers = response_buffers.clone();
                        #[cfg(test)]
                        let dispatch_observer = dispatch_observer.clone();
                        if let Err(error) = thread::Builder::new()
                    .name("bkpr-control".into())
                    .spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let incoming = match capacity::read_incoming(&mut stream, &request_buffers, &decode_work) {
                        capacity::Incoming::Administration(request) => {
                            administration::serve(
                                &mut stream,
                                request,
                                &mut reader,
                                administration::Resources {
                                    endpoint: &endpoint,
                                    responses: &responses,
                                    response_work: &response_work,
                                    response_buffers: &response_buffers,
                                    shutdown: &shutdown,
                                    closing: &closing,
                                    startup: &startup,
                                    wake: &wake_write,
                                },
                            );
                            return;
                        }
                        capacity::Incoming::Native(request) => request,
                    };
                    let mut stop = false;
                    let mut subscription = None;
                    let mut lifetime = None;
                    let mut status_stream = None;
                    let mut pending_response = None;
                    let mut dispatched = false;
                    let mut diagnostic = None;
                    let mut service = None;
                    let result = (|| -> Result<_> {
                        let (request, _request_buffer) = incoming?;
                        pending_response = Some(reader.admit(&responses)?);
                        service = endpoint.upgrade();
                        let service = service.as_ref().context("endpoint is shutting down")?;
                        diagnostic = diagnostic_operation(&request.operation);
                        let requested_subscription = match &request.operation {
                            Operation::Agent {
                                launch_id,
                                request: AgentRequest::ReceiverSubscribe { receiver_id, .. },
                                ..
                            } => Some((launch_id.clone(), receiver_id.clone())),
                            _ => None,
                        };
                        anyhow::ensure!(request.version == PROTOCOL_VERSION, "unsupported protocol version");
                        if let Service::Host(host) = service.as_ref() {
                            dispatched = true;
                            let result = host.request(request.operation, &|| {
                                !shutdown.load(Ordering::Acquire) && peer_connected(&stream)
                            })?;
                            stop = host.stop_requested();
                            return Ok(result);
                        }
                        let registry = service.registry().expect("session endpoint");
                        stop = matches!(&request.operation, Operation::Shutdown);
                        if !startup_callback(&request.operation) {
                            #[cfg(test)]
                            if !*startup.complete.lock().unwrap() {
                                registry.test_notify_request_wait();
                            }
                            if let Err(error) = startup.wait(&shutdown) {
                                registry.request_health()?;
                                return Err(error);
                            }
                        }
                        // Immutable daemon identity must remain readable when
                        // persistence is poisoned: normal stop validates its
                        // exact target through SessionInfo before cleanup.
                        if !matches!(&request.operation, Operation::SessionInfo | Operation::Shutdown)
                            && !matches!(&request.operation, Operation::Client { request, .. }
                                if matches!(request.as_ref(), Operation::SessionInfo))
                        {
                            registry.request_admission()?;
                        }
                        anyhow::ensure!(
                            !shutdown.load(Ordering::Acquire),
                            SessionClosing("session is shutting down")
                        );
                        #[cfg(test)]
                        if let Some(observer) = &dispatch_observer {
                            observer(&request.operation);
                        }
                        dispatched = true;
                        if stop {
                            closing.store(true, Ordering::Release);
                            startup.cancel();
                        }
                        let result = match request.operation {
                            Operation::ClientRegister { session_id, daemon_id, session_instance_id } => {
                                let (client, response) = registry.register_client(&stream, &session_id, &daemon_id, &session_instance_id)?;
                                lifetime = Some(client);
                                Payload::Control(response)
                            }
                            Operation::Client { authority, request: operation } => {
                                registry.client_request(authority, *operation, &stream)?
                            }
                            operation @ Operation::HarnessEvent {
                                event: bkpr_protocol::CaptureEvent::StatusEnroll { .. }, ..
                            } => {
                                let peer = authenticated_peer(&stream)?;
                                let (enrollment, response) = registry.register_native_status(
                                    &Request::new(operation), &peer, &|| peer_connected(&stream),
                                )?;
                                status_stream = Some((enrollment, peer));
                                Payload::Control(response)
                            }
                            Operation::Notifications { request } => {
                                registry.request_notifications(request, &|| {
                                    !shutdown.load(Ordering::Acquire) && peer_connected(&stream)
                                })?
                            }
                            operation @ (Operation::Context { .. } | Operation::HarnessBegin { .. } | Operation::HarnessEvent { .. }
                            | Operation::HarnessContext { .. } | Operation::Agent { .. }) => {
                                let peer = authenticated_peer(&stream)?;
                                registry.request_authenticated_cancellable(Request::new(operation), &peer, &|| peer_connected(&stream))?
                            }
                            operation => registry.request(Request::new(operation))?,
                        };
                        subscription = requested_subscription.and_then(|(launch_id, receiver_id)| {
                            let Payload::Control(value) = &result else { return None; };
                            let notification = value.get("notification")?;
                            Some((
                                launch_id,
                                receiver_id,
                                notification.get("notification_id")?.as_str()?.to_owned(),
                                notification.get("generation")?.as_u64()?,
                            ))
                        });
                        Ok(result)
                    })();
                    if pending_response.is_none() && shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    // Only a typed closing guard can authorize shutdown handling.
                    // In particular, never turn a generic error into a closing
                    // success because another request ended the session.
                    let result = match result {
                        Err(error) if error.is::<SessionClosing>() => {
                            service.as_ref().and_then(|service| service.registry()).map_or(Ok(()), Registry::request_health).and(Err(error))
                        }
                        result => result,
                    };
                    let reply = response(result, dispatched);
                    // Encoding is short work; a stalled reader retains only its
                    // byte reservation, never an encoder or decoder worker slot.
                    let response_stage = response_work.enter();
                    let mut codec = Codec::new();
                    let mut encoding_unknown = false;
                    let sent = match codec.encode_response(&reply) {
                        Ok(bytes) if bytes.len() <= MAX_RESPONSE => {
                            write_buffered_response(&mut stream, bytes, &response_buffers, response_stage)
                        }
                        _ => {
                            encoding_unknown = dispatched;
                            let fallback = response(
                                Err(anyhow::anyhow!("aggregate scene wire limit exceeded after dispatch; outcome unknown, do not replay")),
                                dispatched,
                            );
                            codec.encode_response(&fallback)
                                .and_then(|bytes| write_buffered_response(&mut stream, bytes, &response_buffers, response_stage))
                        }
                    };
                    drop(codec);
                    if let Some(diagnostic) = diagnostic {
                        let unknown = dispatched && (sent.is_err() || encoding_unknown)
                            || matches!(reply.failure.as_ref(), Some(RequestFailure::SessionClosing {
                                outcome: RequestOutcome::Unknown,
                            }));
                        crate::diagnostics::record(
                            diagnostic.name,
                            diagnostic.target.as_ref().map(crate::diagnostics::OpaqueId::as_str),
                            if unknown { "unknown" } else if reply.ok { diagnostic.success } else { "failed" },
                        );
                    }
                    if sent.is_err()
                        && let Some((launch_id, receiver_id, notification_id, generation)) = subscription
                    {
                        let _ = service.as_ref().and_then(|service| service.registry()).expect("session subscription").agent_subscription_disconnected(
                            &launch_id, &receiver_id, &notification_id, generation,
                        );
                    }
                    if let Some((enrollment, peer)) = status_stream {
                        drop(pending_response.take());
                        if sent.is_ok() {
                            let _ = status::serve(
                                &mut stream, service.as_ref().and_then(|service| service.registry()).expect("session status"), &enrollment, &peer, &shutdown,
                                status::Resources {
                                    request_buffers: &request_buffers, decode_work: &decode_work,
                                    response_work: &response_work, response_buffers: &response_buffers,
                                },
                            );
                        }
                        let _ = service.as_ref().and_then(|service| service.registry()).expect("session status").end_native_status(&enrollment);
                    }
                    if let Some(client) = lifetime {
                        if sent.is_ok() {
                            client.handshake_complete();
                            drop(pending_response.take());
                            clients::watch_client(&stream, service.as_ref().and_then(|service| service.registry()).expect("session client"), &client);
                        } else {
                            service.as_ref().and_then(|service| service.registry()).expect("session client").detach_client(&client, "disconnected");
                        }
                    }
                    drop(service);
                    drop(pending_response);
                    // A valid explicit shutdown closes the daemon even when owned cleanup
                    // reports an error. Keep the error response, not a poisoned listener.
                    if stop && dispatched {
                        shutdown.store(true, Ordering::Release);
                        let _ = (&*wake_write).write_all(b"\0");
                        startup.finish();
                    }
                }) {
                    // Dropping the failed spawn's closure closes only this socket.
                    // No request was dispatched and no drain guard was installed.
                    let _ = writeln!(std::io::stderr().lock(), "bkprd: starting connection worker: {error}");
                }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e)
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::ENOBUFS)
                        ) =>
                    {
                        let _ =
                            writeln!(std::io::stderr().lock(), "bkprd: accepting connection: {e}");
                        // Resource pressure is local and may clear when existing
                        // requests close; do not tear down unrelated live panes.
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        })();
        drop(listener);
        stop.request();
        stop.cancel_readers();
        decode_work.cancel();
        startup.finish();
        let cleanup = service.shutdown();
        let bootstrapped = bootstrap
            .map(|thread| {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("bootstrap thread panicked"))?
            })
            .transpose();
        stop.wait_for_readers();
        let mut pending = responses.pending.lock().unwrap();
        while *pending != 0 {
            pending = responses.changed.wait(pending).unwrap();
        }
        serving.and(bootstrapped.map(|_| ())).and(cleanup)
    }
}

fn response(result: Result<Payload>, dispatched: bool) -> Response {
    match result {
        Ok(value) => Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(value),
            error: None,
            failure: None,
        },
        Err(error) => Response {
            version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            failure: if let Some(busy) = error.downcast_ref::<RegistryBusy>() {
                Some(RequestFailure::Busy {
                    target_id: busy.target_id.clone(),
                })
            } else {
                error
                    .is::<SessionClosing>()
                    .then_some(RequestFailure::SessionClosing {
                        outcome: if dispatched {
                            RequestOutcome::Unknown
                        } else {
                            RequestOutcome::NotDispatched
                        },
                    })
            },
            error: Some(format!("{error:#}")),
        },
    }
}

fn write_buffered_response(
    stream: &mut UnixStream,
    bytes: &[u8],
    buffers: &FrameBuffers,
    stage: capacity::WorkStage<'_>,
) -> Result<()> {
    // If encoded buffers cannot be retained safely, fail this connection before
    // writing any reply. Existing clients then report the truthful transport
    // unknown outcome after dispatch, rather than a fabricated known failure.
    let _buffer = buffers.reserve(bytes.len())?;
    drop(stage);
    write_response(stream, bytes, Duration::from_secs(3))
}

fn write_response(stream: &mut UnixStream, bytes: &[u8], timeout: Duration) -> Result<()> {
    wire::write_frame(
        &mut ResponseWriter {
            stream,
            deadline: std::time::Instant::now() + timeout,
        },
        Kind::Response,
        bytes,
    )
}

struct ResponseWriter<'a> {
    stream: &'a UnixStream,
    deadline: std::time::Instant,
}
impl Write for ResponseWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let timeout = self
                .deadline
                .saturating_duration_since(std::time::Instant::now());
            if timeout.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            // Per-call nonblocking keeps lifetime sockets' shared file flags intact.
            let written = unsafe {
                libc::send(
                    self.stream.as_raw_fd(),
                    remaining.as_ptr().cast(),
                    remaining.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if written > 0 {
                remaining = &remaining[written as usize..];
                continue;
            }
            if written == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => {
                    let mut descriptor = libc::pollfd {
                        fd: self.stream.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    let millis = timeout.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
                    let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
                    if ready == 0 {
                        return Err(std::io::ErrorKind::TimedOut.into());
                    }
                    if ready < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                _ => return Err(error),
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn peer_connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
    }
    if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return false;
    }
    if descriptor.revents & libc::POLLIN != 0 {
        let mut byte = 0_u8;
        let count = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        return count > 0
            || (count < 0
                && matches!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ));
    }
    true
}

#[cfg(target_os = "linux")]
fn authenticated_peer(stream: &UnixStream) -> Result<crate::harness::Peer> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credential as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("Unix peer credentials");
    }
    anyhow::ensure!(
        length as usize == std::mem::size_of::<libc::ucred>()
            && credential.uid == unsafe { libc::geteuid() },
        "invalid Unix peer credentials"
    );
    // SO_PEERPIDFD pins the *socket's original peer*, not a possibly recycled claimed PID.
    // Older kernels fail closed for capture rather than silently weakening birth provenance.
    let mut fd = -1_i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            77,
            (&mut fd as *mut i32).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("birth-bound Unix peer authentication requires SO_PEERPIDFD");
    }
    anyhow::ensure!(
        fd >= 0 && length as usize == std::mem::size_of::<i32>(),
        "invalid Unix peer pidfd"
    );
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    let process = crate::harness_process::process(credential.pid)?.context("Unix peer exited")?;
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    anyhow::ensure!(
        result == 0 && poll.revents == 0,
        "Unix socket peer exited or changed birth"
    );
    Ok(crate::harness::Peer {
        pid: credential.pid,
        uid: credential.uid,
        birth: process.identity,
    })
}
#[cfg(target_os = "macos")]
fn authenticated_peer(stream: &UnixStream) -> Result<crate::harness::Peer> {
    // LOCAL_PEERTOKEN includes the kernel PID version. Match PROC_PIDUNIQIDENTIFIERINFO
    // as well as the microsecond birth read, so a recycled peer PID cannot lend authority.
    let mut token = [0_u32; 8];
    let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            6,
            token.as_mut_ptr().cast(),
            &mut size,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("Darwin Unix peer audit token");
    }
    anyhow::ensure!(
        size as usize == std::mem::size_of_val(&token) && token[1] == unsafe { libc::geteuid() },
        "foreign Darwin Unix peer"
    );
    #[repr(C)]
    // xnu bsd/sys/proc_info_private.h: this API structure is exactly 56 bytes.
    struct Unique {
        uuid: [u8; 16],
        unique: u64,
        parent_unique: u64,
        version: i32,
        original_parent_version: i32,
        reserved2: u64,
        reserved3: u64,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            bytes: i32,
        ) -> i32;
    }
    let process =
        crate::harness_process::process(token[5] as i32)?.context("Darwin Unix peer exited")?;
    let mut unique: Unique = unsafe { std::mem::zeroed() };
    let bytes = std::mem::size_of::<Unique>();
    let count = unsafe {
        proc_pidinfo(
            process.pid,
            17,
            0,
            (&mut unique as *mut Unique).cast(),
            bytes as i32,
        )
    };
    anyhow::ensure!(
        count as usize == bytes && unique.version as u32 == token[7],
        "Darwin Unix peer PID version changed"
    );
    Ok(crate::harness::Peer {
        pid: process.pid,
        uid: token[1],
        birth: process.identity,
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn authenticated_peer(_: &UnixStream) -> Result<crate::harness::Peer> {
    bail!("birth-bound Unix peer authentication unsupported")
}
fn wait_for_accept(
    listener: &UnixListener,
    wake: &UnixStream,
    shutdown: &AtomicBool,
) -> Result<bool> {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        if descriptors.iter().any(|descriptor| {
            descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        }) {
            bail!("pane listener or shutdown wake descriptor closed");
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            return Ok(true);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_waiters_are_released_on_commit_and_rejected_on_cancellation() {
        for cancelled in [false, true] {
            let gate = Arc::new(StartupGate {
                complete: Mutex::new(false),
                changed: Condvar::new(),
                cancelled: AtomicBool::new(false),
            });
            let waiting = gate.clone();
            let waiter = thread::spawn(move || waiting.wait(&AtomicBool::new(false)));
            if cancelled {
                gate.cancel();
            } else {
                gate.finish();
            }
            assert_eq!(waiter.join().unwrap().is_err(), cancelled);
        }
    }

    #[test]
    fn invalid_native_frames_never_dispatch_mutations() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let socket = root.path().join("daemon.sock");
        let bound = bind(&socket).unwrap();
        let serving = registry.clone();
        let worker = thread::spawn(move || bound.serve(serving));
        let before = registry.snapshot().unwrap();
        let mut codec = Codec::new();
        let bytes = codec
            .encode_request(&Request::new(Operation::WorkspaceCreate {
                name: Some("must-not-commit".into()),
                initial: None,
            }))
            .unwrap();
        let valid = wire::encode_frame(Kind::Request, bytes).unwrap();
        let mut invalid = Vec::new();
        let mut frame = valid.clone();
        frame[0] ^= 1;
        invalid.push(frame);
        let mut frame = valid.clone();
        frame[4..8].copy_from_slice(&(PROTOCOL_VERSION - 1).to_le_bytes());
        invalid.push(frame);
        let mut frame = valid.clone();
        frame[8] = Kind::Response as u8;
        invalid.push(frame);
        let mut frame = valid.clone();
        frame[12..16].copy_from_slice(&((MAX_REQUEST + 1) as u32).to_le_bytes());
        invalid.push(frame);
        let mut frame = valid;
        frame[16..24].copy_from_slice(&(1_u64 << 63).to_le_bytes());
        invalid.push(frame);
        invalid.push(wire::encode_frame(Kind::Request, &[0; 8]).unwrap());
        for frame in invalid {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(&frame).unwrap();
            let bytes = wire::read_frame(&mut stream, Kind::Response, MAX_RESPONSE).unwrap();
            let reply = wire::decode_response(&bytes).unwrap();
            assert!(!reply.ok);
            assert!(reply.result.is_none());
            assert!(reply.failure.is_none());
            let after = registry.snapshot().unwrap();
            assert_eq!(after.revision, before.revision);
            assert!(after.workspaces.is_empty());
        }
        let mut stream = UnixStream::connect(&socket).unwrap();
        wire::write_frame(
            &mut stream,
            Kind::Request,
            codec
                .encode_request(&Request::new(Operation::Shutdown))
                .unwrap(),
        )
        .unwrap();
        let bytes = wire::read_frame(&mut stream, Kind::Response, MAX_RESPONSE).unwrap();
        assert!(wire::decode_response(&bytes).unwrap().ok);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn listener_poll_handles_readiness_and_shutdown_wake() {
        let directory =
            std::env::temp_dir().join(format!("bkpr-listener-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        {
            let socket = directory.join("pane.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let _guard = SocketGuard(socket.clone());
            let (read, mut write) = UnixStream::pair().unwrap();
            let shutdown = Arc::new(AtomicBool::new(false));
            let _client = UnixStream::connect(socket).unwrap();
            assert!(wait_for_accept(&listener, &read, &shutdown).unwrap());
            let _accepted = listener.accept().unwrap();
            let stop = shutdown.clone();
            let signal = thread::spawn(move || {
                stop.store(true, Ordering::Release);
                write.write_all(b"\0").unwrap();
            });
            assert!(!wait_for_accept(&listener, &read, &shutdown).unwrap());
            signal.join().unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}

#[cfg(test)]
mod shutdown_tests;
#[cfg(all(test, target_os = "linux"))]
mod startup_tests;

#[cfg(all(test, target_os = "linux"))]
mod capacity_tests;
