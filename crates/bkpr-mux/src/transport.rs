use crate::{PROTOCOL_VERSION, Pane, Request};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
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
/// One snapshot per request: slow viewers hold no engine resources or unbounded queues.
pub fn serve(socket: &Path, pane: Pane) -> Result<()> {
    let parent = socket.parent().context("socket needs parent directory")?;
    let meta = std::fs::metadata(parent).context("create a private socket directory first")?;
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        bail!("socket parent must be owned by current user and mode 0700");
    }
    let listener = UnixListener::bind(socket)
        .context("binding private proof socket (never replaces existing sockets)")?;
    let _guard = SocketGuard(socket.to_owned());
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    println!(
        "{}",
        json!({"version":PROTOCOL_VERSION,"ready":true,"socket":socket})
    );
    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone())?;
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if active.fetch_add(1, Ordering::AcqRel) >= 32 {
                    active.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let pane = pane.clone();
                let shutdown = shutdown.clone();
                let active = active.clone();
                thread::spawn(move || {
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
                    }
                    active.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(8))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
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
        bail!("unsupported proof protocol version");
    }
    Ok(response)
}
pub fn checked(socket: &Path, value: &Value) -> Result<Value> {
    let response = request(socket, value)?;
    if response["ok"] != true {
        bail!(
            "{}",
            response["error"]
                .as_str()
                .unwrap_or("unknown endpoint error")
        );
    }
    Ok(response["result"].clone())
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
}
