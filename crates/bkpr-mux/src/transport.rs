use crate::{PROTOCOL_VERSION, Pane, Request};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
const MAX_REQUEST: u64 = 8 * 1024 * 1024;
const MAX_RESPONSE: u64 = 96 * 1024 * 1024;
const MAX_WAITING_POLLS: usize = 32;
// Every attached view may wait without consuming the slots reserved for controls.
const MAX_CONNECTIONS: usize = MAX_WAITING_POLLS + 32;
struct ConnectionSlot(Arc<AtomicUsize>);
impl ConnectionSlot {
    fn take(active: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        if active.fetch_add(1, Ordering::AcqRel) >= limit {
            active.fetch_sub(1, Ordering::AcqRel);
            None
        } else {
            Some(Self(active.clone()))
        }
    }
}
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Default)]
struct SignalRegistrations(Vec<signal_hook::SigId>);
impl Drop for SignalRegistrations {
    fn drop(&mut self) {
        for registration in self.0.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}
struct WireBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for WireBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "aggregate scene wire limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
/// One bounded snapshot per request; waiting viewers hold no engine locks.
pub fn serve(socket: &Path, pane: Pane) -> Result<()> {
    let parent = socket.parent().context("socket needs parent directory")?;
    let meta = std::fs::metadata(parent).context("create a private socket directory first")?;
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        bail!("socket parent must be owned by current user and mode 0700");
    }
    let listener = UnixListener::bind(socket)
        .context("binding private pane socket (never replaces existing sockets)")?;
    let _guard = SocketGuard(socket.to_owned());
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let waiting = Arc::new(AtomicUsize::new(0));
    let (wake_read, wake_write) = UnixStream::pair()?;
    wake_write.set_nonblocking(true)?;
    let wake_write = Arc::new(wake_write);
    let mut signals = SignalRegistrations::default();
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signals
            .0
            .push(signal_hook::flag::register(signal, shutdown.clone())?);
        signals.0.push(signal_hook::low_level::pipe::register(
            signal,
            wake_write.try_clone()?,
        )?);
    }
    println!(
        "{}",
        json!({"version":PROTOCOL_VERSION,"ready":true,"socket":socket})
    );
    while !shutdown.load(Ordering::Acquire) {
        if !wait_for_accept(&listener, &wake_read, &shutdown)? {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let Some(slot) = ConnectionSlot::take(&active, MAX_CONNECTIONS) else {
                    continue;
                };
                let pane = pane.clone();
                let shutdown = shutdown.clone();
                let waiting = waiting.clone();
                let wake_write = wake_write.clone();
                thread::Builder::new()
                    .name("bkpr-control".into())
                    .spawn(move || {
                    let _slot = slot;
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                    let mut stop = false;
                    let result = (|| -> Result<_> {
                        let mut bytes = Vec::new();
                        BufReader::new((&mut stream).take(MAX_REQUEST + 1))
                            .read_until(b'\n', &mut bytes)?;
                        if bytes.len() as u64 > MAX_REQUEST {
                            bail!("request size limit");
                        }
                        let request: Request = serde_json::from_slice(&bytes)?;
                        let _waiting = if matches!(&request, Request::Poll { wait_ms, .. } if *wait_ms > 0) {
                            Some(ConnectionSlot::take(&waiting, MAX_WAITING_POLLS)
                                .context("pending snapshot wait limit")?)
                        } else {
                            None
                        };
                        stop = matches!(&request, Request::Shutdown);
                        let result = pane.request(request)?;
                        Ok(result)
                    })();
                    let response = match result {
                        Ok(value) => json!({"version":PROTOCOL_VERSION,"ok":true,"result":value}),
                        Err(e) => {
                            json!({"version":PROTOCOL_VERSION,"ok":false,"error":format!("{e:#}")})
                        }
                    };
                    let mut encoded = WireBuffer {
                        bytes: Vec::new(),
                        limit: MAX_RESPONSE as usize,
                    };
                    let bytes = if serde_json::to_writer(&mut encoded, &response).is_ok() {
                        encoded.bytes
                    } else {
                        serde_json::to_vec(&json!({"version":PROTOCOL_VERSION,"ok":false,"error":"aggregate scene wire limit exceeded before dispatch"})).unwrap()
                    };
                    let _ = stream.write_all(&bytes);
                    let _ = stream.write_all(b"\n");
                    if stop && response["ok"] == true {
                        shutdown.store(true, Ordering::Release);
                        let _ = (&*wake_write).write_all(b"\0");
                    }
                })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    pane.shared.changes.notify();
    Ok(())
}
fn wait_for_accept(
    listener: &UnixListener,
    wake: &UnixStream,
    shutdown: &AtomicBool,
) -> Result<bool> {
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        if descriptors.iter().any(|descriptor| {
            descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        }) {
            bail!("pane listener or shutdown wake descriptor closed");
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            return Ok(true);
        }
    }
}
pub fn request(socket: &Path, value: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, value)?;
    stream.write_all(b"\n")?;
    let mut bytes = Vec::new();
    BufReader::new(stream.take(MAX_RESPONSE + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        bail!("frame response size limit");
    }
    let response: Value = serde_json::from_slice(&bytes)?;
    if response["version"] != PROTOCOL_VERSION {
        bail!("unsupported pane protocol version");
    }
    Ok(response)
}
pub fn checked(socket: &Path, value: &Value) -> Result<Value> {
    let mut response = request(socket, value)?;
    if response["ok"] != true {
        bail!(
            "{}",
            response["error"]
                .as_str()
                .unwrap_or("unknown endpoint error")
        );
    }
    Ok(response["result"].take())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_wire_limit_rejects_before_transport_dispatch() {
        let mut encoded = WireBuffer {
            bytes: Vec::new(),
            limit: 16,
        };
        assert!(
            serde_json::to_writer(&mut encoded, &json!({"large":"012345678901234567890"})).is_err()
        );
        assert!(encoded.bytes.len() <= 16);
    }

    #[test]
    fn waiting_views_leave_bounded_control_capacity_and_release_slots() {
        let active = Arc::new(AtomicUsize::new(0));
        let waiting = Arc::new(AtomicUsize::new(0));
        let polls = (0..MAX_WAITING_POLLS)
            .map(|_| {
                (
                    ConnectionSlot::take(&active, MAX_CONNECTIONS).unwrap(),
                    ConnectionSlot::take(&waiting, MAX_WAITING_POLLS).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(ConnectionSlot::take(&waiting, MAX_WAITING_POLLS).is_none());
        let controls = (0..MAX_CONNECTIONS - MAX_WAITING_POLLS)
            .map(|_| ConnectionSlot::take(&active, MAX_CONNECTIONS).unwrap())
            .collect::<Vec<_>>();
        assert!(ConnectionSlot::take(&active, MAX_CONNECTIONS).is_none());
        drop(controls);
        drop(polls);
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(waiting.load(Ordering::Acquire), 0);
    }

    #[test]
    fn listener_poll_handles_readiness_and_shutdown_wake() {
        let directory =
            std::env::temp_dir().join(format!("bkpr-listener-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        {
            let socket = directory.join("pane.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let _guard = SocketGuard(socket.clone());
            let (read, mut write) = UnixStream::pair().unwrap();
            let shutdown = Arc::new(AtomicBool::new(false));
            let _client = UnixStream::connect(socket).unwrap();
            assert!(wait_for_accept(&listener, &read, &shutdown).unwrap());
            let _accepted = listener.accept().unwrap();
            let stop = shutdown.clone();
            let signal = thread::spawn(move || {
                stop.store(true, Ordering::Release);
                write.write_all(b"\0").unwrap();
            });
            assert!(!wait_for_accept(&listener, &read, &shutdown).unwrap());
            signal.join().unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
