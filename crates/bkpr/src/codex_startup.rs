//! Observe the TUI's own native API responses, never global thread notifications.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde_json::Value;

const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_SELECTIONS: usize = 32;
const MAX_CONNECTIONS: usize = 8;
const IO_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Control {
    stopped: AtomicBool,
    streams: Mutex<BTreeMap<u64, (UnixStream, UnixStream)>>,
}

impl Control {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        for (client, server) in self.streams.lock().values() {
            let _ = client.shutdown(Shutdown::Both);
            let _ = server.shutdown(Shutdown::Both);
        }
    }

    fn close(&self, connection: u64) {
        if let Some((client, server)) = self.streams.lock().remove(&connection) {
            let _ = client.shutdown(Shutdown::Both);
            let _ = server.shutdown(Shutdown::Both);
        }
    }
}

/// The native endpoint remains owned by the app-server after this TUI exits.
pub(super) fn supervise(
    mut tui: Child,
    listener: UnixListener,
    native_socket: &Path,
    selected: impl FnMut(&str, &str) -> Result<()> + Send,
) -> Result<ExitStatus> {
    let control = Control::default();
    thread::scope(|scope| {
        let control = &control;
        let (send, receive) = mpsc::sync_channel(1);
        let worker = scope.spawn(move || {
            let result = tap(listener, native_socket, control, selected);
            control.stop();
            let _ = send.send(result);
        });
        let result = (|| {
            let mut disconnected = None;
            loop {
                if let Some(status) = tui.try_wait().context("wait for Codex TUI")? {
                    return Ok(status);
                }
                match receive.try_recv() {
                    Ok(Err(error)) => return Err(error),
                    Ok(Ok(())) => disconnected = Some(Instant::now()),
                    Err(mpsc::TryRecvError::Disconnected) if disconnected.is_none() => {
                        bail!("Codex native binding worker stopped unexpectedly")
                    }
                    _ => {}
                }
                if disconnected.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
                    bail!("Codex TUI did not exit after its native connection closed");
                }
                thread::sleep(Duration::from_millis(25));
            }
        })();
        control.stop();
        if result.is_err() {
            let _ = tui.kill();
            let _ = tui.wait();
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Codex native binding worker panicked"))?;
        result
    })
}

fn tap(
    listener: UnixListener,
    native_socket: &Path,
    control: &Control,
    selected: impl FnMut(&str, &str) -> Result<()> + Send,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let selections = Mutex::new(Selections::default());
    let selected = Mutex::new(selected);
    thread::scope(|scope| {
        let mut connections: Vec<thread::ScopedJoinHandle<'_, Result<()>>> = Vec::new();
        let mut sequence = 0_u64;
        let result = (|| {
            while !control.stopped.load(Ordering::Acquire) {
                let mut index = 0;
                while index < connections.len() {
                    if connections[index].is_finished() {
                        connections.swap_remove(index).join().map_err(|_| {
                            anyhow::anyhow!("Codex native connection worker panicked")
                        })??;
                    } else {
                        index += 1;
                    }
                }
                match listener.accept() {
                    Ok((client, _)) => {
                        if connections.len() >= MAX_CONNECTIONS {
                            bail!("too many concurrent Codex TUI native connections");
                        }
                        sequence = sequence
                            .checked_add(1)
                            .context("Codex connection sequence exhausted")?;
                        let connection = sequence;
                        let selections = &selections;
                        let selected = &selected;
                        connections.push(scope.spawn(move || {
                            let result = tap_connection(
                                client,
                                native_socket,
                                control,
                                connection,
                                selections,
                                selected,
                            );
                            control.close(connection);
                            selections
                                .lock()
                                .pending
                                .retain(|(owner, _), _| *owner != connection);
                            result
                        }));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => return Err(error).context("accept Codex TUI native connection"),
                }
            }
            Ok(())
        })();
        control.stop();
        for connection in connections {
            let joined = connection
                .join()
                .map_err(|_| anyhow::anyhow!("Codex native connection worker panicked"))?;
            if result.is_ok() {
                joined?;
            }
        }
        result
    })
}

fn tap_connection(
    client: UnixStream,
    native_socket: &Path,
    control: &Control,
    connection: u64,
    selections: &Mutex<Selections>,
    selected: &Mutex<impl FnMut(&str, &str) -> Result<()> + Send>,
) -> Result<()> {
    let server = UnixStream::connect(native_socket).context("connect owned Codex app-server")?;
    for stream in [&client, &server] {
        stream.set_read_timeout(Some(IO_INTERVAL))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    }
    {
        let mut streams = control.streams.lock();
        if control.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        streams.insert(connection, (client.try_clone()?, server.try_clone()?));
    }
    let stopped = AtomicBool::new(false);
    thread::scope(|scope| {
        let mut client_read = client.try_clone()?;
        let mut server_write = server.try_clone()?;
        let stopped = &stopped;
        let upload = scope.spawn(move || {
            let result = forward(
                &mut client_read,
                &mut server_write,
                stopped,
                true,
                |bytes| {
                    let message =
                        serde_json::from_slice(bytes).context("parse Codex native request")?;
                    selections.lock().request_on(connection, &message)
                },
            );
            stopped.store(true, Ordering::Release);
            control.close(connection);
            result
        });
        let mut server_read = server;
        let mut client_write = client;
        let download = forward(
            &mut server_read,
            &mut client_write,
            stopped,
            false,
            |bytes| {
                // Hold through binding: newer outgoing selections cannot race a stale response.
                let mut state = selections.lock();
                if !state.pending.keys().any(|(owner, _)| *owner == connection) {
                    return Ok(());
                }
                let message = serde_json::from_slice(bytes)
                    .context("parse Codex native selection response")?;
                if let Some((method, thread)) = state.response_on(connection, &message)? {
                    selected.lock()(method, &thread)?;
                }
                Ok(())
            },
        );
        stopped.store(true, Ordering::Release);
        control.close(connection);
        let upload = upload
            .join()
            .map_err(|_| anyhow::anyhow!("Codex native upload worker panicked"))?;
        download.and(upload)
    })
}

#[derive(serde::Deserialize)]
struct RequestMetadata<'a> {
    id: Option<Value>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(borrow)]
    params: Option<SelectionParams<'a>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionParams<'a> {
    #[serde(borrow)]
    thread_source: Option<Cow<'a, str>>,
    #[serde(borrow)]
    thread_id: Option<Cow<'a, str>>,
}

#[derive(Default)]
struct Selections {
    sequence: u64,
    pending: BTreeMap<(u64, String), Selection>,
}

struct Selection {
    sequence: u64,
    method: &'static str,
    expected_thread: Option<String>,
}

impl Selections {
    fn request_on(&mut self, connection: u64, message: &RequestMetadata<'_>) -> Result<()> {
        let source = message
            .params
            .as_ref()
            .and_then(|params| params.thread_source.as_deref());
        let method = match message.method.as_deref() {
            Some("thread/start") if source == Some("user") => "thread/start",
            Some("thread/fork") if source == Some("user") => "thread/fork",
            Some("thread/resume") => "thread/resume",
            _ => return Ok(()),
        };
        let id = request_id(message.id.as_ref())
            .context("Codex thread selection has no native request ID")?;
        if self.pending.len() >= MAX_PENDING_SELECTIONS {
            bail!("too many outstanding Codex thread selections");
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Codex selection sequence exhausted")?;
        let expected_thread = if method == "thread/resume" {
            Some(
                message
                    .params
                    .as_ref()
                    .and_then(|params| params.thread_id.as_deref())
                    .context("Codex resume has no thread ID")?
                    .to_owned(),
            )
        } else {
            None
        };
        if self
            .pending
            .insert(
                (connection, id),
                Selection {
                    sequence: self.sequence,
                    method,
                    expected_thread,
                },
            )
            .is_some()
        {
            bail!("Codex reused an outstanding thread selection request ID");
        }
        Ok(())
    }

    fn response_on(
        &mut self,
        connection: u64,
        message: &Value,
    ) -> Result<Option<(&'static str, String)>> {
        // JSON-RPC request and response IDs occupy separate namespaces.
        if message.get("method").is_some() {
            return Ok(None);
        }
        let Some(id) = request_id(message.get("id")) else {
            return Ok(None);
        };
        let Some(selection) = self.pending.remove(&(connection, id)) else {
            return Ok(None);
        };
        if selection.sequence != self.sequence || message.get("error").is_some() {
            return Ok(None);
        }
        let thread = message["result"]["thread"]["id"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .context("Codex thread selection response has no native thread ID")?;
        if selection
            .expected_thread
            .as_deref()
            .is_some_and(|expected| expected != thread)
        {
            bail!("Codex resumed a different native thread than the TUI requested");
        }
        Ok(Some((selection.method, thread.to_owned())))
    }

    #[cfg(test)]
    fn request(&mut self, message: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(message)?;
        self.request_on(0, &serde_json::from_slice(&bytes)?)
    }

    #[cfg(test)]
    fn response(&mut self, message: &Value) -> Result<Option<(&'static str, String)>> {
        self.response_on(0, message)
    }
}

fn request_id(id: Option<&Value>) -> Option<String> {
    let id = id?;
    (id.is_string() || id.is_i64() || id.is_u64()).then(|| id.to_string())
}

fn read_exact(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    stopped: &AtomicBool,
    allow_eof: bool,
) -> Result<bool> {
    let mut started = false;
    while !bytes.is_empty() {
        if stopped.load(Ordering::Acquire) {
            return Ok(false);
        }
        match stream.read(bytes) {
            Ok(0) if stopped.load(Ordering::Acquire) => return Ok(false),
            Ok(0) if !started && allow_eof => return Ok(false),
            Ok(0) => bail!("Codex closed a partial native WebSocket frame"),
            Ok(count) => {
                started = true;
                bytes = &mut bytes[count..];
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) if stopped.load(Ordering::Acquire) => return Ok(false),
            Err(error) => return Err(error).context("read Codex native WebSocket"),
        }
    }
    Ok(true)
}

/// Preserve the HTTP upgrade verbatim, without buffering the first frame into it.
fn upgrade(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    stopped: &AtomicBool,
) -> Result<bool> {
    let mut header = Vec::with_capacity(512);
    let mut byte = [0];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16 * 1024 {
            bail!("Codex native WebSocket HTTP upgrade is too large");
        }
        if !read_exact(source, &mut byte, stopped, header.is_empty())? {
            return Ok(false);
        }
        header.push(byte[0]);
    }
    write_wire(destination, &header, stopped)?;
    Ok(true)
}

fn forward(
    source: &mut UnixStream,
    destination: &mut UnixStream,
    stopped: &AtomicBool,
    masked: bool,
    mut inspect: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    if !upgrade(source, destination, stopped)? {
        return Ok(());
    }
    let mut fragments = Vec::new();
    let mut fragment_opcode = None;
    let mut payload = Vec::new();
    loop {
        let mut header = [0; 14];
        if !read_exact(source, &mut header[..2], stopped, true)? {
            if fragment_opcode.is_some() && !stopped.load(Ordering::Acquire) {
                bail!("Codex closed an unfinished native WebSocket message");
            }
            return Ok(());
        }
        let final_frame = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0f;
        if header[0] & 0x70 != 0 || (header[1] & 0x80 != 0) != masked {
            bail!("unsupported Codex native WebSocket frame flags");
        }
        let (length, mut header_len) = match header[1] & 0x7f {
            126 => {
                if !read_exact(source, &mut header[2..4], stopped, false)? {
                    return Ok(());
                }
                (
                    u16::from_be_bytes(header[2..4].try_into().unwrap()) as u64,
                    4,
                )
            }
            127 => {
                if !read_exact(source, &mut header[2..10], stopped, false)? {
                    return Ok(());
                }
                (u64::from_be_bytes(header[2..10].try_into().unwrap()), 10)
            }
            length => (u64::from(length), 2),
        };
        if length > MAX_MESSAGE_BYTES as u64 {
            bail!("Codex native WebSocket frame exceeds its message limit");
        }
        if masked {
            if !read_exact(
                source,
                &mut header[header_len..header_len + 4],
                stopped,
                false,
            )? {
                return Ok(());
            }
            header_len += 4;
        }
        let control_frame = opcode & 8 != 0;
        if control_frame && (!final_frame || length > 125 || !matches!(opcode, 8..=10)) {
            bail!("invalid Codex native WebSocket control frame");
        }
        payload.resize(length as usize, 0);
        if !read_exact(source, &mut payload, stopped, false)? {
            return Ok(());
        }
        if !control_frame {
            match opcode {
                1 | 2 if fragment_opcode.is_none() => fragment_opcode = Some(opcode),
                0 if fragment_opcode.is_some() => {}
                _ => bail!("invalid Codex native WebSocket continuation"),
            }
            if fragment_opcode == Some(1) {
                if fragments.len() + payload.len() > MAX_MESSAGE_BYTES {
                    bail!("Codex native WebSocket message exceeds its limit");
                }
                if final_frame && opcode == 1 {
                    if masked {
                        apply_mask(&mut payload, &header[header_len - 4..header_len]);
                    }
                    inspect(&payload)?;
                    if masked {
                        apply_mask(&mut payload, &header[header_len - 4..header_len]);
                    }
                } else {
                    if masked {
                        let mask = &header[header_len - 4..header_len];
                        fragments.extend(
                            payload
                                .iter()
                                .enumerate()
                                .map(|(index, byte)| byte ^ mask[index % 4]),
                        );
                    } else {
                        fragments.extend_from_slice(&payload);
                    }
                    if final_frame {
                        inspect(&fragments)?;
                        fragments.clear();
                    }
                }
            }
            if final_frame {
                fragment_opcode = None;
            }
        }
        // A selected response's final frame cannot reach the TUI before binding.
        write_wire(destination, &header[..header_len], stopped)?;
        write_wire(destination, &payload, stopped)?;
        if opcode == 8 {
            return Ok(());
        }
    }
}

fn write_wire(stream: &mut UnixStream, bytes: &[u8], stopped: &AtomicBool) -> Result<()> {
    match stream.write_all(bytes) {
        Err(_) if stopped.load(Ordering::Acquire) => Ok(()),
        result => result.context("forward Codex native WebSocket bytes"),
    }
}

fn apply_mask(payload: &mut [u8], mask: &[u8]) {
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_correlated_user_selection_changes_binding() -> Result<()> {
        let mut state = Selections::default();
        state.request(
            &json!({"id":"select", "method":"thread/start", "params":{"threadSource":"user"}}),
        )?;
        state.request(&json!({"id":"title", "method":"thread/start", "params":{"threadSource":"thread_title", "ephemeral":true}}))?;
        assert!(
            state
                .response(
                    &json!({"method":"thread/started", "params":{"thread":{"id":"foreign"}}})
                )?
                .is_none()
        );
        assert!(
            state
                .response(&json!({"id":"title", "result":{"thread":{"id":"title-thread"}}}))?
                .is_none()
        );
        assert_eq!(
            state.response(&json!({"id":"select", "result":{"thread":{"id":"selected"}}}))?,
            Some(("thread/start", "selected".to_owned()))
        );
        assert!(
            state
                .response(&json!({"id":"select", "result":{"thread":{"id":"replayed"}}}))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn rebinds_on_selection_after_first() -> Result<()> {
        let mut state = Selections::default();
        state.request(&json!({"id":1,"method":"thread/start","params":{"threadSource":"user"}}))?;
        assert_eq!(
            state.response(&json!({"id":1,"result":{"thread":{"id":"first"}}}))?,
            Some(("thread/start", "first".to_owned()))
        );
        // No selection is pending now, so model output is not parsed.
        assert!(state.pending.is_empty());
        state.request(&json!({"id":2,"method":"thread/resume","params":{"threadId":"resumed"}}))?;
        assert_eq!(
            state.response(&json!({"id":2,"result":{"thread":{"id":"resumed"}}}))?,
            Some(("thread/resume", "resumed".to_owned()))
        );
        state.request(&json!({"id":3,"method":"thread/start","params":{"threadSource":"user"}}))?;
        assert_eq!(
            state.response(&json!({"id":3,"result":{"thread":{"id":"fresh"}}}))?,
            Some(("thread/start", "fresh".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn stale_selection_cannot_overwrite_newer_selection() -> Result<()> {
        let mut state = Selections::default();
        state.request(&json!({"id":1,"method":"thread/resume","params":{"threadId":"old"}}))?;
        state.request(&json!({"id":2,"method":"thread/fork","params":{"threadSource":"user","threadId":"parent"}}))?;
        assert_eq!(
            state.response(&json!({"id":2,"result":{"thread":{"id":"fork"}}}))?,
            Some(("thread/fork", "fork".to_owned()))
        );
        assert!(
            state
                .response(&json!({"id":1,"result":{"thread":{"id":"old"}}}))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn failed_selection_and_mismatched_resume_never_bind() -> Result<()> {
        let mut state = Selections::default();
        state
            .request(&json!({"id":1,"method":"thread/resume","params":{"threadId":"requested"}}))?;
        assert!(
            state
                .response(&json!({"id":1,"error":{"message":"not found"}}))?
                .is_none()
        );
        state
            .request(&json!({"id":2,"method":"thread/resume","params":{"threadId":"requested"}}))?;
        assert!(
            state
                .response(&json!({"id":2,"result":{"thread":{"id":"wrong"}}}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn request_ids_keep_string_and_integer_namespaces_separate() -> Result<()> {
        let mut state = Selections::default();
        state.request(&json!({"id":7,"method":"thread/resume","params":{"threadId":"numeric"}}))?;
        assert!(
            state
                .response(&json!({"id":"7","result":{"thread":{"id":"wrong"}}}))?
                .is_none()
        );
        assert!(
            state
                .response(&json!({"id":7,"method":"server/request"}))?
                .is_none()
        );
        assert_eq!(
            state.response(&json!({"id":7,"result":{"thread":{"id":"numeric"}}}))?,
            Some(("thread/resume", "numeric".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn reconnect_request_ids_do_not_alias_and_old_connection_cannot_rebind() -> Result<()> {
        let mut state = Selections::default();
        let old = serde_json::from_slice(
            br#"{"id":1,"method":"thread/resume","params":{"threadId":"old"}}"#,
        )?;
        let new = serde_json::from_slice(
            br#"{"id":1,"method":"thread/resume","params":{"threadId":"new"}}"#,
        )?;
        state.request_on(1, &old)?;
        state.request_on(2, &new)?;
        assert!(
            state
                .response_on(1, &json!({"id":1,"result":{"thread":{"id":"old"}}}))?
                .is_none()
        );
        assert_eq!(
            state.response_on(2, &json!({"id":1,"result":{"thread":{"id":"new"}}}))?,
            Some(("thread/resume", "new".to_owned()))
        );
        Ok(())
    }

    const UPGRADE: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n";

    fn frame(opcode: u8, final_frame: bool, bytes: &[u8], masked: bool) -> Vec<u8> {
        let mut frame = vec![opcode | if final_frame { 0x80 } else { 0 }];
        let mask_flag = if masked { 0x80 } else { 0 };
        if bytes.len() < 126 {
            frame.push(mask_flag | bytes.len() as u8);
        } else {
            frame.push(mask_flag | 126);
            frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        }
        let mask = [13, 7, 19, 3];
        if masked {
            frame.extend_from_slice(&mask);
        }
        let offset = frame.len();
        frame.extend_from_slice(bytes);
        if masked {
            apply_mask(&mut frame[offset..], &mask);
        }
        frame
    }

    #[test]
    fn fragmented_native_response_binds_before_final_bytes_and_preserves_control_frames()
    -> Result<()> {
        for masked in [false, true] {
            let message =
                serde_json::to_vec(&json!({"id":"selection","result":{"thread":{"id":"native"}}}))?;
            let first = frame(1, false, &message[..17], masked);
            let ping = frame(9, true, b"native-ping", masked);
            let last = frame(0, true, &message[17..], masked);
            let prefix = [UPGRADE, &first, &ping].concat();
            let wire = [&prefix[..], &last].concat();
            let (mut producer, mut source) = UnixStream::pair()?;
            let (mut destination, mut consumer) = UnixStream::pair()?;
            consumer.set_read_timeout(Some(Duration::from_secs(2)))?;
            producer.write_all(&wire)?;
            producer.shutdown(Shutdown::Write)?;
            let mut observed = 0;
            forward(
                &mut source,
                &mut destination,
                &AtomicBool::new(false),
                masked,
                |bytes| {
                    assert_eq!(bytes, message);
                    let mut before_binding = vec![0; prefix.len()];
                    consumer.read_exact(&mut before_binding)?;
                    assert_eq!(before_binding, prefix);
                    consumer.set_nonblocking(true)?;
                    assert_eq!(
                        consumer.read(&mut [0]).unwrap_err().kind(),
                        io::ErrorKind::WouldBlock
                    );
                    consumer.set_nonblocking(false)?;
                    observed += 1;
                    Ok(())
                },
            )?;
            destination.shutdown(Shutdown::Write)?;
            let mut after_binding = Vec::new();
            consumer.read_to_end(&mut after_binding)?;
            assert_eq!(after_binding, last);
            assert_eq!(observed, 1);
        }
        Ok(())
    }

    #[test]
    fn unfragmented_extended_frame_keeps_exact_wire_bytes() -> Result<()> {
        let message = serde_json::to_vec(
            &json!({"id":1,"method":"thread/start","params":{"threadSource":"user","config":{"instruction": "x".repeat(256)}}}),
        )?;
        let wire = [UPGRADE, &frame(1, true, &message, true)].concat();
        let (mut producer, mut source) = UnixStream::pair()?;
        let (mut destination, mut consumer) = UnixStream::pair()?;
        producer.write_all(&wire)?;
        producer.shutdown(Shutdown::Write)?;
        let mut observed = 0;
        forward(
            &mut source,
            &mut destination,
            &AtomicBool::new(false),
            true,
            |bytes| {
                assert_eq!(bytes, message);
                observed += 1;
                Ok(())
            },
        )?;
        destination.shutdown(Shutdown::Write)?;
        let mut received = Vec::new();
        consumer.read_to_end(&mut received)?;
        assert_eq!(received, wire);
        assert_eq!(observed, 1);
        Ok(())
    }

    #[test]
    fn truncated_and_oversized_native_frames_fail_closed() -> Result<()> {
        for bytes in [
            vec![0x81],
            vec![0x81, 126],
            vec![0x81, 127, 0],
            vec![0x81, 0x80],
            vec![0x01, 0],
            vec![0x81, 3, b'{'],
            vec![0x89, 126, 0, 126],
            vec![0x01, 0, 0x81, 0],
            [
                vec![0x81, 127],
                (MAX_MESSAGE_BYTES as u64 + 1).to_be_bytes().to_vec(),
            ]
            .concat(),
        ] {
            let masked = bytes.get(1).is_some_and(|byte| byte & 0x80 != 0);
            let (mut producer, mut source) = UnixStream::pair()?;
            let (mut destination, _consumer) = UnixStream::pair()?;
            producer.write_all(UPGRADE)?;
            producer.write_all(&bytes)?;
            producer.shutdown(Shutdown::Write)?;
            assert!(
                forward(
                    &mut source,
                    &mut destination,
                    &AtomicBool::new(false),
                    masked,
                    |_| Ok(())
                )
                .is_err(),
                "{bytes:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn controlled_shutdown_can_interrupt_partial_native_frame() -> Result<()> {
        let (mut producer, mut source) = UnixStream::pair()?;
        let (mut destination, _consumer) = UnixStream::pair()?;
        let control = Control::default();
        producer.write_all(UPGRADE)?;
        producer.write_all(&[0x81, 127])?;
        source.set_read_timeout(Some(IO_INTERVAL))?;
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(25));
                control.stop();
            });
            forward(
                &mut source,
                &mut destination,
                &control.stopped,
                false,
                |_| Ok(()),
            )
        })?;
        Ok(())
    }
}
