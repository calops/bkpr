use super::*;
use std::os::fd::{FromRawFd, OwnedFd};

/// The owner's kernel lifetime is retained independently of inherited sockets.
pub(crate) struct ClientOwner {
    peer: crate::harness::Peer,
    exit: OwnedFd,
    #[cfg(target_os = "macos")]
    version: u32,
}
impl ClientOwner {
    pub(crate) fn authenticate(stream: &UnixStream) -> Result<Self> {
        let peer = authenticated_peer(stream)?;
        #[cfg(target_os = "linux")]
        let owner = {
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
                return Err(std::io::Error::last_os_error()).context("client owner pidfd");
            }
            anyhow::ensure!(
                fd >= 0 && length as usize == std::mem::size_of::<i32>(),
                "invalid client owner pidfd"
            );
            Self {
                peer,
                exit: unsafe { OwnedFd::from_raw_fd(fd) },
            }
        };
        #[cfg(target_os = "macos")]
        let owner = {
            let (_, _, version) = kernel_peer(stream)?;
            let descriptor = unsafe { libc::kqueue() };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error()).context("client owner kqueue");
            }
            let exit = unsafe { OwnedFd::from_raw_fd(descriptor) };
            if unsafe { libc::fcntl(exit.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error()).context("client owner close-on-exec");
            }
            let event = libc::kevent {
                ident: peer.pid as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if unsafe {
                libc::kevent(
                    exit.as_raw_fd(),
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error()).context("observing client owner exit");
            }
            let checked = authenticated_peer(stream)?;
            anyhow::ensure!(
                checked.pid == peer.pid && checked.uid == peer.uid && checked.birth == peer.birth,
                "client owner birth changed"
            );
            Self {
                peer,
                exit,
                version,
            }
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("managed client authentication is unsupported on this platform");
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            anyhow::ensure!(owner.is_alive(), "client owner exited during registration");
            Ok(owner)
        }
    }
    pub(crate) fn is_alive(&self) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.exit.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        result == 0 && descriptor.revents == 0
    }
    pub(crate) fn validate(&self, stream: &UnixStream) -> Result<()> {
        let (pid, uid, version) = kernel_peer(stream)?;
        anyhow::ensure!(
            pid == self.peer.pid && uid == self.peer.uid && self.is_alive(),
            "client authority belongs to a different or exited process"
        );
        #[cfg(target_os = "macos")]
        anyhow::ensure!(version == self.version, "client peer birth changed");
        #[cfg(not(target_os = "macos"))]
        let _ = version;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn kernel_peer(stream: &UnixStream) -> Result<(i32, u32, u32)> {
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
        return Err(std::io::Error::last_os_error()).context("client Unix peer credentials");
    }
    anyhow::ensure!(
        length as usize == std::mem::size_of::<libc::ucred>()
            && credential.uid == unsafe { libc::geteuid() },
        "invalid client Unix peer credentials"
    );
    Ok((credential.pid, credential.uid, 0))
}
#[cfg(target_os = "macos")]
fn kernel_peer(stream: &UnixStream) -> Result<(i32, u32, u32)> {
    let mut token = [0_u32; 8];
    let mut length = std::mem::size_of_val(&token) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            6,
            token.as_mut_ptr().cast(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("client Unix peer audit token");
    }
    anyhow::ensure!(
        length as usize == std::mem::size_of_val(&token) && token[1] == unsafe { libc::geteuid() },
        "invalid client Unix peer audit token"
    );
    Ok((token[5] as i32, token[1], token[7]))
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn kernel_peer(_: &UnixStream) -> Result<(i32, u32, u32)> {
    anyhow::bail!("client peer authentication unsupported")
}

pub(crate) fn send_lifetime_event(stream: &UnixStream, bytes: &[u8]) {
    // Never wait for a displaced frontend's reader, including a stopped process.
    #[cfg(target_os = "macos")]
    {
        let enabled = 1_i32;
        if unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const i32).cast(),
                std::mem::size_of::<i32>() as libc::socklen_t,
            )
        } != 0
        {
            return;
        }
    }
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
    #[cfg(not(target_os = "linux"))]
    let flags = libc::MSG_DONTWAIT;
    unsafe {
        libc::send(
            stream.as_raw_fd(),
            bytes.as_ptr().cast(),
            bytes.len(),
            flags,
        );
    }
}

pub(crate) fn lifetime_connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // A lifetime stream accepts no traffic after registration; readiness means
    // either EOF or a protocol violation, neither can authorize takeover.
    unsafe { libc::poll(&mut descriptor, 1, 0) == 0 && descriptor.revents == 0 }
}

pub(super) fn watch_client(
    stream: &UnixStream,
    registry: &Registry,
    client: &Arc<crate::registry::sessions::ClientLease>,
) {
    let reason = loop {
        if client.detached() {
            return;
        }
        let mut descriptors = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client.owner().exit.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        };
        if result < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break "disconnected";
        }
        if descriptors[1].revents != 0 {
            break "owner_exited";
        }
        if descriptors[0].revents != 0 {
            // This is a watch-only stream: EOF or unsolicited bytes end its lease.
            break "disconnected";
        }
    };
    registry.detach_client(client, reason);
}
