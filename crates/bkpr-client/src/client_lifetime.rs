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

#[derive(Debug)]
pub(crate) struct NativeRetirement {
    reason: String,
}
impl std::fmt::Display for NativeRetirement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "client attachment retired: {}", self.reason)
    }
}
impl std::error::Error for NativeRetirement {}

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
/// Admission classes let remote observations exhaust only their own capacity.
#[derive(Clone, Copy)]
pub(crate) enum ConnectionPurpose {
    Lifetime,
    Control,
    Observation,
}

pub(crate) trait Connector: Send + Sync {
    fn connect(&self, purpose: ConnectionPurpose) -> Result<UnixStream>;
    fn connect_cancellable(
        &self,
        purpose: ConnectionPurpose,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<UnixStream> {
        ensure!(!cancellation.is_cancelled(), "native connection cancelled");
        self.connect(purpose)
    }
    fn remote(&self) -> bool {
        false
    }
}

/// Offline UI state has neither an address nor an administrative backend.
struct UnavailableConnector;
impl Connector for UnavailableConnector {
    fn connect(&self, _purpose: ConnectionPurpose) -> Result<UnixStream> {
        Err(DetachedBeforeDispatch.into())
    }
}

struct LocalConnector(PathBuf);
impl Connector for LocalConnector {
    fn connect(&self, _purpose: ConnectionPurpose) -> Result<UnixStream> {
        UnixStream::connect(&self.0).with_context(|| format!("connecting to {}", self.0.display()))
    }
    fn connect_cancellable(
        &self,
        _purpose: ConnectionPurpose,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<UnixStream> {
        let stream = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => bail!("local connection cancelled"),
                    stream = tokio::net::UnixStream::connect(&self.0) => {
                        Ok(stream?.into_std()?)
                    }
                }
            })?;
        stream.set_nonblocking(false)?;
        Ok(stream)
    }
}

/// Immutable destination and launch-local authority. Clones share revocation;
/// they do not keep the dedicated attachment lifetime connection alive.
#[derive(Clone)]
pub struct Endpoint {
    connector: Arc<dyn Connector>,
    lease: Arc<Lease>,
}

impl Endpoint {
    fn new(socket: &Path, authority: Option<ClientAuthority>) -> Self {
        Self::from_connector(Arc::new(LocalConnector(socket.to_owned())), authority)
    }

    fn from_connector(connector: Arc<dyn Connector>, authority: Option<ClientAuthority>) -> Self {
        Self {
            connector,
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

    /// Explicit no-backend state for an unattached UI. Every clone is revoked
    /// before publication and can never dispatch a native request.
    pub fn unavailable() -> Self {
        let endpoint = Self::from_connector(Arc::new(UnavailableConnector), None);
        endpoint.lease.revoke();
        endpoint
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

    pub(crate) fn is_remote(&self) -> bool {
        self.connector.remote()
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

    /// Worker-only admission observations. Cancellation closes this request,
    /// while the attachment's lease remains the authority for every clone.
    pub fn checked_cancellable(
        &self,
        operation: Operation,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Value> {
        ensure!(
            observational(&operation),
            "cancellable endpoint request must be observational"
        );
        let result = (|| {
            self.ensure_live()?;
            let stream = self
                .connector
                .connect_cancellable(ConnectionPurpose::Observation, cancellation)?;
            let _inflight = self.track_request(&stream, true)?;
            stream.set_nonblocking(true)?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let mut stream = tokio::net::UnixStream::from_std(stream)?;
                    let dispatched = std::cell::Cell::new(false);
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => {
                            let error = anyhow::anyhow!("client observation cancelled");
                            Err(if dispatched.get() { super::mark_outcome_unknown(error) } else { error })
                        },
                        response = async {
                            self.ensure_live()?;
                            dispatched.set(true);
                            match &self.lease.authority {
                                Some(authority) => super::request_client_async(
                                    &mut stream, authority, &operation,
                                ).await,
                                None => super::request_async(
                                    &mut stream, &Request::new(operation), wire::MAX_RESPONSE,
                                ).await,
                            }
                        } => checked_response(response?),
                    }
                })
        })();
        result.map_err(super::observation_error)
    }

    fn track_request(&self, stream: &UnixStream, observational: bool) -> Result<Inflight<'_>> {
        let cancellation = stream.try_clone()?;
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
                observational,
            },
        );
        Ok(Inflight {
            lease: &self.lease,
            id,
        })
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
            ensure!(
                !self.connector.remote()
                    || !matches!(
                        &operation,
                        Operation::HarnessBegin { .. }
                            | Operation::HarnessEvent { .. }
                            | Operation::HarnessContext { .. }
                            | Operation::Agent { .. }
                    ),
                "remote managed endpoint cannot carry native harness or agent authority"
            );
            self.ensure_live()?;
            let purpose = if is_observation {
                ConnectionPurpose::Observation
            } else {
                ConnectionPurpose::Control
            };
            let mut stream = self.connector.connect(purpose)?;
            stream.set_read_timeout(Some(Duration::from_secs(15)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let _inflight = self.track_request(&stream, is_observation)?;
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

/// Native detach reasons distinguish explicit user/session retirement from
/// lifetime transport cleanup. Only the coordinator owns reconnect policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Active,
    /// Explicit retirement, including unrecognized reasons. Preserve the fact
    /// without inferring user intent; policy must not automatically retry it.
    Detached {
        reason: String,
    },
    TransportClosed,
}

/// Owns the sole lifetime stream. Dropping this object revokes every endpoint
/// clone and closes the stream, even when workers still retain endpoints.
pub struct RegisteredClient {
    endpoint: Endpoint,
    session: SessionInfo,
    lifetime: Option<UnixStream>,
    lifecycle: Lifecycle,
    pending: Vec<u8>,
    activation_attempted: bool,
    agent_socket: Option<PathBuf>,
}

impl RegisteredClient {
    pub fn register(socket: &Path) -> Result<Self> {
        let agent_socket = capture_agent_socket(std::env::var_os("SSH_AUTH_SOCK"))?;
        Self::register_with_agent_socket(socket, agent_socket)
    }

    /// Cancellation covers local UDS connection, identity discovery and native
    /// registration. No activation or mutation is retried.
    pub fn register_cancellable(
        socket: &Path,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        let agent_socket = capture_agent_socket(std::env::var_os("SSH_AUTH_SOCK"))?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("creating cancellable local registration runtime")?
            .block_on(async {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => bail!("local attachment registration cancelled"),
                    registered = async {
                        let mut probe = tokio::net::UnixStream::connect(socket).await
                            .with_context(|| format!("connecting to {}", socket.display()))?;
                        let response = super::request_async(
                            &mut probe, &Request::new(Operation::SessionInfo), wire::MAX_RESPONSE,
                        ).await?;
                        let expected: SessionReply = serde_json::from_value(checked_response(response)?)
                            .context("decoding server session identity")?;
                        Self::validate_identity(&expected.session)?;
                        let mut lifetime = tokio::net::UnixStream::connect(socket).await?;
                        let response = super::request_async(
                            &mut lifetime,
                            &Request::new(Operation::ClientRegister {
                                session_id: expected.session.session_id.clone(),
                                daemon_id: expected.session.daemon_id.clone(),
                            }),
                            MAX_LIFETIME_PAYLOAD,
                        ).await?;
                        Self::from_registration(
                            Arc::new(LocalConnector(socket.to_owned())),
                            expected.session, agent_socket, lifetime.into_std()?, response,
                        )
                    } => registered,
                }
            })
    }

    fn register_with_agent_socket(socket: &Path, agent_socket: Option<PathBuf>) -> Result<Self> {
        let session: SessionReply =
            serde_json::from_value(super::checked(socket, Operation::SessionInfo)?)
                .context("decoding server session identity")?;
        Self::register_connector(
            Arc::new(LocalConnector(socket.to_owned())),
            session.session,
            agent_socket,
        )
    }

    /// Register against an already-validated identity. In particular remote
    /// registration must never probe/rebase to a replacement daemon.
    pub(crate) fn register_connector(
        connector: Arc<dyn Connector>,
        expected: SessionInfo,
        agent_socket: Option<PathBuf>,
    ) -> Result<Self> {
        Self::validate_identity(&expected)?;
        let mut lifetime = connector.connect(ConnectionPurpose::Lifetime)?;
        lifetime.set_read_timeout(Some(Duration::from_secs(15)))?;
        lifetime.set_write_timeout(Some(Duration::from_secs(5)))?;
        let request = Request::new(Operation::ClientRegister {
            session_id: expected.session_id.clone(),
            daemon_id: expected.daemon_id.clone(),
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
        Self::from_registration(connector, expected, agent_socket, lifetime, response)
    }

    fn validate_identity(session: &SessionInfo) -> Result<()> {
        bkpr_protocol::valid_session_name(&session.name)?;
        ensure!(
            !session.session_id.is_empty() && !session.daemon_id.is_empty(),
            "server omitted session or daemon identity"
        );
        Ok(())
    }

    fn from_registration(
        connector: Arc<dyn Connector>,
        expected: SessionInfo,
        agent_socket: Option<PathBuf>,
        lifetime: UnixStream,
        response: bkpr_protocol::Response,
    ) -> Result<Self> {
        let registered: RegistrationReply =
            serde_json::from_value(checked_response(validate_response(response)?)?)
                .context("decoding registered client identity")?;
        ensure!(
            registered.session == expected,
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
            endpoint: Endpoint::from_connector(connector, Some(registered.client)),
            session: registered.session,
            lifetime: Some(lifetime),
            lifecycle: Lifecycle::Active,
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
    pub fn info(&self) -> &SessionInfo {
        &self.session
    }
    pub(crate) fn client_id(&self) -> &str {
        &self
            .endpoint
            .lease
            .authority
            .as_ref()
            .expect("registered client retains its native authority")
            .client_id
    }
    pub fn lifetime_fd(&self) -> RawFd {
        self.lifetime.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    fn ensure_activation_live(&mut self) -> Result<()> {
        match self.poll_lifecycle()? {
            Lifecycle::Active => Ok(()),
            Lifecycle::Detached { reason } => Err(NativeRetirement { reason }.into()),
            Lifecycle::TransportClosed => bail!("client transport closed before activation"),
        }
    }

    /// Must be called only after frontend initialization, before attaching views.
    /// Even an ambiguous failure consumes the attempt: takeover is never replayed.
    pub fn activate_once(&mut self, detach_others: bool) -> Result<()> {
        ensure!(
            !self.activation_attempted,
            "client activation already attempted; do not replay"
        );
        self.activation_attempted = true;
        self.ensure_activation_live()?;
        let value = self.endpoint.checked(Operation::ClientActivate {
            detach_others,
            agent_socket: self.agent_socket.take(),
        })?;
        Self::validate_activation(value)
    }

    /// Worker-only activation with cancellation across the native reply wait.
    /// Cancellation after dispatch is an unknown outcome, never replayable.
    pub fn activate_cancellable(
        &mut self,
        detach_others: bool,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        ensure!(
            !self.activation_attempted,
            "client activation already attempted; do not replay"
        );
        self.activation_attempted = true;
        self.ensure_activation_live()?;
        self.endpoint.ensure_live()?;
        let stream = self
            .endpoint
            .connector
            .connect_cancellable(ConnectionPurpose::Control, cancellation)?;
        stream.set_nonblocking(true)?;
        let authority = self
            .endpoint
            .lease
            .authority
            .as_ref()
            .expect("registered client retains its native authority");
        let operation = Operation::ClientActivate {
            detach_others,
            agent_socket: self.agent_socket.take(),
        };
        let value = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut stream = tokio::net::UnixStream::from_std(stream)?;
                let dispatched = std::cell::Cell::new(false);
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        let error = anyhow::anyhow!("client activation cancelled");
                        Err(if dispatched.get() { super::mark_outcome_unknown(error) } else { error })
                    },
                    response = async {
                        dispatched.set(true);
                        super::request_client_async(&mut stream, authority, &operation).await
                    } => checked_response(response?),
                }
            })?;
        Self::validate_activation(value)
    }

    fn validate_activation(value: Value) -> Result<()> {
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
        self.lifecycle = Lifecycle::Detached {
            reason: "shutdown".into(),
        };
        self.close();
    }

    /// Revoke authority and release the lifetime without a blocking detach RPC.
    /// Accepted mutations retain their original outcome; callers never replay.
    pub fn cancel(&mut self) {
        if self.lifetime.is_some() {
            self.lifecycle = Lifecycle::Detached {
                reason: "cancelled".into(),
            };
        }
        self.close();
    }

    pub fn poll_lifecycle(&mut self) -> Result<Lifecycle> {
        if self.lifetime.is_none() {
            return Ok(self.lifecycle.clone());
        }
        let result = self.read_lifecycle();
        if !matches!(result, Ok(Lifecycle::Active)) {
            self.lifecycle = result
                .as_ref()
                .cloned()
                .unwrap_or(Lifecycle::TransportClosed);
            self.close();
        }
        result
    }

    fn read_lifecycle(&mut self) -> Result<Lifecycle> {
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
                return match event.reason.as_str() {
                    "shutdown" => {
                        self.endpoint.lease.state.lock().unwrap().session_ended = true;
                        Ok(Lifecycle::Detached {
                            reason: event.reason,
                        })
                    }
                    "disconnected" | "owner_exited" => Ok(Lifecycle::TransportClosed),
                    _ => Ok(Lifecycle::Detached {
                        reason: event.reason,
                    }),
                };
            }
            if budget == 0 || reads == 0 {
                return Ok(Lifecycle::Active);
            }
            let length = (target - self.pending.len()).min(bytes.len()).min(budget);
            reads -= 1;
            match stream.read(&mut bytes[..length]) {
                Ok(0) => return Ok(Lifecycle::TransportClosed),
                Ok(count) => {
                    self.pending.extend_from_slice(&bytes[..count]);
                    budget -= count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(Lifecycle::Active);
                }
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
        self.lifecycle = Lifecycle::Detached {
            reason: "detach".into(),
        };
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
                lifecycle: Lifecycle::Active,
                pending: Vec::new(),
                activation_attempted: false,
                agent_socket: None,
            },
            server,
        )
    }

    #[test]
    fn authoritative_retirement_before_activation_is_not_a_reconnect_failure() {
        for (reason, retired) in [
            ("takeover", true),
            ("shutdown", true),
            ("owner_exited", false),
            ("disconnected", false),
        ] {
            for cancellable in [false, true] {
                let (mut client, mut server) = lifecycle_pair();
                server.write_all(&detached(reason)).unwrap();
                let error = if cancellable {
                    client.activate_cancellable(false, &tokio_util::sync::CancellationToken::new())
                } else {
                    client.activate_once(false)
                }
                .unwrap_err();
                assert_eq!(
                    error.is::<NativeRetirement>(),
                    retired,
                    "{reason}; cancellable={cancellable}"
                );
                assert!(client.endpoint().is_revoked());
            }
        }
    }

    #[test]
    fn fragmented_lifecycle_header_and_payload_preserve_authoritative_shutdown() {
        let (mut client, mut server) = lifecycle_pair();
        let bytes = detached("shutdown");
        for fragment in bytes[..bytes.len() - 1].chunks(3) {
            server.write_all(fragment).unwrap();
            assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::Active);
            assert!(!client.endpoint().is_revoked());
        }
        server.write_all(&bytes[bytes.len() - 1..]).unwrap();
        assert_eq!(
            client.poll_lifecycle().unwrap(),
            Lifecycle::Detached {
                reason: "shutdown".into()
            }
        );
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
            assert!(client.poll_lifecycle().is_err());
            assert!(client.endpoint().is_revoked());
            assert!(!client.endpoint().session_ended());
            assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
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
        assert_eq!(
            client.poll_lifecycle().unwrap(),
            Lifecycle::Detached {
                reason: "takeover".into()
            }
        );
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
    fn pinned_connector_registers_exact_welcome_identity_without_reprobing() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pinned.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut lifetime, _) = listener.accept().unwrap();
            let Operation::ClientRegister {
                session_id,
                daemon_id,
            } = read_request(&mut lifetime).operation
            else {
                panic!("pinned registration must not re-probe SessionInfo");
            };
            assert_eq!(session_id, session().session_id);
            assert_eq!(daemon_id, session().daemon_id);
            let mut replacement = session();
            replacement.daemon_id = "replacement-daemon".into();
            lifetime
                .write_all(&response(registration(replacement)))
                .unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        assert!(
            RegisteredClient::register_connector(Arc::new(LocalConnector(socket)), session(), None)
                .is_err()
        );
        server.join().unwrap();
    }

    #[test]
    fn remote_endpoint_rejects_harness_and_agent_envelopes_before_opening_stream() {
        struct NoAuthority;
        impl Connector for NoAuthority {
            fn remote(&self) -> bool {
                true
            }
            fn connect(&self, _: ConnectionPurpose) -> Result<UnixStream> {
                panic!("authority must be rejected before connecting");
            }
        }
        let endpoint = Endpoint::from_connector(Arc::new(NoAuthority), None);
        for operation in [
            Operation::HarnessBegin {
                kind: bkpr_protocol::HarnessKind::Omp,
            },
            Operation::HarnessContext {
                launch_id: "copied".into(),
                token: "copied".into(),
            },
            Operation::Agent {
                launch_id: "copied".into(),
                token: "copied".into(),
                request: bkpr_protocol::AgentRequest::Whoami,
            },
        ] {
            assert!(endpoint.checked(operation).is_err());
        }
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
            assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::Active);
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
        assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
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
        assert_eq!(
            client.poll_lifecycle().unwrap(),
            Lifecycle::Detached {
                reason: "shutdown".into()
            }
        );
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
            assert_eq!(
                client.poll_lifecycle().unwrap(),
                Lifecycle::Detached {
                    reason: "shutdown".into()
                }
            );
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
    #[test]
    fn lifecycle_events_preserve_intentional_reason_while_eof_is_transport_loss() {
        for reason in ["takeover", "shutdown", "detached"] {
            let (mut client, mut server) = lifecycle_pair();
            server.write_all(&detached(reason)).unwrap();
            drop(server);
            let expected = Lifecycle::Detached {
                reason: reason.into(),
            };
            assert_eq!(client.poll_lifecycle().unwrap(), expected);
            assert_eq!(client.poll_lifecycle().unwrap(), expected);
            assert!(client.endpoint().is_revoked());
        }
        let (mut client, server) = lifecycle_pair();
        drop(server);
        assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
        assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
        assert!(!client.endpoint().session_ended());
    }

    #[test]
    fn buffered_native_transport_cleanup_events_are_recoverable_not_user_detach() {
        for reason in ["disconnected", "owner_exited"] {
            let (mut client, mut server) = lifecycle_pair();
            server.write_all(&detached(reason)).unwrap();
            // The event is already buffered while this endpoint is still open:
            // classification cannot depend on local timeout/EOF winning a race.
            assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
            assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::TransportClosed);
            assert!(client.endpoint().is_revoked());
            assert!(!client.endpoint().session_ended());
        }
    }

    #[test]
    fn unknown_native_detach_reason_preserves_explicit_retirement_without_inventing_intent() {
        let (mut client, mut server) = lifecycle_pair();
        server
            .write_all(&detached("unknown-future-reason"))
            .unwrap();
        assert_eq!(
            client.poll_lifecycle().unwrap(),
            Lifecycle::Detached {
                reason: "unknown-future-reason".into(),
            }
        );
        assert_eq!(
            client.poll_lifecycle().unwrap(),
            Lifecycle::Detached {
                reason: "unknown-future-reason".into(),
            }
        );
        assert!(client.endpoint().is_revoked());
        assert!(!client.endpoint().session_ended());
    }

    #[test]
    fn cancellation_interrupts_registration_read_without_waiting_for_native_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (accepted, accepted_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut probe, _) = listener.accept().unwrap();
            assert!(matches!(
                read_request(&mut probe).operation,
                Operation::SessionInfo
            ));
            probe
                .write_all(&response(serde_json::json!({"session": session()})))
                .unwrap();
            let (mut lifetime, _) = listener.accept().unwrap();
            assert!(matches!(
                read_request(&mut lifetime).operation,
                Operation::ClientRegister { .. }
            ));
            accepted.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
        });
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (completed, completed_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = RegisteredClient::register_cancellable(&socket, &worker_cancellation);
            completed.send(result.map(|_| ())).unwrap();
        });
        accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancellation.cancel();
        let error = completed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("registration cancelled"));
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn cancellable_registration_rejects_replacement_daemon_authority() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut probe, _) = listener.accept().unwrap();
            assert!(matches!(
                read_request(&mut probe).operation,
                Operation::SessionInfo
            ));
            probe
                .write_all(&response(serde_json::json!({"session": session()})))
                .unwrap();
            let (mut lifetime, _) = listener.accept().unwrap();
            let request = read_request(&mut lifetime);
            assert!(
                matches!(request.operation, Operation::ClientRegister { daemon_id, .. }
                if daemon_id == session().daemon_id)
            );
            let mut replacement = session();
            replacement.daemon_id = "replacement-daemon".into();
            lifetime
                .write_all(&response(registration(replacement)))
                .unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
        });
        let error = RegisteredClient::register_cancellable(
            &socket,
            &tokio_util::sync::CancellationToken::new(),
        )
        .err()
        .expect("replacement identity must fail admission");
        assert!(format!("{error:#}").contains("session or daemon changed"));
        server.join().unwrap();
    }

    #[test]
    fn cancelled_activation_keeps_unknown_outcome_and_cannot_be_replayed() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (accepted, accepted_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut probe, _) = listener.accept().unwrap();
            read_request(&mut probe);
            probe
                .write_all(&response(serde_json::json!({"session": session()})))
                .unwrap();
            let (mut lifetime, _) = listener.accept().unwrap();
            read_request(&mut lifetime);
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let (mut activation, _) = listener.accept().unwrap();
            let request = read_request(&mut activation);
            assert!(
                matches!(request.operation, Operation::Client { request, .. }
                if matches!(*request, Operation::ClientActivate { detach_others: true, .. }))
            );
            accepted.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(activation.read(&mut byte).unwrap(), 0);
        });
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (completed, completed_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut client =
                RegisteredClient::register_cancellable(&socket, &worker_cancellation).unwrap();
            let error = client
                .activate_cancellable(true, &worker_cancellation)
                .unwrap_err();
            assert!(super::super::outcome_unknown(&error));
            assert!(client.activate_once(true).is_err());
            completed.send(()).unwrap();
        });
        accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancellation.cancel();
        completed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn abandoned_late_registration_completion_releases_lifetime_without_activation() {
        let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let mut byte = [0];
            assert_eq!(lifetime.read(&mut byte).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock));
        });
        let (completed, abandoned) = mpsc::sync_channel(1);
        drop(abandoned);
        let worker = thread::spawn(move || {
            let client = RegisteredClient::register_cancellable(
                &socket,
                &tokio_util::sync::CancellationToken::new(),
            )
            .unwrap();
            // A completion for a retired attempt no longer has a receiver.
            // Dropping its sole owner must revoke registration, not activate.
            assert!(completed.send(client).is_err());
        });
        worker.join().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn admission_observation_cancellation_closes_only_its_request_without_replay() {
        let (accepted, accepted_rx) = mpsc::sync_channel(1);
        let (finish, finish_rx) = mpsc::sync_channel(1);
        let (_directory, socket, server) = fixture(move |mut lifetime, listener| {
            lifetime
                .write_all(&response(registration(session())))
                .unwrap();
            let (mut observation, _) = listener.accept().unwrap();
            let request = read_request(&mut observation);
            assert!(
                matches!(request.operation, Operation::Client { request, .. }
                if matches!(*request, Operation::Inspect))
            );
            accepted.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(observation.read(&mut byte).unwrap(), 0);
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock));
        });
        let mut client = RegisteredClient::register_cancellable(
            &socket,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let endpoint = client.endpoint().clone();
        let worker_endpoint = endpoint.clone();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (completed, completed_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            completed
                .send(worker_endpoint.checked_cancellable(Operation::Inspect, &worker_cancellation))
                .unwrap();
        });
        accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancellation.cancel();
        let error = completed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert!(super::super::outcome_unknown(&error));
        assert!(
            !endpoint.is_revoked(),
            "request cancellation revoked the entire attachment"
        );
        assert_eq!(client.poll_lifecycle().unwrap(), Lifecycle::Active);
        client.cancel();
        finish.send(()).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn unavailable_endpoint_is_revoked_without_address_or_dispatch_authority() {
        let endpoint = Endpoint::unavailable();
        assert!(endpoint.is_revoked());
        for operation in [
            Operation::Inspect,
            Operation::SessionInfo,
            Operation::WorkspaceRename {
                workspace_id: "must-not-dispatch".into(),
                expected_revision: 1,
                name: "unavailable".into(),
            },
        ] {
            let error = endpoint.clone().checked(operation).unwrap_err();
            assert!(detached_before_dispatch(&error));
            assert!(!super::super::outcome_unknown(&error));
        }
        let error = endpoint
            .checked_cancellable(
                Operation::Inspect,
                &tokio_util::sync::CancellationToken::new(),
            )
            .unwrap_err();
        assert!(detached_before_dispatch(&error));
        assert!(!super::super::outcome_unknown(&error));
    }
}
