use crate::registry::Registry;
use anyhow::{Context, Result, bail};
#[cfg(test)]
use bkpr_protocol::MAX_CONTROL_CONNECTIONS;
use bkpr_protocol::{
    MAX_CONNECTIONS, MAX_REGISTRY_WATCHERS, MAX_WAITING_PANE_POLLS, Operation, PROTOCOL_VERSION,
    PaneRequest, Request,
};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
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
fn waiting_slot(
    operation: &Operation,
    panes: &Arc<AtomicUsize>,
    registry: &Arc<AtomicUsize>,
) -> Result<Option<ConnectionSlot>> {
    match operation {
        Operation::Pane {
            request: PaneRequest::Poll { wait_ms, .. },
            ..
        } if *wait_ms > 0 => Ok(Some(
            ConnectionSlot::take(panes, MAX_WAITING_PANE_POLLS)
                .context("pending pane poll limit")?,
        )),
        Operation::Wait { wait_ms, .. } if *wait_ms > 0 => Ok(Some(
            ConnectionSlot::take(registry, MAX_REGISTRY_WATCHERS)
                .context("registry watcher limit")?,
        )),
        _ => Ok(None),
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
struct SocketLease(std::fs::File);
impl Drop for SocketLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
/// A bound listener may queue initial-wrapper requests before pane startup commits.
pub struct BoundSocket {
    listener: UnixListener,
    socket: PathBuf,
    _guard: SocketGuard,
    _lease: SocketLease,
}
/// Bind ownership and listening independently of starting request dispatch.
pub fn bind(socket: &Path) -> Result<BoundSocket> {
    let parent = socket.parent().context("socket needs parent directory")?;
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = std::fs::metadata(parent).context("create a private socket directory first")?;
    if !std::fs::symlink_metadata(parent)?.is_dir() {
        bail!("socket parent cannot be a symlink");
    }
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        bail!("socket parent must be owned by current user and mode 0700");
    }
    let lease = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket.with_extension("lock"))?;
    let metadata = lease.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("socket lock must be private and current-user-owned");
    }
    fs2::FileExt::try_lock_exclusive(&lease).context("socket already owned by another daemon")?;
    let _lease = SocketLease(lease);
    if socket.exists() {
        match UnixStream::connect(socket) {
            Ok(_) => bail!("socket already has a live listener"),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                let metadata = std::fs::symlink_metadata(socket)?;
                if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() }
                {
                    bail!("refusing to replace foreign or non-socket path");
                }
                std::fs::remove_file(socket)?;
            }
            Err(error) => return Err(error).context("checking existing daemon socket"),
        }
    }
    let listener = UnixListener::bind(socket).context("binding private registry socket")?;
    let guard = SocketGuard(socket.to_owned());
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(BoundSocket {
        listener,
        socket: socket.to_owned(),
        _guard: guard,
        _lease,
    })
}
/// Convenience for existing library consumers which do not bootstrap a first pane.
pub fn serve(socket: &Path, registry: Registry) -> Result<()> {
    bind(socket)?.serve(registry)
}
impl BoundSocket {
    pub fn serve(self, registry: Registry) -> Result<()> {
        let Self {
            listener,
            socket,
            _guard,
            _lease,
        } = self;
        let shutdown = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let waiting_panes = Arc::new(AtomicUsize::new(0));
        let waiting_registry = Arc::new(AtomicUsize::new(0));
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
            json!({"version":PROTOCOL_VERSION,"ready":true,"socket":bkpr_protocol::path_wire::WirePath(&socket)})
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
                    let registry = registry.clone();
                    let shutdown = shutdown.clone();
                    let waiting_panes = waiting_panes.clone();
                    let waiting_registry = waiting_registry.clone();
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
                        let _waiting = waiting_slot(&request.operation, &waiting_panes, &waiting_registry)?;
                        stop = request.version == PROTOCOL_VERSION && matches!(&request.operation, Operation::Shutdown);
                        let result = if matches!(&request.operation, Operation::HarnessBegin { .. } | Operation::HarnessEvent { .. } | Operation::HarnessContext { .. }) {
                            let peer = authenticated_peer(&stream)?;
                            registry.request_authenticated(request, &peer)?
                        } else {
                            registry.request(request)?
                        };
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
                        serde_json::to_vec(&json!({"version":PROTOCOL_VERSION,"ok":false,"error":"aggregate scene wire limit exceeded after dispatch; outcome unknown, do not replay"})).unwrap()
                    };
                    let _ = stream.write_all(&bytes);
                    let _ = stream.write_all(b"\n");
                    // A valid explicit shutdown closes the daemon even when owned cleanup
                    // reports an error. Keep the error response, not a poisoned listener.
                    if stop {
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
        registry.shutdown()
    }
}

#[cfg(target_os = "linux")]
fn authenticated_peer(stream: &UnixStream) -> Result<crate::harness::Peer> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credential as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("Unix peer credentials");
    }
    anyhow::ensure!(
        length as usize == std::mem::size_of::<libc::ucred>()
            && credential.uid == unsafe { libc::geteuid() },
        "invalid Unix peer credentials"
    );
    // SO_PEERPIDFD pins the *socket's original peer*, not a possibly recycled claimed PID.
    // Older kernels fail closed for capture rather than silently weakening birth provenance.
    let mut fd = -1_i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            77,
            (&mut fd as *mut i32).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("birth-bound Unix peer authentication requires SO_PEERPIDFD");
    }
    anyhow::ensure!(
        fd >= 0 && length as usize == std::mem::size_of::<i32>(),
        "invalid Unix peer pidfd"
    );
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    let process = crate::harness_process::process(credential.pid)?.context("Unix peer exited")?;
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    anyhow::ensure!(
        result == 0 && poll.revents == 0,
        "Unix socket peer exited or changed birth"
    );
    Ok(crate::harness::Peer {
        pid: credential.pid,
        uid: credential.uid,
        birth: process.identity,
    })
}
#[cfg(target_os = "macos")]
fn authenticated_peer(stream: &UnixStream) -> Result<crate::harness::Peer> {
    // LOCAL_PEERTOKEN includes the kernel PID version. Match PROC_PIDUNIQIDENTIFIERINFO
    // as well as the microsecond birth read, so a recycled peer PID cannot lend authority.
    let mut token = [0_u32; 8];
    let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            6,
            token.as_mut_ptr().cast(),
            &mut size,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("Darwin Unix peer audit token");
    }
    anyhow::ensure!(
        size as usize == std::mem::size_of_val(&token) && token[1] == unsafe { libc::geteuid() },
        "foreign Darwin Unix peer"
    );
    #[repr(C)]
    // xnu bsd/sys/proc_info_private.h: this API structure is exactly 56 bytes.
    struct Unique {
        uuid: [u8; 16],
        unique: u64,
        parent_unique: u64,
        version: i32,
        original_parent_version: i32,
        reserved2: u64,
        reserved3: u64,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            bytes: i32,
        ) -> i32;
    }
    let process =
        crate::harness_process::process(token[5] as i32)?.context("Darwin Unix peer exited")?;
    let mut unique: Unique = unsafe { std::mem::zeroed() };
    let bytes = std::mem::size_of::<Unique>();
    let count = unsafe {
        proc_pidinfo(
            process.pid,
            17,
            0,
            (&mut unique as *mut Unique).cast(),
            bytes as i32,
        )
    };
    anyhow::ensure!(
        count as usize == bytes && unique.version as u32 == token[7],
        "Darwin Unix peer PID version changed"
    );
    Ok(crate::harness::Peer {
        pid: process.pid,
        uid: token[1],
        birth: process.identity,
    })
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn authenticated_peer(_: &UnixStream) -> Result<crate::harness::Peer> {
    bail!("birth-bound Unix peer authentication unsupported")
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
        let waiting_panes = Arc::new(AtomicUsize::new(0));
        let waiting_registry = Arc::new(AtomicUsize::new(0));
        let poll = Operation::Pane {
            pane_id: "pane".into(),
            request: PaneRequest::Poll {
                view_id: "view".into(),
                after: 0,
                frame_after: 0,
                resources: Vec::new(),
                snapshot_after: 0,
                wait_ms: 1000,
                row_deltas: false,
                scene_encoding: None,
            },
        };
        let watch = Operation::Wait {
            after: 0,
            wait_ms: 1000,
        };
        let polls = (0..MAX_WAITING_PANE_POLLS)
            .map(|_| {
                (
                    ConnectionSlot::take(&active, MAX_CONNECTIONS).unwrap(),
                    waiting_slot(&poll, &waiting_panes, &waiting_registry).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(waiting_slot(&poll, &waiting_panes, &waiting_registry).is_err());
        let watchers = (0..MAX_REGISTRY_WATCHERS)
            .map(|_| {
                (
                    ConnectionSlot::take(&active, MAX_CONNECTIONS).unwrap(),
                    waiting_slot(&watch, &waiting_panes, &waiting_registry).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(waiting_slot(&watch, &waiting_panes, &waiting_registry).is_err());
        let controls = (0..MAX_CONTROL_CONNECTIONS)
            .map(|_| ConnectionSlot::take(&active, MAX_CONNECTIONS).unwrap())
            .collect::<Vec<_>>();
        assert!(ConnectionSlot::take(&active, MAX_CONNECTIONS).is_none());
        drop((controls, watchers, polls));
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(waiting_panes.load(Ordering::Acquire), 0);
        assert_eq!(waiting_registry.load(Ordering::Acquire), 0);
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

#[cfg(test)]
mod shutdown_tests;
