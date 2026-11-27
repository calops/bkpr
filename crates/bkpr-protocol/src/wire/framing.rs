//! Fixed, allocation-free admission header. No negotiation or replay is implicit.
use super::{MAX_NATIVE_DEPTH, g};
use crate::PROTOCOL_VERSION;
use anyhow::{Result, ensure};
use planus::ReadAsRoot;
use std::io::{Read, Write};

/// Native frame header size in bytes.
pub const HEADER_LEN: usize = 24;
/// Bytes added to a native payload by framing.
pub const FRAME_OVERHEAD: usize = HEADER_LEN;
/// Maximum native request payload size in bytes, excluding its header.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
/// Maximum native response payload size in bytes, excluding its header.
pub const MAX_RESPONSE: usize = 96 * 1024 * 1024;
/// Maximum client-event payload size in bytes, excluding its header.
pub const MAX_CLIENT_EVENT: usize = MAX_REQUEST;
/// Capability bit requiring binary pane-scene support.
pub const CAP_BINARY_SCENE: u64 = 1;
/// Capability bit requiring typed request-failure support.
pub const CAP_TYPED_FAILURE: u64 = 2;
/// Capability bit requiring connection-bound client-lifetime support.
pub const CAP_CLIENT_LIFETIME: u64 = 4;
/// Baseline required capability bits emitted on native frames and remote handshakes.
pub const SUPPORTED_CAPABILITIES: u64 = CAP_BINARY_SCENE | CAP_TYPED_FAILURE | CAP_CLIENT_LIFETIME;
// Per-request capabilities are not part of the connection's baseline mask.
pub(super) const CAP_CHANNEL_POST_SOURCE: u64 = 8;
/// Per-request requirement for typed workspace role operations.
pub const CAP_WORKSPACE_ROLES: u64 = 16;

/// Native payload category used for header admission and size limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    /// Client-to-daemon operation.
    Request = 1,
    /// Daemon reply to an operation.
    Response = 2,
    /// Daemon event on a registered client's lifetime connection.
    ClientEvent = 3,
}
impl Kind {
    /// Maximum payload bytes accepted for this category, excluding its header.
    pub const fn max_payload(self) -> usize {
        match self {
            Self::Request => MAX_REQUEST,
            Self::Response => MAX_RESPONSE,
            Self::ClientEvent => MAX_CLIENT_EVENT,
        }
    }
}

/// Return the declared length after validating the native header and expected kind.
/// Reject unknown required capabilities and lengths above either `max` or the kind's limit.
pub fn frame_payload_len(header: &[u8], expected: Kind, max: usize) -> Result<usize> {
    let capabilities = if expected == Kind::Request {
        SUPPORTED_CAPABILITIES | CAP_CHANNEL_POST_SOURCE | CAP_WORKSPACE_ROLES
    } else {
        SUPPORTED_CAPABILITIES
    };
    frame_payload_len_with_capabilities(header, expected, max, capabilities)
}

pub(super) fn frame_payload_len_with_capabilities(
    header: &[u8],
    expected: Kind,
    max: usize,
    capabilities: u64,
) -> Result<usize> {
    ensure!(header.len() == HEADER_LEN, "incomplete native frame header");
    ensure!(
        &header[..4] == b"BKPR",
        "not a Bkpr native frame (old JSON transport is unsupported)"
    );
    let version = u32::from_le_bytes(header[4..8].try_into()?);
    ensure!(
        version == PROTOCOL_VERSION,
        "unsupported Bkpr wire family {version}; expected {PROTOCOL_VERSION}"
    );
    ensure!(
        header[8] == expected as u8,
        "unexpected or unsupported native message kind {}",
        header[8]
    );
    ensure!(
        header[9..12] == [0; 3],
        "nonzero native frame reserved bits"
    );
    let unsupported = u64::from_le_bytes(header[16..24].try_into()?) & !capabilities;
    ensure!(
        unsupported == 0,
        "unsupported required native capabilities {unsupported:#x}"
    );
    let length = u32::from_le_bytes(header[12..16].try_into()?) as usize;
    ensure!(
        length <= max.min(expected.max_payload()),
        "native frame exceeds payload limit"
    );
    Ok(length)
}

fn header(kind: Kind, payload: &[u8]) -> Result<[u8; HEADER_LEN]> {
    let length = payload.len();
    ensure!(
        length <= kind.max_payload(),
        "native frame exceeds payload limit"
    );
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(b"BKPR");
    header[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header[8] = kind as u8;
    header[12..16].copy_from_slice(&u32::try_from(length)?.to_le_bytes());
    let capabilities = SUPPORTED_CAPABILITIES
        | if kind == Kind::Request {
            request_capabilities(payload).unwrap_or(0)
        } else {
            0
        };
    header[16..24].copy_from_slice(&capabilities.to_le_bytes());
    Ok(header)
}

fn request_capabilities(payload: &[u8]) -> planus::Result<u64> {
    let mut operation = g::RequestRef::read_as_root(payload)?.operation()?;
    for _ in 0..=MAX_NATIVE_DEPTH {
        let channel = match operation {
            g::OperationRef::OperationClient(client) => {
                operation = client.request()?;
                continue;
            }
            g::OperationRef::OperationAgent(agent) => match agent.request()? {
                g::AgentRequestRef::AgentRequestChannels(channels) => channels.request()?,
                g::AgentRequestRef::AgentRequestChannelDirectPost(post) => {
                    return Ok(source_capability(!matches!(
                        post.post()?.source(),
                        Ok(None)
                    )));
                }
                g::AgentRequestRef::AgentRequestControl(control) => {
                    operation = control.request()?;
                    continue;
                }
                g::AgentRequestRef::AgentRequestRoleRequest(_)
                | g::AgentRequestRef::AgentRequestRoleRelease(_)
                | g::AgentRequestRef::AgentRequestRoleStatus(_) => return Ok(CAP_WORKSPACE_ROLES),
                _ => return Ok(0),
            },
            g::OperationRef::OperationChannels(channels) => channels.request()?,
            g::OperationRef::OperationRoles(_) => return Ok(CAP_WORKSPACE_ROLES),
            _ => return Ok(0),
        };
        return Ok(source_capability(match channel {
            // A legacy reader skips this slot even when the source offset is malformed.
            g::ChannelRequestRef::ChannelRequestPost(post) => {
                !matches!(post.post()?.source(), Ok(None))
            }
            g::ChannelRequestRef::ChannelRequestPostUser(post) => {
                !matches!(post.post()?.source(), Ok(None))
            }
            g::ChannelRequestRef::ChannelRequestPostUserWithCoordinator(post) => {
                !matches!(post.post()?.source(), Ok(None))
            }
            _ => false,
        }));
    }
    Ok(0)
}

fn source_capability(required: bool) -> u64 {
    if required { CAP_CHANNEL_POST_SOURCE } else { 0 }
}

/// Read one owned payload after admitting its header; reject truncation or allocation failure.
/// The returned bytes exclude the header and still require payload decoding.
pub fn read_frame(reader: &mut impl Read, expected: Kind, max: usize) -> Result<Vec<u8>> {
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let length = frame_payload_len(&header, expected, max)?;
    let mut payload = Vec::new();
    payload.try_reserve_exact(length)?;
    payload.resize(length, 0);
    reader.read_exact(&mut payload)?;
    Ok(payload)
}
/// Write a native header and payload without flushing or validating the payload's contents.
/// Source-bearing channel post requests declare their required capability in the header.
/// An I/O error may follow a partial write and does not establish safe replay.
pub fn write_frame(writer: &mut impl Write, kind: Kind, payload: &[u8]) -> Result<()> {
    writer.write_all(&header(kind, payload)?)?;
    writer.write_all(payload)?;
    Ok(())
}
/// Copy a payload into an owned native frame for queued writes.
/// Enforces the kind's size limit and declares source-bearing channel post requests in the header.
pub fn encode_frame(kind: Kind, payload: &[u8]) -> Result<Vec<u8>> {
    let header = header(kind, payload)?;
    let mut frame = Vec::new();
    frame.try_reserve_exact(HEADER_LEN + payload.len())?;
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    Ok(frame)
}
