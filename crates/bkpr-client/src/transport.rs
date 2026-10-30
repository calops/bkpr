use anyhow::{Context, Result, bail};
use bkpr_protocol::{Operation, PROTOCOL_VERSION, Request, Response};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

#[derive(Debug)]
struct OutcomeUnknown;
impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("request outcome unknown; do not replay")
    }
}
impl std::error::Error for OutcomeUnknown {}

pub(crate) fn outcome_unknown(error: &anyhow::Error) -> bool {
    error.is::<OutcomeUnknown>()
}

pub(crate) fn mark_outcome_unknown(error: anyhow::Error) -> anyhow::Error {
    error.context(OutcomeUnknown)
}

/// Every request is sent once. A disconnected mutation is never replayed.
pub fn request(socket: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Once writing starts, even an incomplete/invalid reply cannot prove whether
    // a mutation executed. Keep that fact typed through all caller contexts.
    let response = (|| -> Result<Response> {
        {
            let mut writer = std::io::BufWriter::new(&mut stream);
            serde_json::to_writer(&mut writer, request)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        let mut bytes = Vec::new();
        BufReader::new(stream.take(96 * 1024 * 1024 + 1)).read_until(b'\n', &mut bytes)?;
        if bytes.len() > 96 * 1024 * 1024 {
            bail!("server response exceeds scene limit");
        }
        let response: Response =
            serde_json::from_slice(&bytes).context("decoding server response")?;
        if response.version != PROTOCOL_VERSION {
            bail!("unsupported server protocol version");
        }
        match (
            response.ok,
            response.result.is_some(),
            response.error.is_some(),
        ) {
            (true, true, false) | (false, false, true) => Ok(response),
            _ => bail!("incomplete or contradictory server response"),
        }
    })();
    response.context(OutcomeUnknown)
}
pub fn checked(socket: &Path, operation: Operation) -> Result<Value> {
    let response = request(socket, &Request::new(operation))?;
    if !response.ok {
        let message = response.error.expect("validated rejection has an error");
        let ambiguous = message.contains("outcome unknown");
        let error = anyhow::Error::msg(message);
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
mod tests {
    use super::*;
    use std::{os::unix::net::UnixListener, thread};

    fn rejected_reply(reply: Vec<u8>) -> anyhow::Error {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            BufReader::new(&mut stream)
                .read_until(b'\n', &mut request)
                .unwrap();
            // The fixture has accepted a real mutation before losing/corrupting
            // its reply; the client cannot infer that it is safe to replay.
            let _: Request = serde_json::from_slice(&request).unwrap();
            stream.write_all(&reply).unwrap();
        });
        let error = checked(&socket, Operation::Shutdown).unwrap_err();
        server.join().unwrap();
        error
    }

    #[test]
    fn lost_or_invalid_mutation_replies_keep_unknown_outcome_through_context() {
        let cases = [
            Vec::new(),
            br#"{"version":2,"ok":true,"result":"#.to_vec(),
            format!(r#"{{"version":{},"ok":true,"result":{{}}}}"#, PROTOCOL_VERSION + 1).into_bytes(),
            format!(r#"{{"version":{PROTOCOL_VERSION},"ok":true}}"#).into_bytes(),
            format!(r#"{{"version":{PROTOCOL_VERSION},"ok":true,"result":{{}},"error":"contradiction"}}"#).into_bytes(),
            format!(r#"{{"version":{PROTOCOL_VERSION},"ok":false,"error":"rejected","result":{{}}}}"#).into_bytes(),
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
            let reply = serde_json::to_vec(&serde_json::json!({
                "version": PROTOCOL_VERSION, "ok": false, "error": message
            }))
            .unwrap();
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
}
