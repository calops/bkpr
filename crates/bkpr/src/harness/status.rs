//! Launch-local, process-authenticated volatile semantic-status stream.
use super::{Capture, bind_arguments, sanitize_request_error};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::wire::{self, Codec, Kind};
use bkpr_protocol::{CaptureEvent, ConversationSource, NativeStatus, Operation, Request};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, BufRead, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Duration,
};

const MAX_INPUT: usize = 512;
const MAX_REPLY: usize = 4096;

/// Stream ordered native status reports for one authenticated conversation binding.
pub(super) fn cli(arguments: Vec<OsString>) -> Result<i32> {
    let (source, conversation_id) = bind_arguments(arguments)?;
    let capture = Capture::from_environment()?;
    ensure!(
        source.harness() == capture.grant.kind,
        "capture source does not match harness"
    );
    let socket = UnixStream::connect(&capture.socket).context("connect native status outlet")?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut input = CancellableInput {
        socket: socket.try_clone()?,
        bytes: [0; MAX_INPUT],
        start: 0,
        end: 0,
    };
    run(
        &capture,
        source,
        &conversation_id,
        socket,
        &mut input,
        &mut io::stdout().lock(),
    )
    .map_err(|error| sanitize_request_error(error, &capture.grant.token))?;
    Ok(0)
}

fn run(
    capture: &Capture,
    source: ConversationSource,
    conversation_id: &str,
    mut socket: UnixStream,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let mut codec = Codec::new();
    let mut exchange = |event| -> Result<Value> {
        let request = Request::new(Operation::HarnessEvent {
            launch_id: capture.grant.launch_id.clone(),
            token: capture.grant.token.clone(),
            event,
        });
        wire::write_frame(&mut socket, Kind::Request, codec.encode_request(&request)?)?;
        let bytes = wire::read_frame(&mut socket, Kind::Response, MAX_REPLY)?;
        let response = wire::decode_response(&bytes)?;
        ensure!(
            response.ok,
            "{}",
            response
                .error
                .as_deref()
                .unwrap_or("native status request rejected")
        );
        response
            .result
            .context("native status response missing result")?
            .into_json()
    };
    let handshake = exchange(CaptureEvent::StatusEnroll {
        conversation_id: conversation_id.into(),
        source,
    })?;
    let generation = handshake["generation"]
        .as_u64()
        .filter(|generation| *generation > 0)
        .context("native status enrollment returned no generation")?;
    write_line(output, &json!({"generation":generation}))?;
    let mut line = Vec::with_capacity(MAX_INPUT);
    while read_line(input, &mut line)? {
        let status: NativeStatus =
            serde_json::from_slice(&line).context("invalid native status payload")?;
        status.validate()?;
        let response = exchange(CaptureEvent::StatusReport {
            conversation_id: conversation_id.into(),
            source,
            generation,
            status,
        })?;
        ensure!(
            response["accepted"] == true && response["generation"].as_u64() == Some(generation),
            "native status report was not accepted for the enrolled generation"
        );
        write_line(output, &json!({"accepted":true,"generation":generation}))?;
    }
    Ok(())
}

fn write_line(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool> {
    line.clear();
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            ensure!(line.is_empty(), "native status payload requires a newline");
            return Ok(false);
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(buffer.len(), |index| index + 1);
        ensure!(
            count <= MAX_INPUT - line.len(),
            "native status payload exceeds byte limit"
        );
        line.extend_from_slice(&buffer[..count]);
        input.consume(count);
        if newline.is_some() {
            return Ok(true);
        }
    }
}

struct CancellableInput {
    socket: UnixStream,
    bytes: [u8; MAX_INPUT],
    start: usize,
    end: usize,
}
impl BufRead for CancellableInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        loop {
            let buffered = self.start < self.end;
            let mut descriptors = [
                libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let result =
                unsafe { libc::poll(descriptors.as_mut_ptr(), 2, if buffered { 0 } else { -1 }) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptors[1].revents != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "native status outlet disconnected",
                ));
            }
            if buffered {
                return Ok(&self.bytes[self.start..self.end]);
            }
            if descriptors[0].revents == 0 {
                continue;
            }
            let count = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    self.bytes.as_mut_ptr().cast(),
                    self.bytes.len(),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            self.start = 0;
            self.end = count as usize;
            return Ok(&self.bytes[..self.end]);
        }
    }
    fn consume(&mut self, amount: usize) {
        self.start = self.start.saturating_add(amount).min(self.end);
    }
}
impl Read for CancellableInput {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let buffer = self.fill_buf()?;
        let count = buffer.len().min(output.len());
        output[..count].copy_from_slice(&buffer[..count]);
        self.consume(count);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn capture() -> Capture {
        Capture {
            grant: bkpr_protocol::CaptureGrant {
                launch_id: "launch".into(),
                token: "private".into(),
                pane_id: "pane".into(),
                kind: bkpr_protocol::HarnessKind::Opencode,
            },
            program: "/unused".into(),
            socket: "/unused".into(),
            revoke_on_drop: false,
            ended: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[test]
    fn ndjson_admission_is_bounded_and_requires_complete_records() {
        let mut line = Vec::new();
        let mut input = Cursor::new(b"{\"state\":\"working\"}\n{\"state\":\"clear\"}\n");
        assert!(read_line(&mut input, &mut line).unwrap());
        assert_eq!(
            serde_json::from_slice::<NativeStatus>(&line)
                .unwrap()
                .state
                .as_str(),
            "working"
        );
        assert!(read_line(&mut input, &mut line).unwrap());
        assert_eq!(
            serde_json::from_slice::<NativeStatus>(&line)
                .unwrap()
                .state
                .as_str(),
            "clear"
        );
        assert!(!read_line(&mut input, &mut line).unwrap());
        assert!(read_line(&mut Cursor::new(vec![b' '; MAX_INPUT + 1]), &mut line).is_err());
        assert!(read_line(&mut Cursor::new(b"{\"state\":\"idle\"}"), &mut line).is_err());
    }

    #[test]
    fn failed_publication_never_produces_a_cli_report_ack() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut codec = Codec::new();
            for result in [Some(json!({"generation":7})), None] {
                let bytes = wire::read_frame(&mut server, Kind::Request, 4096).unwrap();
                let request = wire::decode_request(&bytes).unwrap();
                assert!(matches!(request.operation, Operation::HarnessEvent { .. }));
                let ok = result.is_some();
                let reply = bkpr_protocol::Response {
                    version: bkpr_protocol::PROTOCOL_VERSION,
                    ok,
                    result: result.map(bkpr_protocol::Payload::Control),
                    error: Some("publication failed".into()).filter(|_| !ok),
                    failure: None,
                };
                wire::write_frame(
                    &mut server,
                    Kind::Response,
                    codec.encode_response(&reply).unwrap(),
                )
                .unwrap();
            }
        });
        let capture = capture();
        let mut output = Vec::new();
        assert!(
            run(
                &capture,
                ConversationSource::OpencodePlugin,
                "session",
                client,
                &mut Cursor::new(b"{\"state\":\"working\"}\n"),
                &mut output
            )
            .is_err()
        );
        assert_eq!(output, b"{\"generation\":7}\n");
        worker.join().unwrap();
    }

    #[test]
    fn one_socket_preserves_frozen_generation_and_ordered_report_acknowledgements() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut codec = Codec::new();
            for expected in [None, Some("working"), Some("blocked"), Some("clear")] {
                let bytes = wire::read_frame(&mut server, Kind::Request, 4096).unwrap();
                let request = wire::decode_request(&bytes).unwrap();
                let Operation::HarnessEvent { event, .. } = request.operation else {
                    unreachable!()
                };
                let result = match (expected, event) {
                    (
                        None,
                        CaptureEvent::StatusEnroll {
                            conversation_id,
                            source,
                        },
                    ) => {
                        assert_eq!(conversation_id, "session");
                        assert_eq!(source, ConversationSource::OpencodePlugin);
                        json!({"generation":7})
                    }
                    (
                        Some(expected),
                        CaptureEvent::StatusReport {
                            conversation_id,
                            source,
                            generation,
                            status,
                        },
                    ) => {
                        assert_eq!(conversation_id, "session");
                        assert_eq!(source, ConversationSource::OpencodePlugin);
                        assert_eq!(generation, 7);
                        assert_eq!(status.state.as_str(), expected);
                        json!({"accepted":true,"generation":7})
                    }
                    _ => panic!("unexpected native status stream request"),
                };
                let reply = bkpr_protocol::Response {
                    version: bkpr_protocol::PROTOCOL_VERSION,
                    ok: true,
                    result: Some(bkpr_protocol::Payload::Control(result)),
                    error: None,
                    failure: None,
                };
                wire::write_frame(
                    &mut server,
                    Kind::Response,
                    codec.encode_response(&reply).unwrap(),
                )
                .unwrap();
            }
        });
        let mut output = Vec::new();
        run(&capture(), ConversationSource::OpencodePlugin, "session", client,
            &mut Cursor::new(b"{\"state\":\"working\"}\n{\"state\":\"blocked\",\"kind\":\"question\"}\n{\"state\":\"clear\"}\n"),
            &mut output).unwrap();
        let responses = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            responses,
            vec![
                json!({"generation":7}),
                json!({"accepted":true,"generation":7}),
                json!({"accepted":true,"generation":7}),
                json!({"accepted":true,"generation":7}),
            ]
        );
        worker.join().unwrap();
    }

    #[test]
    fn outlet_disconnection_cancels_even_buffered_or_idle_stdin() {
        for end in [0, 1] {
            let (socket, server) = UnixStream::pair().unwrap();
            drop(server);
            let mut input = CancellableInput {
                socket,
                bytes: [b' '; MAX_INPUT],
                start: 0,
                end,
            };
            assert!(input.fill_buf().is_err());
        }
    }
}
