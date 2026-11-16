//! Bounded administrative migration bridge for already-running native families 5 through 12.
//!
//! Families 5–10 have per-session daemons; only 11–12 have host administration.
//! This is not a data-plane decoder or a negotiation/retry mechanism. New daemons use BKPA.
use super::{Budget, generated::native_wire as g, value};
use anyhow::{Result, bail, ensure};
use planus::{Builder, ReadAsRoot};
use serde_json::Value;
use std::io::Read;

const HEADER_LEN: usize = 24;
const MAX_PAYLOAD: usize = 8192;
const CAPABILITIES: u64 = 7;

// Frozen native.fbs/framing.rs: 22b8fdc (5), b6c329c (6), 88902be (7),
// 6556b97 (8), 7e71a0b (9), eb10a4f (10), ab420b9 (11), 44d3dc1 (12).
// These four operation tables, their ordinals (1/28/32/34), and ControlPayload are unchanged.
/// The only legacy operations available for inspecting and stopping an existing daemon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Inspect the session served by this connection (families 5–12).
    SessionInfo,
    /// Stop only the connected session; before family 11 this also stops its daemon.
    Shutdown,
    /// Inspect a shared host daemon (families 11–12 only).
    DaemonInfo,
    /// Stop the shared host daemon only if its incarnation matches (families 11–12).
    DaemonShutdown {
        /// Nonempty host incarnation, at most 256 UTF-8 bytes.
        daemon_id: String,
    },
}

/// A decoded legacy daemon's explicit rejection, distinct from an uncertain I/O outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rejected(
    /// The daemon's diagnostic, or an explanation that it supplied none.
    pub String,
);

impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "legacy administrative request rejected: {}",
            self.0
        )
    }
}

impl std::error::Error for Rejected {}

fn validate_family(family: u32) -> Result<()> {
    ensure!(
        (5..=12).contains(&family),
        "unsupported legacy administrative wire family {family}; expected 5 through 12"
    );
    Ok(())
}

/// Encode one complete native frame for a known legacy administrative operation.
///
/// Rejects unknown families, host operations before family 11, and invalid host IDs.
/// The caller must pin and validate the target connection before dispatching a stop;
/// neither an encoding nor an I/O failure establishes permission to retry a mutation.
pub fn encode_request(family: u32, operation: &Operation) -> Result<Vec<u8>> {
    validate_family(family)?;
    let mut builder = Builder::new();
    let operation = match operation {
        Operation::SessionInfo => {
            let offset = g::OperationSessionInfo::create(&mut builder);
            g::Operation::create_operation_session_info(&mut builder, offset)
        }
        Operation::Shutdown => {
            let offset = g::OperationShutdown::create(&mut builder);
            g::Operation::create_operation_shutdown(&mut builder, offset)
        }
        Operation::DaemonInfo => {
            ensure!(
                family >= 11,
                "legacy family {family} has no host administration"
            );
            let offset = g::OperationDaemonInfo::create(&mut builder);
            g::Operation::create_operation_daemon_info(&mut builder, offset)
        }
        Operation::DaemonShutdown { daemon_id } => {
            ensure!(
                family >= 11,
                "legacy family {family} has no host administration"
            );
            ensure!(
                !daemon_id.is_empty() && daemon_id.len() <= 256,
                "legacy host daemon ID must contain 1 through 256 bytes"
            );
            let daemon_id = builder.create_string(daemon_id);
            let offset = g::OperationDaemonShutdown::create(&mut builder, daemon_id);
            g::Operation::create_operation_daemon_shutdown(&mut builder, offset)
        }
    };
    let root = g::Request::create(&mut builder, family, operation);
    frame(family, 1, builder.finish(root, None))
}

fn frame(family: u32, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len() <= MAX_PAYLOAD,
        "legacy administrative payload size limit"
    );
    let mut header = [0; HEADER_LEN];
    header[..4].copy_from_slice(b"BKPR");
    header[4..8].copy_from_slice(&family.to_le_bytes());
    header[8] = kind;
    header[12..16].copy_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
    header[16..24].copy_from_slice(&CAPABILITIES.to_le_bytes());
    let mut output = Vec::new();
    output.try_reserve_exact(HEADER_LEN + payload.len())?;
    output.extend_from_slice(&header);
    output.extend_from_slice(payload);
    Ok(output)
}

/// Read one legacy administrative reply and return its control-result diagnostic JSON.
///
/// Requires the exact previously observed family (5–12) in both header and payload.
/// Rejects non-control payloads, unknown required capabilities, malformed/truncated
/// replies, server failures, and payloads or expanded metadata exceeding 8192 bytes.
/// Does not retry, infer shutdown completion from EOF, or decode scenes/client events.
/// Explicit server failures are downcastable [`Rejected`] errors, unlike uncertain I/O failures.
pub fn read_response(reader: &mut impl Read, expected_family: u32) -> Result<Value> {
    validate_family(expected_family)?;
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    ensure!(
        &header[..4] == b"BKPR",
        "not a legacy native administrative response"
    );
    let family = u32::from_le_bytes(header[4..8].try_into()?);
    ensure!(
        family == expected_family,
        "legacy response family {family} differs from observed family {expected_family}"
    );
    ensure!(
        header[8] == 2,
        "unexpected legacy administrative response kind"
    );
    ensure!(
        header[9..12] == [0; 3],
        "nonzero legacy frame reserved bits"
    );
    let required = u64::from_le_bytes(header[16..24].try_into()?);
    ensure!(
        required & !CAPABILITIES == 0,
        "unsupported legacy required capabilities"
    );
    let length = u32::from_le_bytes(header[12..16].try_into()?) as usize;
    ensure!(
        length <= MAX_PAYLOAD,
        "legacy administrative payload size limit"
    );
    let mut payload = Vec::new();
    payload.try_reserve_exact(length)?;
    payload.resize(length, 0);
    reader.read_exact(&mut payload)?;
    let root = g::ResponseRef::read_as_root(&payload)?;
    let version = root.version()?;
    ensure!(
        version == expected_family,
        "legacy response payload version {version} differs from observed family {expected_family}"
    );
    let error = root.error()?;
    if !root.ok()? {
        return Err(Rejected(
            error
                .unwrap_or("daemon supplied no error message")
                .to_owned(),
        )
        .into());
    }
    ensure!(
        error.is_none() && root.failure()?.is_none(),
        "inconsistent successful legacy administrative response"
    );
    let result = root
        .result()?
        .ok_or_else(|| anyhow::anyhow!("missing legacy administrative result"))?;
    let g::PayloadRef::ControlPayload(result) = result.value()? else {
        bail!("legacy administrative response is not control metadata");
    };
    let mut budget = Budget::new(MAX_PAYLOAD);
    budget.remaining = MAX_PAYLOAD;
    value::read_value(result.value()?, &mut budget, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn response(family: u32, ok: bool, result: Option<&Value>, error: Option<&str>) -> Vec<u8> {
        let mut builder = Builder::new();
        let result = result.map(|value| {
            let value =
                value::write_value(&mut builder, value, &mut Budget::new(MAX_PAYLOAD), 0).unwrap();
            let control = g::ControlPayload::create(&mut builder, value);
            let payload = g::Payload::create_control_payload(&mut builder, control);
            g::OptionalPayload::create(&mut builder, payload)
        });
        let error = error.map(|error| builder.create_string(error));
        let root = g::Response::create(
            &mut builder,
            family,
            ok,
            result,
            error,
            None::<planus::Offset<g::OptionalFailure>>,
        );
        frame(family, 2, builder.finish(root, None)).unwrap()
    }

    #[test]
    fn legacy_session_and_host_metadata_decode_without_current_identity_requirements() {
        for family in 5..=12 {
            let mut session = json!({"name":"old", "session_id":"session", "daemon_id":"host"});
            if family >= 11 {
                session["session_instance_id"] = json!("instance");
            }
            let result = json!({"session":session, "spawn_defaults":null});
            assert_eq!(
                read_response(
                    &mut Cursor::new(response(family, true, Some(&result), None)),
                    family
                )
                .unwrap(),
                result
            );
            let stopped = json!({"shutdown":true});
            assert_eq!(
                read_response(
                    &mut Cursor::new(response(family, true, Some(&stopped), None)),
                    family
                )
                .unwrap(),
                stopped
            );
            if family >= 11 {
                let result = json!({"daemon":{"daemon_id":"host", "pid":42}});
                assert_eq!(
                    read_response(
                        &mut Cursor::new(response(family, true, Some(&result), None)),
                        family
                    )
                    .unwrap(),
                    result
                );
            }
        }
    }

    #[test]
    fn requests_keep_exact_family_and_allowlisted_operations() {
        for family in 5..=12 {
            for operation in [
                Operation::SessionInfo,
                Operation::Shutdown,
                Operation::DaemonInfo,
                Operation::DaemonShutdown {
                    daemon_id: "host".into(),
                },
            ] {
                let encoded = encode_request(family, &operation);
                if family < 11
                    && matches!(
                        operation,
                        Operation::DaemonInfo | Operation::DaemonShutdown { .. }
                    )
                {
                    assert!(encoded.is_err());
                    continue;
                }
                let frame = encoded.unwrap();
                assert_eq!(&frame[..4], b"BKPR");
                assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), family);
                assert_eq!(frame[8], 1);
                assert_eq!(&frame[9..12], &[0; 3]);
                assert_eq!(u64::from_le_bytes(frame[16..24].try_into().unwrap()), 7);
                let payload = &frame[HEADER_LEN..];
                let table = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
                let distance = i32::from_le_bytes(payload[table..table + 4].try_into().unwrap());
                let vtable = usize::try_from(table as i64 - i64::from(distance)).unwrap();
                let tag_field =
                    u16::from_le_bytes(payload[vtable + 6..vtable + 8].try_into().unwrap())
                        as usize;
                let expected_tag = match &operation {
                    Operation::SessionInfo => 1,
                    Operation::Shutdown => 28,
                    Operation::DaemonInfo => 32,
                    Operation::DaemonShutdown { .. } => 34,
                };
                assert_eq!(payload[table + tag_field], expected_tag);
                let root = g::RequestRef::read_as_root(&frame[HEADER_LEN..]).unwrap();
                assert_eq!(root.version().unwrap(), family);
                match (operation, root.operation().unwrap()) {
                    (Operation::SessionInfo, g::OperationRef::OperationSessionInfo(_))
                    | (Operation::Shutdown, g::OperationRef::OperationShutdown(_))
                    | (Operation::DaemonInfo, g::OperationRef::OperationDaemonInfo(_)) => {}
                    (
                        Operation::DaemonShutdown { daemon_id },
                        g::OperationRef::OperationDaemonShutdown(value),
                    ) => {
                        assert_eq!(value.daemon_id().unwrap(), daemon_id);
                    }
                    _ => panic!("wrong legacy administrative operation"),
                }
            }
        }
        for family in [0, 4, 13, u32::MAX] {
            assert!(encode_request(family, &Operation::SessionInfo).is_err());
            assert!(read_response(&mut Cursor::new([]), family).is_err());
        }
        for daemon_id in [String::new(), "h".repeat(257)] {
            assert!(encode_request(11, &Operation::DaemonShutdown { daemon_id }).is_err());
        }
        assert!(
            encode_request(
                11,
                &Operation::DaemonShutdown {
                    daemon_id: "h".repeat(256)
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn failed_missing_and_inconsistent_results_never_look_successful() {
        let error = read_response(
            &mut Cursor::new(response(10, false, None, Some("cleanup failed"))),
            10,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cleanup failed"));
        assert_eq!(
            error.downcast_ref::<Rejected>().unwrap().0,
            "cleanup failed"
        );
        let missing =
            read_response(&mut Cursor::new(response(10, false, None, None)), 10).unwrap_err();
        assert!(missing.downcast_ref::<Rejected>().is_some());
        for frame in [
            response(10, false, None, None),
            response(10, true, None, None),
            response(10, true, Some(&json!({"shutdown":true})), Some("failed")),
        ] {
            assert!(read_response(&mut Cursor::new(frame), 10).is_err());
        }
    }

    #[test]
    fn malformed_headers_versions_and_truncation_are_rejected() {
        let valid = response(10, true, Some(&json!({"shutdown":true})), None);
        for (offset, bytes) in [
            (0, b"BKPA".to_vec()),
            (4, 11_u32.to_le_bytes().to_vec()),
            (8, vec![1]),
            (8, vec![3]),
            (9, vec![1]),
            (12, 8193_u32.to_le_bytes().to_vec()),
            (16, 8_u64.to_le_bytes().to_vec()),
        ] {
            let mut frame = valid.clone();
            frame[offset..offset + bytes.len()].copy_from_slice(&bytes);
            assert!(read_response(&mut Cursor::new(frame), 10).is_err());
        }
        let mut wrong_payload_version = response(11, true, Some(&json!({"shutdown":true})), None);
        wrong_payload_version[4..8].copy_from_slice(&10_u32.to_le_bytes());
        assert!(read_response(&mut Cursor::new(wrong_payload_version), 10).is_err());
        for length in 0..valid.len() {
            assert!(read_response(&mut Cursor::new(&valid[..length]), 10).is_err());
        }
        let mut malformed = valid;
        malformed[HEADER_LEN..HEADER_LEN + 4].fill(0xff);
        assert!(read_response(&mut Cursor::new(malformed), 10).is_err());
    }

    #[test]
    fn scenes_and_aliased_metadata_expansion_are_rejected() {
        let mut builder = Builder::new();
        let value =
            value::write_value(&mut builder, &json!({}), &mut Budget::new(MAX_PAYLOAD), 0).unwrap();
        let pane = g::PanePayload::create(&mut builder, value, None::<planus::Offset<[u8]>>);
        let payload = g::Payload::create_pane_payload(&mut builder, pane);
        let result = g::OptionalPayload::create(&mut builder, payload);
        let root = g::Response::create(
            &mut builder,
            10,
            true,
            Some(result),
            None::<planus::Offset<str>>,
            None::<planus::Offset<g::OptionalFailure>>,
        );
        let scene = frame(10, 2, builder.finish(root, None)).unwrap();
        assert!(read_response(&mut Cursor::new(scene), 10).is_err());

        let mut builder = Builder::new();
        let text = builder.create_string("x".repeat(1024));
        let string = g::StringValue::create(&mut builder, text);
        let body = g::ValueBody::create_string_value(&mut builder, string);
        let value = g::Value::create(&mut builder, body);
        let values = builder.create_vector([value; 16]);
        let array = g::ArrayValue::create(&mut builder, values);
        let body = g::ValueBody::create_array_value(&mut builder, array);
        let value = g::Value::create(&mut builder, body);
        let control = g::ControlPayload::create(&mut builder, value);
        let payload = g::Payload::create_control_payload(&mut builder, control);
        let result = g::OptionalPayload::create(&mut builder, payload);
        let root = g::Response::create(
            &mut builder,
            10,
            true,
            Some(result),
            None::<planus::Offset<str>>,
            None::<planus::Offset<g::OptionalFailure>>,
        );
        let expanded = frame(10, 2, builder.finish(root, None)).unwrap();
        assert!(expanded.len() < MAX_PAYLOAD);
        assert!(
            read_response(&mut Cursor::new(expanded), 10)
                .unwrap_err()
                .to_string()
                .contains("aggregate allocation")
        );
    }
}
