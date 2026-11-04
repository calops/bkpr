use super::{
    checked_payload_response, checked_response, request_client_stream, request_stream,
    validate_response,
};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{
    ClientAuthority, Operation, PaneSnapshot, Payload, Request, SessionInfo, scene_wire,
    wire::{self, Codec, Kind},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsString,
    io::Read,
    net::Shutdown,
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_LIFETIME_PAYLOAD: usize = 64 * 1024;
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

struct LeaseState {
    revoked: bool,
    session_ended: bool,
    next_request: u64,
    requests: HashMap<u64, ActiveRequest>,
}

struct ActiveRequest {
    stream: UnixStream,
    observational: bool,
}

fn observational(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Inspect
            | Operation::SessionInfo
            | Operation::Wait { .. }
            | Operation::RecoveryList
            | Operation::Pane {
                request: bkpr_protocol::PaneRequest::Inspect
                    | bkpr_protocol::PaneRequest::Poll { .. },
                ..
            }
    )
}

struct Lease {
    authority: Option<ClientAuthority>,
    state: Mutex<LeaseState>,
}

impl Lease {
    fn revoke(&self) {
        let mut state = self.state.lock().unwrap();
        state.revoked = true;
        // Observations are cancellable; accepted mutations must drain their
        // actual reply rather than manufacturing an unknown outcome.
        for request in state
            .requests
            .values()
            .filter(|request| request.observational)
        {
            let _ = request.stream.shutdown(Shutdown::Both);
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
                    session_ended: false,
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

    pub(crate) fn session_ended(&self) -> bool {
        self.lease.state.lock().unwrap().session_ended
    }

    pub fn ensure_live(&self) -> Result<()> {
        if self.is_revoked() {
            return Err(DetachedBeforeDispatch.into());
        }
        Ok(())
    }

    /// Dispatch exactly once; never replay a request after disconnect/takeover.
    pub fn checked(&self, operation: Operation) -> Result<Value> {
        self.checked_payload(operation)?.into_json()
    }

    /// Dispatch without converting binary pane scenes into diagnostic JSON.
    pub fn checked_payload(&self, operation: Operation) -> Result<Payload> {
        let is_observation = observational(&operation);
        let result = (|| -> Result<Payload> {
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
                state.requests.insert(
                    id,
                    ActiveRequest {
                        stream: cancellation,
                        observational: is_observation,
                    },
                );
                id
            };
            let _inflight = Inflight {
                lease: &self.lease,
                id,
            };
            let response = match &self.lease.authority {
                Some(authority) => request_client_stream(&mut stream, authority, &operation)?,
                None => request_stream(&mut stream, &Request::new(operation))?,
            };
            checked_payload_response(response)
        })();
        if is_observation {
            result.map_err(super::observation_error)
        } else {
            result
        }
    }

    /// Decode owned scene bytes directly, before the poller advances its cursors.
    pub fn checked_scene(
        &self,
        operation: Operation,
    ) -> Result<(PaneSnapshot, Option<scene_wire::DecodedScene>)> {
        let Payload::Pane(payload) = self.checked_payload(operation)? else {
            bail!("pane response omitted native scene payload");
        };
        let metadata: PaneSnapshot =
            serde_json::from_value(payload.metadata).context("decoding pane snapshot metadata")?;
        ensure!(
            metadata.frame.is_none(),
            "native pane metadata must not contain a JSON frame"
        );
        let scene = payload
            .scene
            .as_deref()
            .map(scene_wire::decode)
            .transpose()
            .context("decoding native pane scene")?;
        if let Some(scene) = &scene {
            ensure!(
                scene.revision == metadata.revision,
                "native pane scene revision does not match snapshot metadata"
            );
        }
        Ok((metadata, scene))
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
        let mut codec = Codec::new();
        wire::write_frame(
            &mut lifetime,
            Kind::Request,
            codec.encode_request(&request)?,
        )?;
        // Read exactly the ACK: a coalesced takeover event must remain on the
        // socket so the frontend's lifecycle descriptor is still readable.
        let bytes = wire::read_frame(&mut lifetime, Kind::Response, MAX_LIFETIME_PAYLOAD)?;
        let response =
            wire::decode_response_owned(bytes).context("decoding client registration response")?;
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

    pub(crate) fn close_for_shutdown(&mut self) {
        self.endpoint.lease.state.lock().unwrap().session_ended = true;
        self.close();
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
        let mut budget = LIFECYCLE_READ_BUDGET;
        let mut reads = LIFECYCLE_READ_BUDGET / bytes.len();
        loop {
            let target = if self.pending.len() < wire::HEADER_LEN {
                wire::HEADER_LEN
            } else {
                wire::HEADER_LEN
                    + wire::frame_payload_len(
                        &self.pending[..wire::HEADER_LEN],
                        Kind::ClientEvent,
                        MAX_LIFETIME_PAYLOAD,
                    )?
            };
            if self.pending.len() == target {
                let event = wire::decode_client_event(&self.pending[wire::HEADER_LEN..])
                    .context("decoding client lifecycle event")?;
                ensure!(
                    event.event == "client_detached",
                    "unsupported client lifecycle event"
                );
                if event.reason == "shutdown" {
                    self.endpoint.lease.state.lock().unwrap().session_ended = true;
                }
                return Ok(true);
            }
            if budget == 0 || reads == 0 {
                return Ok(false);
            }
            let length = (target - self.pending.len()).min(bytes.len()).min(budget);
            reads -= 1;
            match stream.read(&mut bytes[..length]) {
                Ok(0) => return Ok(true),
                Ok(count) => {
                    self.pending.extend_from_slice(&bytes[..count]);
                    budget -= count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("reading client lifetime"),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{ClientEvent, PROTOCOL_VERSION, Response};
    use std::{io::Write, os::unix::net::UnixListener, sync::mpsc, thread};

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

    fn reply(response: Response) -> Vec<u8> {
        let mut codec = Codec::new();
        wire::encode_frame(Kind::Response, codec.encode_response(&response).unwrap()).unwrap()
    }

    fn response(result: Value) -> Vec<u8> {
        reply(Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(result.into()),
            error: None,
            failure: None,
        })
    }

    fn detached(reason: &str) -> Vec<u8> {
        let mut codec = Codec::new();
        wire::encode_frame(
            Kind::ClientEvent,
            codec
                .encode_client_event(&ClientEvent {
                    event: "client_detached".into(),
                    reason: reason.into(),
                })
                .unwrap(),
        )
        .unwrap()
    }

    fn lifecycle_pair() -> (RegisteredClient, UnixStream) {
        let (lifetime, server) = UnixStream::pair().unwrap();
        lifetime.set_nonblocking(true).unwrap();
        (
            RegisteredClient {
                endpoint: Endpoint::unmanaged(Path::new("unused.sock")),
                session: session(),
                lifetime: Some(lifetime),
                pending: Vec::new(),
                activation_attempted: false,
                agent_socket: None,
            },
            server,
        )
    }

    #[test]
    fn fragmented_lifecycle_header_and_payload_preserve_authoritative_shutdown() {
        let (mut client, mut server) = lifecycle_pair();
        let bytes = detached("shutdown");
        for fragment in bytes[..bytes.len() - 1].chunks(3) {
            server.write_all(fragment).unwrap();
            assert!(!client.consume_lifecycle().unwrap());
            assert!(!client.endpoint().is_revoked());
        }
        server.write_all(&bytes[bytes.len() - 1..]).unwrap();
        assert!(client.consume_lifecycle().unwrap());
        assert!(client.endpoint().is_revoked());
        assert!(client.endpoint().session_ended());
    }

    #[test]
    fn invalid_lifecycle_frames_fail_closed_without_claiming_shutdown() {
        let mut codec = Codec::new();
        let oversized = ClientEvent {
            event: "client_detached".into(),
            reason: "x".repeat(MAX_LIFETIME_PAYLOAD),
        };
        let cases = [
            wire::encode_frame(Kind::Response, &[0]).unwrap(),
            wire::encode_frame(Kind::ClientEvent, &[0]).unwrap(),
            wire::encode_frame(
                Kind::ClientEvent,
                codec.encode_client_event(&oversized).unwrap(),
            )
            .unwrap()[..wire::HEADER_LEN]
                .to_vec(),
        ];
        for bytes in cases {
            let (mut client, mut server) = lifecycle_pair();
            server.write_all(&bytes).unwrap();
            assert!(client.consume_lifecycle().is_err());
            assert!(client.endpoint().is_revoked());
            assert!(!client.endpoint().session_ended());
            assert!(client.consume_lifecycle().unwrap());
        }
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
        let bytes = wire::read_frame(stream, Kind::Request, wire::MAX_REQUEST).unwrap();
        wire::decode_request(&bytes).unwrap()
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
            bytes.extend_from_slice(&detached("takeover"));
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
        assert!(!super::super::expected_shutdown(
            &error,
            endpoint.session_ended()
        ));
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
    #[test]
    fn authoritative_shutdown_cancels_observation_without_claiming_dispatch_rejection() {
        let (accepted, observed) = mpsc::channel();
        let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let (mut control, _) = listener.accept().unwrap();
            assert!(matches!(
                read_request(&mut control).operation,
                Operation::Client { .. }
            ));
            lifetime.write_all(&detached("shutdown")).unwrap();
            accepted.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(control.read(&mut byte).unwrap(), 0);
        });
        let mut client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
        let endpoint = client.endpoint().clone();
        let worker_endpoint = endpoint.clone();
        let worker = thread::spawn(move || worker_endpoint.checked(Operation::Inspect));
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(client.consume_lifecycle().unwrap());
        let error = worker.join().unwrap().unwrap_err();
        assert!(super::super::outcome_unknown(&error));
        assert!(!super::super::shutdown_not_dispatched(&error));
        assert!(super::super::expected_shutdown(
            &error,
            endpoint.session_ended()
        ));
        server.join().unwrap();
    }

    #[test]
    fn accepted_mutation_receipts_survive_authoritative_shutdown() {
        for outcome in ["committed", "failed", "unknown"] {
            let (accepted, observed) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
                lifetime
                    .write_all(&response(registration(session())))
                    .unwrap();
                let (mut control, _) = listener.accept().unwrap();
                assert!(matches!(
                    read_request(&mut control).operation,
                    Operation::Client { .. }
                ));
                lifetime.write_all(&detached("shutdown")).unwrap();
                accepted.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(3)).unwrap();
                let reply = match outcome {
                    "committed" => response(serde_json::json!({"committed_name":"renamed"})),
                    _ => reply(Response {
                        version: PROTOCOL_VERSION,
                        ok: false,
                        result: None,
                        error: Some("durable mutation failed".into()),
                        failure: (outcome == "unknown").then_some(
                            bkpr_protocol::RequestFailure::SessionClosing {
                                outcome: bkpr_protocol::RequestOutcome::Unknown,
                            },
                        ),
                    }),
                };
                control.write_all(&reply).unwrap();
                listener.set_nonblocking(true).unwrap();
                assert!(
                    matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
                );
            });
            let mut client = RegisteredClient::register_with_agent_socket(&socket, None).unwrap();
            let endpoint = client.endpoint().clone();
            let worker_endpoint = endpoint.clone();
            let worker = thread::spawn(move || {
                worker_endpoint.checked(Operation::WorkspaceRename {
                    workspace_id: "owned-workspace".into(),
                    expected_revision: 7,
                    name: "renamed".into(),
                })
            });
            observed.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(client.consume_lifecycle().unwrap());
            release.send(()).unwrap();
            match worker.join().unwrap() {
                Ok(value) => {
                    assert_eq!(outcome, "committed");
                    assert_eq!(value["committed_name"], "renamed");
                }
                Err(error) => {
                    assert_ne!(outcome, "committed");
                    assert_eq!(super::super::outcome_unknown(&error), outcome == "unknown");
                    assert!(!super::super::expected_shutdown(
                        &error,
                        endpoint.session_ended()
                    ));
                }
            }
            server.join().unwrap();
        }
    }
}
