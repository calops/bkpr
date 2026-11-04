//! Fixed, allocation-free admission header. No negotiation or replay is implicit.
use crate::PROTOCOL_VERSION;
use anyhow::{Result, ensure};
use std::io::{Read, Write};

pub const HEADER_LEN: usize = 24;
pub const FRAME_OVERHEAD: usize = HEADER_LEN;
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE: usize = 96 * 1024 * 1024;
pub const MAX_CLIENT_EVENT: usize = MAX_REQUEST;
pub const CAP_BINARY_SCENE: u64 = 1;
pub const CAP_TYPED_FAILURE: u64 = 2;
pub const CAP_CLIENT_LIFETIME: u64 = 4;
pub const SUPPORTED_CAPABILITIES: u64 = CAP_BINARY_SCENE | CAP_TYPED_FAILURE | CAP_CLIENT_LIFETIME;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Request = 1,
    Response = 2,
    ClientEvent = 3,
}
impl Kind {
    pub const fn max_payload(self) -> usize {
        match self {
            Self::Request => MAX_REQUEST,
            Self::Response => MAX_RESPONSE,
            Self::ClientEvent => MAX_CLIENT_EVENT,
        }
    }
}

/// Validate the complete header before allocating or reading any declared payload.
/// Unknown required semantic capabilities fail closed; future optional table fields do not.
pub fn frame_payload_len(header: &[u8], expected: Kind, max: usize) -> Result<usize> {
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
    let required = u64::from_le_bytes(header[16..24].try_into()?);
    ensure!(
        required & !SUPPORTED_CAPABILITIES == 0,
        "unsupported required native capabilities {:#x}",
        required & !SUPPORTED_CAPABILITIES
    );
    let length = u32::from_le_bytes(header[12..16].try_into()?) as usize;
    ensure!(
        length <= max.min(expected.max_payload()),
        "native frame exceeds payload limit"
    );
    Ok(length)
}

fn header(kind: Kind, length: usize) -> Result<[u8; HEADER_LEN]> {
    ensure!(
        length <= kind.max_payload(),
        "native frame exceeds payload limit"
    );
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(b"BKPR");
    header[4..8].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header[8] = kind as u8;
    header[12..16].copy_from_slice(&u32::try_from(length)?.to_le_bytes());
    header[16..24].copy_from_slice(&SUPPORTED_CAPABILITIES.to_le_bytes());
    Ok(header)
}

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
pub fn write_frame(writer: &mut impl Write, kind: Kind, payload: &[u8]) -> Result<()> {
    writer.write_all(&header(kind, payload.len())?)?;
    writer.write_all(payload)?;
    Ok(())
}
/// Own a queued write without retaining a reusable codec builder's borrowed output.
pub fn encode_frame(kind: Kind, payload: &[u8]) -> Result<Vec<u8>> {
    let header = header(kind, payload.len())?;
    let mut frame = Vec::new();
    frame.try_reserve_exact(HEADER_LEN + payload.len())?;
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    Ok(frame)
}
