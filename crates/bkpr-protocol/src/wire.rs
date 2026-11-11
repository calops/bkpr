//! Native wire-family 8 payload codecs and framing, separate from CLI/MCP/persistence JSON.
//!
//! Encoders borrow reusable storage; decoders enforce bounded traversal and owned-data budgets.
//! Framing validates admission headers, not dispatch, effect completion or replay safety.
mod agents;
mod framing;
#[rustfmt::skip]
#[allow(dead_code, unused_imports, clippy::all)]
mod generated;
mod operations;
mod pane;
mod paths;
#[cfg(test)]
mod tests;
mod types;
mod value;

use crate::{
    ClientAuthority, ClientEvent, Operation, PROTOCOL_VERSION, PanePayload, Payload, Request,
    Response,
};
use anyhow::{Result, ensure};
/// Native frame admission, payload limits and owned-frame helpers.
pub use framing::*;
use generated::native_wire as g;
use planus::{Builder, ReadAsRoot};

/// Bounds lazy Planus traversal, including repeated/aliased offsets. The wire is
/// at most `max` bytes, but an owned Rust tree can require more than its compact
/// representation. A separate aggregate allocation/work budget prevents a small
/// malicious DAG from expanding without bound. Charge before reserving/copying.
struct Budget<'a> {
    remaining: usize,
    nodes: usize,
    max: usize,
    charge: Option<&'a mut dyn FnMut(usize) -> Result<()>>,
}
impl<'a> Budget<'a> {
    fn new(max: usize) -> Self {
        Self {
            remaining: max.saturating_mul(4),
            nodes: max / 4,
            max,
            charge: None,
        }
    }
    fn bytes(&mut self, bytes: usize) -> Result<()> {
        ensure!(bytes <= self.max, "native allocation exceeds message limit");
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| anyhow::anyhow!("native aggregate allocation limit"))?;
        if let Some(charge) = &mut self.charge {
            charge(bytes)?;
        }
        Ok(())
    }
    fn vector(&mut self, length: usize, element: usize) -> Result<()> {
        ensure!(length <= self.nodes, "native aggregate element limit");
        self.bytes(
            length
                .checked_mul(element)
                .ok_or_else(|| anyhow::anyhow!("native vector size overflow"))?,
        )
    }
    fn node(&mut self, depth: usize) -> Result<()> {
        ensure!(depth <= 128, "native nesting depth limit");
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("native aggregate node limit"))?;
        self.bytes(32)
    }
}

/// Reusable native payload encoder; returned bytes borrow its builder until the next mutable call.
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

    /// Encode an unframed request, rejecting unsupported versions or exceeded codec limits.
    pub fn encode_request(&mut self, request: &Request) -> Result<&[u8]> {
        ensure!(
            request.version == PROTOCOL_VERSION,
            "unsupported native request version"
        );
        self.builder.clear();
        let mut budget = Budget::new(MAX_REQUEST);
        let operation =
            operations::write_operation(&mut self.builder, &request.operation, &mut budget, 0)?;
        let root = g::Request::create(&mut self.builder, request.version, operation);
        let bytes = self.builder.finish(root, None);
        ensure!(bytes.len() <= MAX_REQUEST, "native request size limit");
        Ok(bytes)
    }

    /// Encode an authenticated client operation from borrowed authority and operation fields.
    /// Uses the current native version and rejects exceeded codec limits.
    pub fn encode_client_request(
        &mut self,
        authority: &ClientAuthority,
        request: &Operation,
    ) -> Result<&[u8]> {
        self.builder.clear();
        let mut budget = Budget::new(MAX_REQUEST);
        let authority =
            types::write_client_authority(&mut self.builder, authority, &mut budget, 1)?;
        let operation = operations::write_operation(&mut self.builder, request, &mut budget, 1)?;
        let client = g::OperationClient::create(&mut self.builder, authority, operation);
        let operation = g::Operation::create_operation_client(&mut self.builder, client);
        let root = g::Request::create(&mut self.builder, PROTOCOL_VERSION, operation);
        let bytes = self.builder.finish(root, None);
        ensure!(bytes.len() <= MAX_REQUEST, "native request size limit");
        Ok(bytes)
    }

    /// Encode an unframed response, rejecting unsupported versions or exceeded codec limits.
    /// Nested scene bytes remain opaque to this encoder.
    pub fn encode_response(&mut self, response: &Response) -> Result<&[u8]> {
        ensure!(
            response.version == PROTOCOL_VERSION,
            "unsupported native response version"
        );
        self.builder.clear();
        let mut budget = Budget::new(MAX_RESPONSE);
        let result = response
            .result
            .as_ref()
            .map(|payload| -> Result<_> {
                let value = match payload {
                    Payload::Control(metadata) => {
                        let metadata =
                            value::write_value(&mut self.builder, metadata, &mut budget, 0)?;
                        let payload = g::ControlPayload::create(&mut self.builder, metadata);
                        g::Payload::create_control_payload(&mut self.builder, payload)
                    }
                    Payload::Pane(payload) => {
                        let metadata = value::write_value(
                            &mut self.builder,
                            &payload.metadata,
                            &mut budget,
                            0,
                        )?;
                        let scene = payload
                            .scene
                            .as_ref()
                            .map(|scene| -> Result<_> {
                                ensure!(scene.len() <= MAX_RESPONSE, "native scene size limit");
                                budget.bytes(scene.len())?;
                                Ok(self.builder.create_vector(scene.as_ref()))
                            })
                            .transpose()?;
                        let payload = g::PanePayload::create(&mut self.builder, metadata, scene);
                        g::Payload::create_pane_payload(&mut self.builder, payload)
                    }
                };
                Ok(g::OptionalPayload::create(&mut self.builder, value))
            })
            .transpose()?;
        let error = response
            .error
            .as_ref()
            .map(|error| -> Result<_> {
                budget.bytes(error.len())?;
                Ok(self.builder.create_string(error))
            })
            .transpose()?;
        let failure = response
            .failure
            .as_ref()
            .map(|failure| -> Result<_> {
                let value =
                    operations::write_request_failure(&mut self.builder, failure, &mut budget, 0)?;
                Ok(g::OptionalFailure::create(&mut self.builder, value))
            })
            .transpose()?;
        let root = g::Response::create(
            &mut self.builder,
            response.version,
            response.ok,
            result,
            error,
            failure,
        );
        let bytes = self.builder.finish(root, None);
        ensure!(bytes.len() <= MAX_RESPONSE, "native response size limit");
        Ok(bytes)
    }

    /// Encode an unframed client event, rejecting exceeded codec limits.
    pub fn encode_client_event(&mut self, event: &ClientEvent) -> Result<&[u8]> {
        self.builder.clear();
        let root = types::write_client_event(
            &mut self.builder,
            event,
            &mut Budget::new(MAX_CLIENT_EVENT),
            0,
        )?;
        let bytes = self.builder.finish(root, None);
        ensure!(
            bytes.len() <= MAX_CLIENT_EVENT,
            "native client event size limit"
        );
        Ok(bytes)
    }
}

/// Decode an unframed request into owned fields with version, traversal and allocation limits.
pub fn decode_request(bytes: &[u8]) -> Result<Request> {
    decode_request_inner(bytes, None)
}

/// Decode a request while charging owned-data budget bytes before allocation or copying.
/// Callback errors abort decoding; earlier charges are not refunded. Native limits still apply.
pub fn decode_request_charged(
    bytes: &[u8],
    charge: &mut dyn FnMut(usize) -> Result<()>,
) -> Result<Request> {
    decode_request_inner(bytes, Some(charge))
}

fn decode_request_inner(
    bytes: &[u8],
    charge: Option<&mut dyn FnMut(usize) -> Result<()>>,
) -> Result<Request> {
    ensure!(bytes.len() <= MAX_REQUEST, "native request size limit");
    let root = g::RequestRef::read_as_root(bytes)?;
    let version = root.version()?;
    ensure!(
        version == PROTOCOL_VERSION,
        "unsupported native request version {version}"
    );
    let mut budget = Budget::new(MAX_REQUEST);
    budget.charge = charge;
    let operation = operations::read_operation(root.operation()?, &mut budget, 0)?;
    Ok(Request { version, operation })
}
/// Decode an unframed response with version and codec limits, copying any nested scene bytes.
/// Scene contents require separate [`crate::scene_wire::decode`] validation.
pub fn decode_response(bytes: &[u8]) -> Result<Response> {
    decode_response_inner(bytes, None)
}

/// Decode an owned unframed response, retaining nested scene slices in its receive allocation.
/// Enforces version and codec limits but does not validate nested scene contents.
pub fn decode_response_owned(bytes: Vec<u8>) -> Result<Response> {
    let owner = bytes::Bytes::from(bytes);
    decode_response_inner(&owner, Some(&owner))
}

fn decode_response_inner(bytes: &[u8], owner: Option<&bytes::Bytes>) -> Result<Response> {
    ensure!(bytes.len() <= MAX_RESPONSE, "native response size limit");
    let root = g::ResponseRef::read_as_root(bytes)?;
    let version = root.version()?;
    ensure!(
        version == PROTOCOL_VERSION,
        "unsupported native response version {version}"
    );
    let mut budget = Budget::new(MAX_RESPONSE);
    let result = root
        .result()?
        .map(|payload| -> Result<_> {
            Ok(match payload.value()? {
                g::PayloadRef::ControlPayload(payload) => {
                    Payload::Control(value::read_value(payload.value()?, &mut budget, 0)?)
                }
                g::PayloadRef::PanePayload(payload) => {
                    let metadata = value::read_value(payload.metadata()?, &mut budget, 0)?;
                    let scene = payload
                        .scene()?
                        .map(|scene| -> Result<_> {
                            budget.bytes(scene.len())?;
                            match owner {
                                Some(owner) => {
                                    let start = (scene.as_ptr() as usize)
                                        .checked_sub(owner.as_ptr() as usize)
                                        .ok_or_else(|| {
                                            anyhow::anyhow!(
                                                "native scene precedes response allocation"
                                            )
                                        })?;
                                    let end = start.checked_add(scene.len()).ok_or_else(|| {
                                        anyhow::anyhow!("native scene range overflow")
                                    })?;
                                    ensure!(
                                        end <= owner.len(),
                                        "native scene outside response allocation"
                                    );
                                    Ok(owner.slice(start..end))
                                }
                                None => Ok(bytes::Bytes::copy_from_slice(scene)),
                            }
                        })
                        .transpose()?;
                    Payload::Pane(PanePayload { metadata, scene })
                }
            })
        })
        .transpose()?;
    let error = root
        .error()?
        .map(|error| -> Result<_> {
            budget.bytes(error.len())?;
            Ok(error.to_owned())
        })
        .transpose()?;
    let failure = root
        .failure()?
        .map(|failure| operations::read_request_failure(failure.value()?, &mut budget, 0))
        .transpose()?;
    Ok(Response {
        version,
        ok: root.ok()?,
        result,
        error,
        failure,
    })
}
/// Decode an unframed client event into owned fields with traversal and allocation limits.
pub fn decode_client_event(bytes: &[u8]) -> Result<ClientEvent> {
    ensure!(
        bytes.len() <= MAX_CLIENT_EVENT,
        "native client event size limit"
    );
    types::read_client_event(
        g::ClientEventRef::read_as_root(bytes)?,
        &mut Budget::new(MAX_CLIENT_EVENT),
        0,
    )
}
