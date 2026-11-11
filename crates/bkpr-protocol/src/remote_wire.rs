//! Remote attachment control version 1, independent of native operation framing.
//!
//! Control payloads carry handshakes and lease heartbeats, never native operations.
//! Decoding validates individual messages; callers must associate handshakes with [`Welcome::validate_for`].
#[rustfmt::skip]
#[allow(dead_code, unused_imports, clippy::all)]
mod generated;
#[cfg(test)]
mod tests;

use crate::{PROTOCOL_VERSION, SessionInfo, valid_session_name};
use anyhow::{Result, ensure};
use generated::remote_wire as g;
use planus::{Builder, ReadAsRoot};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Remote control wire version, independent of the native wire family.
pub const VERSION: u32 = 1;
/// Remote frame header size in bytes.
pub const HEADER_LEN: usize = 16;
/// Maximum remote control payload size in bytes, excluding its header.
pub const MAX_CONTROL: usize = 64 * 1024;
/// Maximum UTF-8 byte length of a remote session name.
pub const MAX_SESSION_NAME: usize = 4096;
/// Maximum UTF-8 byte length of a remote session or daemon identity.
pub const MAX_IDENTITY: usize = 256;
/// Maximum byte length of the remote helper's SSH agent socket path.
pub const MAX_AGENT_PATH: usize = 4096;
/// Maximum UTF-8 byte length of a remote failure diagnostic.
pub const MAX_FAILURE: usize = 4096;
/// Number of bytes in a connection-association nonce.
pub const NONCE_LEN: usize = 32;
/// Minimum accepted heartbeat interval in milliseconds.
pub const MIN_HEARTBEAT_MS: u32 = 250;
/// Maximum accepted heartbeat interval in milliseconds.
pub const MAX_HEARTBEAT_MS: u32 = 30_000;
/// Maximum accepted lease timeout in milliseconds.
pub const MAX_LEASE_TIMEOUT_MS: u32 = 120_000;

/// Attachment request naming the remote session and required native semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Remote session name, subject to session-name validation and [`MAX_SESSION_NAME`].
    pub session_name: String,
    /// Native wire family the attachment requires.
    pub native_family: u32,
    /// Native semantic capability bits the remote endpoint must support.
    pub required_capabilities: u64,
    /// Per-connection nonce that the welcome must echo.
    pub connection_nonce: [u8; NONCE_LEN],
    /// Whether this attachment requests SSH agent forwarding.
    pub forward_agent: bool,
}
/// Remote session identity, negotiated capabilities and connection lease parameters.
#[derive(Clone, PartialEq, Eq)]
pub struct Welcome {
    /// Remote session and daemon identities to recheck during native client registration.
    pub session: SessionInfo,
    /// Nonce echoed from the associated hello.
    pub connection_nonce: [u8; NONCE_LEN],
    /// Native semantic capabilities advertised by the remote endpoint.
    pub capabilities: u64,
    /// Helper-side SSH agent socket path, never a path from the local environment.
    pub agent_socket: Option<PathBuf>,
    /// Heartbeat interval in milliseconds, within the supported bounds.
    pub heartbeat_interval_ms: u32,
    /// Lease timeout in milliseconds, at least three heartbeat intervals and at most [`MAX_LEASE_TIMEOUT_MS`].
    pub lease_timeout_ms: u32,
}
impl std::fmt::Debug for Welcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Welcome")
            .field("session", &self.session)
            .field("connection_nonce", &self.connection_nonce)
            .field("capabilities", &self.capabilities)
            .field(
                "agent_socket",
                &self.agent_socket.as_ref().map(|_| "[REDACTED]"),
            )
            .field("heartbeat_interval_ms", &self.heartbeat_interval_ms)
            .field("lease_timeout_ms", &self.lease_timeout_ms)
            .finish()
    }
}
/// Bounded remote attachment failure diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Nonempty, control-character-free text of at most [`MAX_FAILURE`] UTF-8 bytes.
    pub message: String,
}
impl Failure {
    /// Replace control characters, truncate to [`MAX_FAILURE`] UTF-8 bytes and supply nonempty text.
    /// Does not redact credentials or private environment values.
    pub fn new(message: impl AsRef<str>) -> Self {
        let mut sanitized = String::new();
        for c in message.as_ref().chars() {
            let c = if c.is_control() { '?' } else { c };
            if sanitized.len() + c.len_utf8() > MAX_FAILURE {
                break;
            }
            sanitized.push(c);
        }
        if sanitized.is_empty() {
            sanitized.push_str("remote attachment failed");
        }
        Self { message: sanitized }
    }
}
/// Remote handshake, heartbeat or attachment-close message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// Begin attachment negotiation.
    Hello(
        /// Requested session and native attachment requirements.
        Hello,
    ),
    /// Return the remote session and lease parameters.
    Welcome(
        /// Associated session identity and lease parameters.
        Welcome,
    ),
    /// Reject or report failure of an attachment.
    Failure(
        /// Bounded diagnostic describing the failure.
        Failure,
    ),
    /// Request a heartbeat response with the same sequence.
    Ping {
        /// Opaque sequence to echo in the pong.
        sequence: u64,
    },
    /// Respond to a heartbeat request.
    Pong {
        /// Sequence echoed from the ping.
        sequence: u64,
    },
    /// End the remote attachment.
    Close,
}

impl Hello {
    /// Reject an invalid session name, unsupported native family or unknown required capability.
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.session_name)?;
        ensure!(
            self.native_family == PROTOCOL_VERSION,
            "unsupported remote native wire family"
        );
        ensure!(
            self.required_capabilities & !crate::wire::SUPPORTED_CAPABILITIES == 0,
            "unsupported required remote native capabilities"
        );
        Ok(())
    }
}
impl Welcome {
    /// Reject invalid session identities, lease bounds or remote agent-path bytes.
    /// Does not associate this welcome with a hello; use [`Self::validate_for`] for that.
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.session.name)?;
        validate_text(&self.session.session_id, MAX_IDENTITY, "session identity")?;
        validate_text(&self.session.daemon_id, MAX_IDENTITY, "daemon identity")?;
        ensure!(
            (MIN_HEARTBEAT_MS..=MAX_HEARTBEAT_MS).contains(&self.heartbeat_interval_ms),
            "remote heartbeat interval outside supported bounds"
        );
        ensure!(
            self.lease_timeout_ms >= self.heartbeat_interval_ms.saturating_mul(3)
                && self.lease_timeout_ms <= MAX_LEASE_TIMEOUT_MS,
            "remote lease timeout outside supported bounds"
        );
        if let Some(path) = &self.agent_socket {
            validate_path(path_bytes(path)?)?;
        }
        Ok(())
    }
    /// Validate both messages and their name, nonce, capability and forwarding agreement.
    /// Native client registration must still recheck the returned session and daemon identities.
    pub fn validate_for(&self, hello: &Hello) -> Result<()> {
        hello.validate()?;
        self.validate()?;
        ensure!(
            self.session.name == hello.session_name,
            "remote session name mismatch"
        );
        ensure!(
            self.connection_nonce == hello.connection_nonce,
            "remote connection nonce mismatch"
        );
        ensure!(
            hello.required_capabilities & !self.capabilities == 0,
            "remote session lacks required native capabilities"
        );
        ensure!(
            hello.forward_agent || self.agent_socket.is_none(),
            "unsolicited remote SSH agent socket"
        );
        ensure!(
            !hello.forward_agent || self.agent_socket.is_some(),
            "requested remote SSH agent forwarding is unavailable"
        );
        Ok(())
    }
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        name.len() <= MAX_SESSION_NAME,
        "remote session name exceeds limit"
    );
    valid_session_name(name)
}
fn validate_text(text: &str, max: usize, field: &str) -> Result<()> {
    ensure!(
        !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control),
        "invalid or excessive remote {field}"
    );
    Ok(())
}
fn validate_path(bytes: &[u8]) -> Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_AGENT_PATH && !bytes.contains(&0),
        "invalid or excessive remote agent path"
    );
    Ok(())
}
#[cfg(unix)]
fn path_bytes(path: &Path) -> Result<&[u8]> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes())
}
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<&[u8]> {
    Ok(path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("remote Unix path is unsupported"))?
        .as_bytes())
}
#[cfg(unix)]
fn owned_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
}
#[cfg(not(unix))]
fn owned_path(bytes: &[u8]) -> Result<PathBuf> {
    Ok(std::str::from_utf8(bytes)?.into())
}

// The schema is flat and nonrecursive: at most four strings and one byte vector
// can be copied. Charge every copy before allocation, even for aliased offsets.
struct Budget(usize);
impl Budget {
    fn charge(&mut self, size: usize) -> Result<()> {
        self.0 = self
            .0
            .checked_sub(size)
            .ok_or_else(|| anyhow::anyhow!("remote aggregate allocation limit"))?;
        Ok(())
    }
    fn text(&mut self, text: &str, max: usize, field: &str) -> Result<String> {
        validate_text(text, max, field)?;
        self.charge(text.len())?;
        Ok(text.to_owned())
    }
}

/// Reusable remote control encoder; returned bytes borrow its storage until the next mutable call.
/// Use [`encode_frame`] to retain an owned frame for a queued write.
#[derive(Default)]
pub struct Codec {
    builder: Builder,
}
impl Codec {
    /// Create an encoder with empty reusable storage.
    pub fn new() -> Self {
        Self::default()
    }
    /// Encode an unframed control message, rejecting invalid fields or excessive payload size.
    /// Does not validate hello/welcome association.
    pub fn encode(&mut self, control: &Control) -> Result<&[u8]> {
        self.builder.clear();
        let b = &mut self.builder;
        let value = match control {
            Control::Hello(hello) => {
                hello.validate()?;
                let name = b.create_string(&hello.session_name);
                let nonce = b.create_vector(hello.connection_nonce.as_slice());
                let hello = g::Hello::create(
                    b,
                    name,
                    hello.native_family,
                    hello.required_capabilities,
                    nonce,
                    hello.forward_agent,
                );
                g::Control::create_hello(b, hello)
            }
            Control::Welcome(welcome) => {
                welcome.validate()?;
                let name = b.create_string(&welcome.session.name);
                let session_id = b.create_string(&welcome.session.session_id);
                let daemon_id = b.create_string(&welcome.session.daemon_id);
                let session = g::SessionInfo::create(b, name, session_id, daemon_id);
                let nonce = b.create_vector(welcome.connection_nonce.as_slice());
                let agent = welcome
                    .agent_socket
                    .as_deref()
                    .map(|path| -> Result<_> {
                        let bytes = b.create_vector(path_bytes(path)?);
                        let path = g::UnixPath::create(b, bytes);
                        let value = g::AgentPath::create_unix_path(b, path);
                        Ok(g::OptionalAgentPath::create(b, value))
                    })
                    .transpose()?;
                let welcome = g::Welcome::create(
                    b,
                    session,
                    nonce,
                    welcome.capabilities,
                    agent,
                    welcome.heartbeat_interval_ms,
                    welcome.lease_timeout_ms,
                );
                g::Control::create_welcome(b, welcome)
            }
            Control::Failure(failure) => {
                validate_text(&failure.message, MAX_FAILURE, "failure diagnostic")?;
                let message = b.create_string(&failure.message);
                let failure = g::Failure::create(b, message);
                g::Control::create_failure(b, failure)
            }
            Control::Ping { sequence } => {
                let ping = g::Ping::create(b, *sequence);
                g::Control::create_ping(b, ping)
            }
            Control::Pong { sequence } => {
                let pong = g::Pong::create(b, *sequence);
                g::Control::create_pong(b, pong)
            }
            Control::Close => {
                let close = g::Close::create(b);
                g::Control::create_close(b, close)
            }
        };
        let root = g::Message::create(b, value);
        let bytes = b.finish(root, None);
        ensure!(
            bytes.len() <= MAX_CONTROL,
            "remote control payload exceeds limit"
        );
        Ok(bytes)
    }
}

/// Decode an owned control message with field, payload and aggregate-copy limits.
/// Handshake association still requires [`Welcome::validate_for`].
pub fn decode(payload: &[u8]) -> Result<Control> {
    ensure!(
        payload.len() <= MAX_CONTROL,
        "remote control payload exceeds limit"
    );
    let mut budget = Budget(MAX_CONTROL);
    let root = g::MessageRef::read_as_root(payload)?;
    let control = match root.control()? {
        g::ControlRef::Hello(hello) => Control::Hello(Hello {
            session_name: budget.text(hello.session_name()?, MAX_SESSION_NAME, "session name")?,
            native_family: hello.native_family()?,
            required_capabilities: hello.required_capabilities()?,
            connection_nonce: hello
                .connection_nonce()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid remote connection nonce length"))?,
            forward_agent: hello.forward_agent()?,
        }),
        g::ControlRef::Welcome(welcome) => {
            let session = welcome.session()?;
            let session = SessionInfo {
                name: budget.text(session.name()?, MAX_SESSION_NAME, "session name")?,
                session_id: budget.text(session.session_id()?, MAX_IDENTITY, "session identity")?,
                daemon_id: budget.text(session.daemon_id()?, MAX_IDENTITY, "daemon identity")?,
            };
            let agent_socket = welcome
                .agent_socket()?
                .map(|wrapper| -> Result<_> {
                    let g::AgentPathRef::UnixPath(path) = wrapper.value()?;
                    let bytes = path.bytes()?;
                    validate_path(bytes)?;
                    budget.charge(bytes.len())?;
                    owned_path(bytes)
                })
                .transpose()?;
            Control::Welcome(Welcome {
                session,
                connection_nonce: welcome
                    .connection_nonce()?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid remote connection nonce length"))?,
                capabilities: welcome.capabilities()?,
                agent_socket,
                heartbeat_interval_ms: welcome.heartbeat_interval_ms()?,
                lease_timeout_ms: welcome.lease_timeout_ms()?,
            })
        }
        g::ControlRef::Failure(failure) => Control::Failure(Failure {
            message: budget.text(failure.message()?, MAX_FAILURE, "failure diagnostic")?,
        }),
        g::ControlRef::Ping(ping) => Control::Ping {
            sequence: ping.sequence()?,
        },
        g::ControlRef::Pong(pong) => Control::Pong {
            sequence: pong.sequence()?,
        },
        g::ControlRef::Close(_) => Control::Close,
    };
    match &control {
        Control::Hello(hello) => hello.validate()?,
        Control::Welcome(welcome) => welcome.validate()?,
        _ => {}
    }
    Ok(control)
}

/// Return the payload length after validating remote magic, version, reserved bits and bounds.
/// The 16-byte header contains magic, version, payload length and reserved u32, in little endian.
pub fn frame_payload_len(header: &[u8]) -> Result<usize> {
    ensure!(
        header.len() == HEADER_LEN,
        "incomplete remote control header"
    );
    ensure!(&header[..4] == b"BKRS", "not a Bkpr remote control frame");
    ensure!(
        u32::from_le_bytes(header[4..8].try_into()?) == VERSION,
        "unsupported Bkpr remote control version"
    );
    ensure!(
        header[12..16] == [0; 4],
        "nonzero remote control reserved bits"
    );
    let length = u32::from_le_bytes(header[8..12].try_into()?) as usize;
    ensure!(
        (4..=MAX_CONTROL).contains(&length),
        "remote control payload length outside bounds"
    );
    Ok(length)
}
fn header(length: usize) -> Result<[u8; HEADER_LEN]> {
    ensure!(
        (4..=MAX_CONTROL).contains(&length),
        "remote control payload length outside bounds"
    );
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(b"BKRS");
    header[4..8].copy_from_slice(&VERSION.to_le_bytes());
    header[8..12].copy_from_slice(&u32::try_from(length)?.to_le_bytes());
    Ok(header)
}
/// Read one owned remote payload after header admission; reject truncation or allocation failure.
/// The returned bytes exclude the header and still require [`decode`].
pub fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let length = frame_payload_len(&header)?;
    let mut payload = Vec::new();
    payload.try_reserve_exact(length)?;
    payload.resize(length, 0);
    reader.read_exact(&mut payload)?;
    Ok(payload)
}
/// Write a remote control header and payload without flushing or decoding the payload.
/// An I/O error may follow a partial write and does not establish safe replay.
pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<()> {
    writer.write_all(&header(payload.len())?)?;
    writer.write_all(payload)?;
    Ok(())
}
/// Copy a payload into an owned remote frame, enforcing size bounds without decoding it.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>> {
    let header = header(payload.len())?;
    let mut frame = Vec::new();
    frame.try_reserve_exact(HEADER_LEN + payload.len())?;
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    Ok(frame)
}
