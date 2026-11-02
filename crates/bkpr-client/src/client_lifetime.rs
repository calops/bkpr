use super::{checked_response, request_stream, validate_response};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{
    ClientAuthority, ClientEvent, Operation, PROTOCOL_VERSION, Request, Response, SessionInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsString,
    io::{Read, Write},
    net::Shutdown,
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_LIFETIME_LINE: usize = 64 * 1024;
const LIFECYCLE_READ_BUDGET: usize = 16 * 1024;

#[derive(Debug)]
struct DetachedBeforeDispatch;
impl std::fmt::Display for DetachedBeforeDispatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("client attachment detached; request not dispatched")
    }
}
impl std::error::Error for DetachedBeforeDispatch {}

/// Only local revocation before dispatch qualifies. In-flight cancellation
/// remains an unknown outcome and must never be reported as undispatched.
pub fn detached_before_dispatch(error: &anyhow::Error) -> bool {
    error.is::<DetachedBeforeDispatch>() && !super::outcome_unknown(error)
}

/// Same wire envelope as Request(Operation::Client), without cloning secrets
/// or allocating a Box for each key, poll, or other managed request.
#[derive(Serialize)]
struct ManagedRequest<'a> {
    version: u32,
    op: &'static str,
    authority: &'a ClientAuthority,
    request: &'a Operation,
}

struct LeaseState {
    revoked: bool,
    next_request: u64,
    requests: HashMap<u64, UnixStream>,
}

struct Lease {
    authority: Option<ClientAuthority>,
    state: Mutex<LeaseState>,
}

impl Lease {
    fn revoke(&self) {
        let mut state = self.state.lock().unwrap();
        state.revoked = true;
        // Wake outstanding polls and writes. Their outcome remains unknown;
        // work that has not entered dispatch is rejected without sending bytes.
        for stream in state.requests.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Immutable destination and launch-local authority. Clones share revocation;
/// they do not keep the dedicated attachment lifetime connection alive.
#[derive(Clone)]
pub struct Endpoint {
    socket: Arc<PathBuf>,
    lease: Arc<Lease>,
}

impl Endpoint {
    fn new(socket: &Path, authority: Option<ClientAuthority>) -> Self {
        Self {
            socket: Arc::new(socket.to_owned()),
            lease: Arc::new(Lease {
                authority,
                state: Mutex::new(LeaseState {
                    revoked: false,
                    next_request: 0,
                    requests: HashMap::new(),
                }),
            }),
        }
    }

    /// Explicitly unbound headless/admin access, never inferred client access.
    /// The server rejects this endpoint's use of managed-client-owned views.
    pub fn admin(socket: &Path) -> Self {
        Self::new(socket, None)
    }

    #[cfg(test)]
    pub(crate) fn unmanaged(socket: &Path) -> Self {
        Self::admin(socket)
    }

    /// Local authoritative lifecycle state, shared by every endpoint clone.
    /// This does not turn an earlier unrelated request failure into cancellation.
    pub fn is_revoked(&self) -> bool {
        self.lease.state.lock().unwrap().revoked
    }

    pub fn ensure_live(&self) -> Result<()> {
        if self.is_revoked() {
            return Err(DetachedBeforeDispatch.into());
        }
        Ok(())
    }

    /// Dispatch exactly once; never replay a request after disconnect/takeover.
    pub fn checked(&self, operation: Operation) -> Result<Value> {
        ensure!(
            !matches!(
                &operation,
                Operation::Client { .. } | Operation::ClientRegister { .. }
            ),
            "client authority cannot be nested or registered through an endpoint"
        );
        self.ensure_live()?;
        let mut stream = UnixStream::connect(self.socket.as_path())
            .with_context(|| format!("connecting to {}", self.socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let cancellation = stream.try_clone()?;
        let id = {
            let mut state = self.lease.state.lock().unwrap();
            if state.revoked {
                return Err(DetachedBeforeDispatch.into());
            }
            let id = state.next_request;
            state.next_request = state.next_request.wrapping_add(1);
            state.requests.insert(id, cancellation);
            id
        };
        let _inflight = Inflight {
            lease: &self.lease,
            id,
        };
        let response = match &self.lease.authority {
            Some(authority) => request_stream(
                &mut stream,
                &ManagedRequest {
                    version: PROTOCOL_VERSION,
                    op: "client",
                    authority,
                    request: &operation,
                },
            )?,
            None => request_stream(&mut stream, &Request::new(operation))?,
        };
        checked_response(response)
    }
}

struct Inflight<'a> {
    lease: &'a Lease,
    id: u64,
}
impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.lease.state.lock().unwrap().requests.remove(&self.id);
    }
}

#[derive(Deserialize)]
struct SessionReply {
    session: SessionInfo,
}
#[derive(Deserialize)]
struct RegistrationReply {
    client: ClientAuthority,
    session: SessionInfo,
}

fn capture_agent_socket(value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    Ok(Some(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("reading client invocation directory for SSH agent source")?
            .join(path)
    }))
}

/// Owns the sole lifetime stream. Dropping this object revokes every endpoint
/// clone and closes the stream, even when workers still retain endpoints.
pub struct RegisteredClient {
    endpoint: Endpoint,
    session: SessionInfo,
    lifetime: Option<UnixStream>,
    pending: Vec<u8>,
    activation_attempted: bool,
    agent_socket: Option<PathBuf>,
}

impl RegisteredClient {
    pub fn register(socket: &Path) -> Result<Self> {
        let agent_socket = capture_agent_socket(std::env::var_os("SSH_AUTH_SOCK"))?;
        Self::register_with_agent_socket(socket, agent_socket)
    }

    fn register_with_agent_socket(socket: &Path, agent_socket: Option<PathBuf>) -> Result<Self> {
        let session: SessionReply =
            serde_json::from_value(super::checked(socket, Operation::SessionInfo)?)
                .context("decoding server session identity")?;
        bkpr_protocol::valid_session_name(&session.session.name)?;
        ensure!(
            !session.session.session_id.is_empty() && !session.session.daemon_id.is_empty(),
            "server omitted session or daemon identity"
        );
        let mut lifetime = UnixStream::connect(socket)
            .with_context(|| format!("connecting to {}", socket.display()))?;
        lifetime.set_read_timeout(Some(Duration::from_secs(15)))?;
        lifetime.set_write_timeout(Some(Duration::from_secs(5)))?;
        let request = Request::new(Operation::ClientRegister {
            session_id: session.session.session_id.clone(),
            daemon_id: session.session.daemon_id.clone(),
        });
        {
            let mut writer = std::io::BufWriter::new(&mut lifetime);
            serde_json::to_writer(&mut writer, &request)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        // Read exactly the ACK: a coalesced takeover event must remain on the
        // socket so the frontend's lifecycle descriptor is still readable.
        let line = registration_line(&mut lifetime)?;
        let response: Response =
            serde_json::from_slice(&line).context("decoding client registration response")?;
        let registered: RegistrationReply =
            serde_json::from_value(checked_response(validate_response(response)?)?)
                .context("decoding registered client identity")?;
        ensure!(
            registered.session == session.session,
            "server session or daemon changed during registration"
        );
        ensure!(
            !registered.client.client_id.is_empty() && !registered.client.token.is_empty(),
            "server omitted client authority"
        );
        lifetime.set_read_timeout(None)?;
        lifetime.set_write_timeout(None)?;
        lifetime.set_nonblocking(true)?;
        Ok(Self {
            endpoint: Endpoint::new(socket, Some(registered.client)),
            session: registered.session,
            lifetime: Some(lifetime),
            pending: Vec::new(),
            activation_attempted: false,
            agent_socket,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn session(&self) -> &SessionInfo {
        &self.session
    }
    pub fn lifetime_fd(&self) -> RawFd {
        self.lifetime.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    /// Must be called only after frontend initialization, before attaching views.
    /// Even an ambiguous failure consumes the attempt: takeover is never replayed.
    pub fn activate_once(&mut self, detach_others: bool) -> Result<()> {
        ensure!(
            !self.activation_attempted,
            "client activation already attempted; do not replay"
        );
        self.activation_attempted = true;
        ensure!(
            !self.consume_lifecycle()?,
            "client detached before activation"
        );
        let value = self.endpoint.checked(Operation::ClientActivate {
            detach_others,
            agent_socket: self.agent_socket.take(),
        })?;
        ensure!(
            value.get("activated").and_then(Value::as_bool) == Some(true)
                && value
                    .get("already_active")
                    .and_then(Value::as_bool)
                    .is_some(),
            "server did not confirm client activation; do not replay"
        );
        Ok(())
    }

    /// Bounded, nonblocking lifecycle consumption. EOF is authoritative detach,
    /// including EOF without an event. Malformed events fail closed as well.
    pub fn consume_lifecycle(&mut self) -> Result<bool> {
        if self.lifetime.is_none() {
            return Ok(true);
        }
        let result = self.read_lifecycle();
        match result {
            Ok(false) => Ok(false),
            Ok(true) => {
                self.close();
                Ok(true)
            }
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }

    fn read_lifecycle(&mut self) -> Result<bool> {
        let stream = self.lifetime.as_mut().unwrap();
        let mut bytes = [0; 1024];
        for _ in 0..LIFECYCLE_READ_BUDGET / bytes.len() {
            match stream.read(&mut bytes) {
                Ok(0) => return Ok(true),
                Ok(count) => {
                    let end = bytes[..count].iter().position(|byte| *byte == b'\n');
                    let length = end.map_or(count, |index| index + 1);
                    ensure!(
                        self.pending.len() + length <= MAX_LIFETIME_LINE,
                        "client lifecycle event exceeds limit"
                    );
                    self.pending.extend_from_slice(&bytes[..length]);
                    if end.is_some() {
                        let event: ClientEvent = serde_json::from_slice(&self.pending)
                            .context("decoding client lifecycle event")?;
                        ensure!(
                            event.event == "client_detached",
                            "unsupported client lifecycle event"
                        );
                        return Ok(true);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("reading client lifetime"),
            }
        }
        Ok(false)
    }

    pub fn detach(&mut self) -> Result<()> {
        if self.lifetime.is_none() {
            return Ok(());
        }
        let result = self.endpoint.checked(Operation::ClientDetach).map(|_| ());
        self.close();
        result
    }

    fn close(&mut self) {
        self.endpoint.lease.revoke();
        if let Some(stream) = self.lifetime.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for RegisteredClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn registration_line(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut bytes = [0u8; 1024];
    loop {
        // Peek without consuming future lifecycle events. The descriptor is
        // exclusively owned here and remains valid for the entire recv call.
        let count = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                libc::MSG_PEEK,
            )
        };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("reading client registration response");
        }
        ensure!(
            count > 0,
            "server closed client registration before newline"
        );
        let count = count as usize;
        let end = bytes[..count].iter().position(|byte| *byte == b'\n');
        let length = end.map_or(count, |index| index + 1);
        ensure!(
            line.len() + length <= MAX_LIFETIME_LINE,
            "client registration response exceeds limit"
        );
        stream
            .read_exact(&mut bytes[..length])
            .context("reading client registration response")?;
        line.extend_from_slice(&bytes[..length]);
        if end.is_some() {
            return Ok(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        sync::mpsc,
        thread,
    };

    #[test]
    fn source_capture_resolves_relative_raw_paths_and_preserves_intentional_absence() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(capture_agent_socket(None).unwrap(), None);
        assert_eq!(capture_agent_socket(Some(OsString::new())).unwrap(), None);
        let relative = OsString::from_vec(b"agent-\xff.sock".to_vec());
        let captured = capture_agent_socket(Some(relative.clone())).unwrap();
        assert_eq!(
            captured,
            Some(std::env::current_dir().unwrap().join(relative))
        );
        let absolute = OsString::from_vec(b"/private/agent-\xfe.sock".to_vec());
        assert_eq!(
            capture_agent_socket(Some(absolute.clone())).unwrap(),
            Some(PathBuf::from(absolute))
        );
    }

    fn session() -> SessionInfo {
        SessionInfo {
            name: "../exact session 🐈".into(),
            session_id: "persistent-session".into(),
            daemon_id: "daemon-incarnation".into(),
        }
    }

    fn response(result: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&Response {
            version: bkpr_protocol::PROTOCOL_VERSION,
            ok: true,
            result: Some(result),
            error: None,
        })
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn registration(session: SessionInfo) -> Value {
        serde_json::json!({
            "client": {"client_id": "attachment", "token": "test-only-secret"},
            "session": session,
        })
    }

    fn read_request(stream: &mut UnixStream) -> Request {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut line = Vec::new();
        BufReader::new(stream).read_until(b'\n', &mut line).unwrap();
        serde_json::from_slice(&line).unwrap()
    }

    fn fixture(
        run: impl FnOnce(UnixStream, UnixListener) + Send + 'static,
    ) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<()>) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("client.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            {
                let (mut discovery, _) = listener.accept().unwrap();
                assert!(matches!(
                    read_request(&mut discovery).operation,
                    Operation::SessionInfo
                ));
                discovery
                    .write_all(&response(serde_json::json!({"session": session()})))
                    .unwrap();
            }
            let (mut lifetime, _) = listener.accept().unwrap();
            let Operation::ClientRegister {
                session_id,
                daemon_id,
            } = read_request(&mut lifetime).operation
            else {
                panic!("expected dedicated client registration");
            };
            assert_eq!(session_id, session().session_id);
            assert_eq!(daemon_id, session().daemon_id);
            run(lifetime, listener);
        });
        (directory, socket, server)
    }

    #[test]
    fn registration_preserves_coalesced_takeover_and_revokes_endpoint_clones() {
        let (finish, finished) = mpsc::channel();
        let (_directory, socket, server) = fixture(move |mut lifetime, _| {
            let mut bytes = response(registration(session()));
            bytes.extend_from_slice(b"{\"event\":\"client_detached\",\"reason\":\"takeover\"}\n");
            lifetime.write_all(&bytes).unwrap();
            // Keep the descriptor open: EOF must not conceal an ACK reader
            // accidentally swallowing the coalesced lifecycle event.
            finished.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let mut client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
        let endpoint = client.endpoint().clone();
        assert_eq!(client.session(), &session());
        assert!(client.consume_lifecycle().unwrap());
        assert!(endpoint.checked(Operation::Inspect).is_err());
        assert!(client.activate_once(true).is_err());
        finish.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn registration_rejects_changed_daemon_without_activation() {
        let (_directory, socket, server) = fixture(|mut lifetime, listener| {
            let mut changed = session();
            changed.daemon_id = "replacement-daemon".into();
            lifetime
                .write_all(&response(registration(changed)))
                .unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        assert!(RegisteredClient::register_with_agent_socket(&socket, None).is_err());
        server.join().unwrap();
    }

    #[test]
    fn activation_publishes_source_or_absence_once_under_authority() {
        use std::os::unix::ffi::OsStringExt;
        let raw_path = PathBuf::from(OsString::from_vec(b"/private/agent-\xff.sock".to_vec()));
        for source in [None, Some(raw_path)] {
            let expected_source = source.clone();
            let (finish, finished) = mpsc::channel();
            let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
                lifetime
                    .write_all(&response(registration(session())))
                    .unwrap();
                let (mut control, _) = listener.accept().unwrap();
                let Operation::Client { authority, request } = read_request(&mut control).operation
                else {
                    panic!("managed activation must carry explicit authority");
                };
                assert!(
                    authority.client_id == "attachment" && authority.token == "test-only-secret"
                );
                let Operation::ClientActivate {
                    detach_others,
                    agent_socket,
                } = *request
                else {
                    panic!("expected activation");
                };
                assert!(detach_others);
                assert_eq!(agent_socket, expected_source);
                control
                    .write_all(&response(
                        serde_json::json!({"activated":true,"already_active":false}),
                    ))
                    .unwrap();
                finished.recv_timeout(Duration::from_secs(3)).unwrap();
                listener.set_nonblocking(true).unwrap();
                assert!(
                    matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
                );
            });
            let mut client = RegisteredClient::register_with_agent_socket(&socket, source).unwrap();
            assert!(!client.consume_lifecycle().unwrap());
            client.activate_once(true).unwrap();
            assert!(client.activate_once(true).is_err());
            finish.send(()).unwrap();
            server.join().unwrap();
        }
    }

    #[test]
    fn ambiguous_activation_failure_is_not_replayed() {
        let (finish, finished) = mpsc::channel();
        let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let (mut control, _) = listener.accept().unwrap();
            let Operation::Client { request, .. } = read_request(&mut control).operation else {
                panic!("managed activation must carry explicit authority");
            };
            assert!(matches!(*request, Operation::ClientActivate { .. }));
            drop(control);
            finished.recv_timeout(Duration::from_secs(3)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        let mut client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
        let error = client.activate_once(true).unwrap_err();
        assert!(super::super::outcome_unknown(&error));
        assert!(client.activate_once(true).is_err());
        finish.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn lifetime_eof_cancels_inflight_request_and_blocks_later_dispatch() {
        let (accepted, observed) = mpsc::channel();
        let (finish, finished) = mpsc::channel();
        let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let (mut control, _) = listener.accept().unwrap();
            assert!(matches!(
                read_request(&mut control).operation,
                Operation::Client { .. }
            ));
            drop(lifetime);
            accepted.send(()).unwrap();
            // Cancellation closes the request connection, rather than waiting
            // for the normal 15-second transport response deadline.
            let mut byte = [0];
            assert_eq!(control.read(&mut byte).unwrap(), 0);
            finished.recv_timeout(Duration::from_secs(3)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        let mut client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
        let endpoint = client.endpoint().clone();
        let worker_endpoint = endpoint.clone();
        let worker = thread::spawn(move || worker_endpoint.checked(Operation::Inspect));
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(client.consume_lifecycle().unwrap());
        let error = worker.join().unwrap().unwrap_err();
        assert!(super::super::outcome_unknown(&error));
        assert!(!detached_before_dispatch(&error));
        let error = endpoint
            .checked(Operation::Inspect)
            .unwrap_err()
            .context("queued operation");
        assert!(!super::super::outcome_unknown(&error));
        assert!(detached_before_dispatch(&error));
        finish.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn dropping_lifetime_closes_attachment_despite_retained_endpoint() {
        let (_directory, socket, server) = fixture(|mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        let client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
        let endpoint = client.endpoint().clone();
        drop(client);
        assert!(endpoint.checked(Operation::Inspect).is_err());
        server.join().unwrap();
    }
}
