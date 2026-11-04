use anyhow::{Context, Result, bail};
use bkpr_protocol::{
    ClientAuthority, Operation, PROTOCOL_VERSION, Payload, Request, RequestFailure, RequestOutcome,
    Response,
    wire::{self, Codec, Kind},
};
use serde_json::Value;
use std::{io::Write, os::unix::net::UnixStream, path::Path, time::Duration};

#[path = "client_lifetime.rs"]
pub(crate) mod client_lifetime;
pub use client_lifetime::{Endpoint, RegisteredClient, detached_before_dispatch};

#[derive(Debug)]
struct OutcomeUnknown;
impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("request outcome unknown; do not replay")
    }
}
impl std::error::Error for OutcomeUnknown {}

pub fn outcome_unknown(error: &anyhow::Error) -> bool {
    error.is::<OutcomeUnknown>()
}

pub(crate) fn mark_outcome_unknown(error: anyhow::Error) -> anyhow::Error {
    error.context(OutcomeUnknown)
}

#[derive(Debug)]
struct SessionClosing(RequestOutcome);
impl std::fmt::Display for SessionClosing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("session is shutting down")
    }
}
impl std::error::Error for SessionClosing {}

#[derive(Debug)]
struct InterruptedObservation;
impl std::fmt::Display for InterruptedObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("observational request interrupted")
    }
}
impl std::error::Error for InterruptedObservation {}

pub(crate) fn shutdown_not_dispatched(error: &anyhow::Error) -> bool {
    !outcome_unknown(error)
        && (detached_before_dispatch(error)
            || matches!(
                error.downcast_ref::<SessionClosing>(),
                Some(SessionClosing(RequestOutcome::NotDispatched))
            ))
}

pub(crate) fn server_closing(error: &anyhow::Error) -> bool {
    error.is::<SessionClosing>()
}

pub(crate) fn expected_shutdown(error: &anyhow::Error, session_ended: bool) -> bool {
    shutdown_not_dispatched(error)
        || (error.is::<InterruptedObservation>() && (session_ended || error.is::<SessionClosing>()))
}

fn observation_error(error: anyhow::Error) -> anyhow::Error {
    let interrupted = error.is::<SessionClosing>()
        || error.chain().any(|cause| {
            cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::NotFound
                )
            })
        });
    if interrupted {
        error.context(InterruptedObservation)
    } else {
        error
    }
}

/// Every request is sent once. A disconnected mutation is never replayed.
pub fn request(socket: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    request_stream(&mut stream, request)
}

/// A caller-owned connection allows cancellation by shutting down a cloned socket.
/// Configure read/write deadlines before dispatch; no request is replayed.
pub fn request_stream(stream: &mut UnixStream, request: &Request) -> Result<Response> {
    let mut codec = Codec::new();
    let payload = codec.encode_request(request).context(OutcomeUnknown)?;
    request_encoded_stream(stream, payload)
}

fn request_client_stream(
    stream: &mut UnixStream,
    authority: &ClientAuthority,
    operation: &Operation,
) -> Result<Response> {
    let mut codec = Codec::new();
    let payload = codec
        .encode_client_request(authority, operation)
        .context(OutcomeUnknown)?;
    request_encoded_stream(stream, payload)
}

fn request_encoded_stream(stream: &mut UnixStream, payload: &[u8]) -> Result<Response> {
    // A missing or invalid reply cannot establish whether a mutation executed.
    // Keep that uncertainty typed through caller error contexts.
    let response = (|| -> Result<Response> {
        {
            let mut writer = std::io::BufWriter::new(&mut *stream);
            wire::write_frame(&mut writer, Kind::Request, payload)?;
            writer.flush()?;
        }
        let bytes = wire::read_frame(stream, Kind::Response, wire::MAX_RESPONSE)?;
        validate_response(wire::decode_response_owned(bytes).context("decoding server response")?)
    })();
    response.context(OutcomeUnknown)
}
pub fn checked(socket: &Path, operation: Operation) -> Result<Value> {
    checked_payload(socket, operation)?.into_json()
}

/// Preserve native pane scene bytes for callers that do not need diagnostic JSON.
pub fn checked_payload(socket: &Path, operation: Operation) -> Result<Payload> {
    checked_payload_response(request(socket, &Request::new(operation))?)
}

/// Dispatch once on a connection whose lifetime and deadlines the caller owns.
pub fn checked_stream(stream: &mut UnixStream, operation: Operation) -> Result<Value> {
    checked_response(request_stream(stream, &Request::new(operation))?)
}

fn validate_response(response: Response) -> Result<Response> {
    if response.version != PROTOCOL_VERSION {
        bail!("unsupported server protocol version");
    }
    match (
        response.ok,
        response.result.is_some(),
        response.error.is_some(),
    ) {
        (true, true, false) if response.failure.is_none() => Ok(response),
        (false, false, true) => Ok(response),
        _ => bail!("incomplete or contradictory server response"),
    }
}

fn checked_response(response: Response) -> Result<Value> {
    checked_payload_response(response)?.into_json()
}

fn checked_payload_response(response: Response) -> Result<Payload> {
    if !response.ok {
        let message = response.error.expect("validated rejection has an error");
        let ambiguous = message.contains("outcome unknown");
        let error = match response.failure {
            Some(RequestFailure::SessionClosing { outcome }) => {
                let unknown = outcome == RequestOutcome::Unknown;
                let error = anyhow::Error::msg(message).context(SessionClosing(outcome));
                return Err(if unknown {
                    error.context(OutcomeUnknown)
                } else {
                    error
                });
            }
            None => anyhow::Error::msg(message),
        };
        return Err(if ambiguous {
            error.context(OutcomeUnknown)
        } else {
            error
        });
    }
    response
        .result
        .context("successful response omitted result")
}

#[cfg(test)]
pub(crate) mod test_wire {
    use super::*;
    use std::io::Read;

    pub(crate) fn read_request(stream: &mut impl Read) -> Result<Request> {
        wire::decode_request(&wire::read_frame(stream, Kind::Request, wire::MAX_REQUEST)?)
    }

    pub(crate) fn write_response(stream: &mut impl Write, response: &Response) -> Result<()> {
        let mut codec = Codec::new();
        wire::write_frame(stream, Kind::Response, codec.encode_response(response)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixListener, thread};

    fn reply(response: Response) -> Vec<u8> {
        let mut codec = Codec::new();
        wire::encode_frame(Kind::Response, codec.encode_response(&response).unwrap()).unwrap()
    }

    fn rejected_reply(reply: Vec<u8>) -> anyhow::Error {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let bytes = wire::read_frame(&mut stream, Kind::Request, wire::MAX_REQUEST).unwrap();
            // The fixture has accepted a real mutation before losing/corrupting
            // its reply; the client cannot infer that it is safe to replay.
            assert!(matches!(
                wire::decode_request(&bytes).unwrap().operation,
                Operation::Shutdown
            ));
            stream.write_all(&reply).unwrap();
        });
        let error = checked(&socket, Operation::Shutdown).unwrap_err();
        server.join().unwrap();
        error
    }

    #[test]
    fn lost_or_invalid_mutation_replies_keep_unknown_outcome_through_context() {
        let success = Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(serde_json::json!({}).into()),
            error: None,
            failure: None,
        };
        let mut truncated = reply(success.clone());
        truncated.pop();
        let mut wrong_family = reply(success.clone());
        wrong_family[4..8].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        let cases = [
            Vec::new(),
            wire::encode_frame(Kind::Response, &[0]).unwrap(),
            wire::encode_frame(Kind::Request, &[0]).unwrap(),
            truncated,
            wrong_family,
            reply(Response {
                result: None,
                ..success.clone()
            }),
            reply(Response {
                error: Some("contradiction".into()),
                ..success
            }),
        ];
        for reply in cases {
            let error = rejected_reply(reply).context("dispatching the user's command");
            assert!(outcome_unknown(&error), "{error:#}");
        }
    }

    #[test]
    fn explicit_rejection_and_native_timeout_have_distinct_outcomes() {
        for (message, unknown) in [
            ("only current size owner may seed terminal colors", false),
            (
                "pane command timed out; outcome unknown, do not retry input",
                true,
            ),
        ] {
            let reply = reply(Response {
                version: PROTOCOL_VERSION,
                ok: false,
                result: None,
                error: Some(message.into()),
                failure: None,
            });
            let error = rejected_reply(reply);
            assert_eq!(outcome_unknown(&error), unknown, "{error:#}");
        }
    }

    #[test]
    fn connection_failure_is_known_not_dispatched() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            checked(&directory.path().join("absent.sock"), Operation::Shutdown).unwrap_err();
        assert!(!outcome_unknown(&error));
    }
    #[test]
    fn typed_shutdown_rejection_does_not_hide_unknown_or_untyped_failures() {
        for (failure, unknown, clean) in [
            (None, false, false),
            (
                Some(RequestFailure::SessionClosing {
                    outcome: RequestOutcome::NotDispatched,
                }),
                false,
                true,
            ),
            (
                Some(RequestFailure::SessionClosing {
                    outcome: RequestOutcome::Unknown,
                }),
                true,
                false,
            ),
        ] {
            let error = rejected_reply(reply(Response {
                version: PROTOCOL_VERSION,
                ok: false,
                result: None,
                error: Some("session is shutting down".into()),
                failure,
            }));
            assert_eq!(outcome_unknown(&error), unknown);
            assert_eq!(expected_shutdown(&error, true), clean);
        }
        let error = rejected_reply(reply(Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(serde_json::json!({"committed":true}).into()),
            error: None,
            failure: Some(RequestFailure::SessionClosing {
                outcome: RequestOutcome::NotDispatched,
            }),
        }));
        assert!(outcome_unknown(&error));
        assert!(!expected_shutdown(&error, true));
    }
}
