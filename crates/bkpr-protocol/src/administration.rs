//! Frozen, bounded host/session inspection and shutdown independent of native families.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};

/// Administration frame discriminator, distinct from native data-plane framing.
pub const MAGIC: [u8; 4] = *b"BKPA";
/// Frozen administration header length in bytes.
pub const HEADER_LEN: usize = 24;
/// Maximum administration JSON body length in bytes.
pub const MAX_PAYLOAD: usize = 8192;

/// Exact endpoint incarnation required before any stop effect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Identity {
    /// Entire host daemon; stopping it closes all of its sessions.
    Host {
        /// Host daemon incarnation, nonempty and at most 256 UTF-8 bytes.
        daemon_id: String,
    },
    /// One session runtime; stopping it does not stop the host or other sessions.
    Session {
        /// Owning host daemon incarnation, nonempty and at most 256 UTF-8 bytes.
        daemon_id: String,
        /// Persistent session identity, nonempty and at most 256 UTF-8 bytes.
        session_id: String,
        /// Fresh session runtime identity, nonempty and at most 256 UTF-8 bytes.
        session_instance_id: String,
    },
}
impl Identity {
    fn validate(&self) -> Result<()> {
        let valid = |id: &str| !id.is_empty() && id.len() <= 256;
        match self {
            Self::Host { daemon_id } => ensure!(valid(daemon_id), "invalid daemon identity"),
            Self::Session {
                daemon_id,
                session_id,
                session_instance_id,
            } => {
                ensure!(valid(daemon_id), "invalid daemon identity");
                ensure!(valid(session_id), "invalid session identity");
                ensure!(
                    valid(session_instance_id),
                    "invalid session runtime identity"
                );
            }
        }
        Ok(())
    }
}

/// Closed version-one administration operations; mutations must never be automatically retried.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Read immutable endpoint identity even if durable storage is unavailable.
    Inspect {},
    /// Stop only if the connected endpoint still has exactly this identity and role.
    Stop {
        /// Exact identity previously observed on the retained connection's endpoint.
        expected: Identity,
    },
}
impl Request {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Inspect {} => Ok(()),
            Self::Stop { expected } => expected.validate(),
        }
    }
}

/// Version-one administration result; `Stopped` means owned cleanup succeeded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// Immutable endpoint identity, independent of native wire family.
    Identified {
        /// Exact host or session incarnation served by this endpoint.
        identity: Identity,
    },
    /// The requested shutdown operation completed successfully.
    Stopped {},
    /// Rejected input, target mismatch or a truthful owned-cleanup failure.
    Rejected {
        /// Human-readable failure; never establishes safe mutation replay.
        error: String,
    },
}
impl Response {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Identified { identity } => identity.validate(),
            Self::Stopped {} => Ok(()),
            Self::Rejected { error } => {
                ensure!(
                    !error.is_empty() && error.len() <= MAX_PAYLOAD,
                    "invalid administration error"
                );
                Ok(())
            }
        }
    }
}

/// A valid native response header from a server without this administration channel.
/// No native response body is consumed or interpreted as administration JSON.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeFamily(pub u32);
impl std::fmt::Display for NativeFamily {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "server answered with native wire family {}",
            self.0
        )
    }
}
impl std::error::Error for NativeFamily {}

/// Validate the complete frozen header and return its bounded body length.
/// `response` selects response kind 2 rather than request kind 1.
pub fn frame_payload_len(header: &[u8], response: bool) -> Result<usize> {
    ensure!(
        header.len() == HEADER_LEN,
        "incomplete administration header"
    );
    ensure!(header[..4] == MAGIC, "not a Bkpr administration frame");
    ensure!(
        u32::from_le_bytes(header[4..8].try_into()?) == 1,
        "unsupported administration version"
    );
    ensure!(
        header[8] == if response { 2 } else { 1 },
        "unexpected administration message kind"
    );
    ensure!(
        header[9..12] == [0; 3] && header[16..24] == [0; 8],
        "nonzero administration reserved bits"
    );
    let length = u32::from_le_bytes(header[12..16].try_into()?) as usize;
    ensure!(
        length <= MAX_PAYLOAD,
        "administration body exceeds payload limit"
    );
    Ok(length)
}

fn write_message(writer: &mut impl Write, value: &impl Serialize, response: bool) -> Result<()> {
    let mut bytes = [0; MAX_PAYLOAD];
    let mut body = Cursor::new(bytes.as_mut_slice());
    serde_json::to_writer(&mut body, value)?;
    let length = body.position() as usize;
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..8].copy_from_slice(&1_u32.to_le_bytes());
    header[8] = if response { 2 } else { 1 };
    header[12..16].copy_from_slice(&(length as u32).to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(&bytes[..length])?;
    Ok(())
}

/// Validate and write one request without flushing. I/O failure may follow a partial write.
pub fn write_request(writer: &mut impl Write, request: &Request) -> Result<()> {
    request.validate()?;
    write_message(writer, request, false)
}

/// Read one framed request, rejecting malformed headers, JSON and invalid identities.
pub fn read_request(reader: &mut impl Read) -> Result<Request> {
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let length = frame_payload_len(&header, false)?;
    let mut body = [0; MAX_PAYLOAD];
    reader.read_exact(&mut body[..length])?;
    decode_request(&body[..length])
}

/// Decode an unframed bounded request body, rejecting unknown fields and invalid identities.
pub fn decode_request(bytes: &[u8]) -> Result<Request> {
    ensure!(
        bytes.len() <= MAX_PAYLOAD,
        "administration body exceeds payload limit"
    );
    let request: Request = serde_json::from_slice(bytes)?;
    request.validate()?;
    Ok(request)
}

/// Validate and write one response without flushing. I/O failure may follow a partial write.
pub fn write_response(writer: &mut impl Write, response: &Response) -> Result<()> {
    response.validate()?;
    write_message(writer, response, true)
}

/// Read a bounded response. A valid old-server native response header returns [`NativeFamily`].
/// No native payload is accepted as an administration response.
pub fn read_response(reader: &mut impl Read) -> Result<Response> {
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if &header[..4] == b"BKPR" {
        let family = u32::from_le_bytes(header[4..8].try_into()?);
        let mut native = header;
        native[4..8].copy_from_slice(&crate::PROTOCOL_VERSION.to_le_bytes());
        crate::wire::frame_payload_len(
            &native,
            crate::wire::Kind::Response,
            crate::wire::MAX_RESPONSE,
        )?;
        return Err(NativeFamily(family).into());
    }
    let length = frame_payload_len(&header, true)?;
    let mut body = [0; MAX_PAYLOAD];
    reader.read_exact(&mut body[..length])?;
    let response: Response = serde_json::from_slice(&body[..length])?;
    response.validate()?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity::Session {
            daemon_id: "host".into(),
            session_id: "session".into(),
            session_instance_id: "runtime".into(),
        }
    }

    #[test]
    fn frozen_headers_and_messages_round_trip() {
        for request in [
            Request::Inspect {},
            Request::Stop {
                expected: identity(),
            },
        ] {
            let mut bytes = Vec::new();
            write_request(&mut bytes, &request).unwrap();
            assert_eq!(&bytes[..12], b"BKPA\x01\0\0\0\x01\0\0\0");
            assert_eq!(
                frame_payload_len(&bytes[..HEADER_LEN], false).unwrap(),
                bytes.len() - HEADER_LEN
            );
            assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), request);
        }
        for response in [
            Response::Identified {
                identity: identity(),
            },
            Response::Stopped {},
            Response::Rejected {
                error: "cleanup failed".into(),
            },
        ] {
            let mut bytes = Vec::new();
            write_response(&mut bytes, &response).unwrap();
            assert_eq!(read_response(&mut bytes.as_slice()).unwrap(), response);
        }
    }

    #[test]
    fn closed_arguments_and_identity_limits_are_enforced_before_writing() {
        for body in [
            r#"{"op":"inspect","extra":true}"#,
            r#"{"op":"stop","expected":{"kind":"host","daemon_id":""}}"#,
            r#"{"op":"stop","expected":{"kind":"host","daemon_id":"ok","session_id":"other"}}"#,
            r#"{"op":"stop","expected":{"kind":"session","daemon_id":"host","session_id":"s"}}"#,
            r#"{"op":"inspect"}{}"#,
        ] {
            assert!(decode_request(body.as_bytes()).is_err(), "{body}");
        }
        for size in [0, 257] {
            let mut bytes = Vec::new();
            assert!(
                write_request(
                    &mut bytes,
                    &Request::Stop {
                        expected: Identity::Host {
                            daemon_id: "x".repeat(size)
                        }
                    }
                )
                .is_err()
            );
            assert!(bytes.is_empty());
        }
        let mut bytes = Vec::new();
        assert!(
            write_response(
                &mut bytes,
                &Response::Rejected {
                    error: "\0".repeat(MAX_PAYLOAD)
                }
            )
            .is_err()
        );
        assert!(bytes.is_empty());
        assert!(decode_request(&vec![b' '; MAX_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn malformed_bounded_headers_and_bodies_are_rejected() {
        let mut frame = Vec::new();
        write_request(&mut frame, &Request::Inspect {}).unwrap();
        for index in [0, 4, 8, 9, 16, 23] {
            let mut invalid = frame.clone();
            invalid[index] ^= 128;
            assert!(read_request(&mut invalid.as_slice()).is_err());
        }
        let mut oversized = frame.clone();
        oversized[12..16].copy_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        assert!(read_request(&mut oversized.as_slice()).is_err());
        assert!(read_request(&mut &frame[..frame.len() - 1]).is_err());
        assert!(frame_payload_len(&frame[..HEADER_LEN - 1], false).is_err());
        let mut response = Vec::new();
        write_response(&mut response, &Response::Stopped {}).unwrap();
        let body = br#"{"status":"stopped","extra":true}"#;
        response.truncate(HEADER_LEN);
        response[12..16].copy_from_slice(&(body.len() as u32).to_le_bytes());
        response.extend_from_slice(body);
        assert!(read_response(&mut response.as_slice()).is_err());
    }

    #[test]
    fn native_fallback_recognizes_only_valid_response_headers_without_decoding() {
        let mut frame =
            crate::wire::encode_frame(crate::wire::Kind::Response, b"not admin JSON").unwrap();
        frame[4..8].copy_from_slice(&7_u32.to_le_bytes());
        let mut input = frame.as_slice();
        let error = read_response(&mut input).unwrap_err();
        assert_eq!(error.downcast_ref::<NativeFamily>(), Some(&NativeFamily(7)));
        assert_eq!(input, b"not admin JSON");
        for index in [8, 9, 23] {
            let mut invalid = frame.clone();
            invalid[index] ^= 128;
            assert!(
                read_response(&mut invalid.as_slice())
                    .unwrap_err()
                    .downcast_ref::<NativeFamily>()
                    .is_none()
            );
        }
    }
}
