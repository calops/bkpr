//! Narrow provenance port of core transport.rs/harness_process.rs; keep kernel birth checks aligned.
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, ensure};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Process {
    pid: i32,
    parent: i32,
    birth: [u64; 2],
    tty: u64,
}

pub(super) struct Root(Process);
impl Root {
    pub fn current() -> Result<Self> {
        Ok(Self(process(std::process::id() as i32)?))
    }
}

pub(super) struct Peer {
    process: Process,
    #[cfg(target_os = "linux")]
    pidfd: std::os::fd::OwnedFd,
    #[cfg(target_os = "macos")]
    version: u32,
}
impl Peer {
    pub fn authenticate(stream: &UnixStream) -> Result<Self> {
        platform_peer(stream)
    }

    pub fn validate(&self, root: &Root) -> Result<()> {
        self.pinned()?;
        let mut current = process(self.process.pid)?;
        ensure!(
            current == self.process,
            "Codex relay peer birth or ancestry changed"
        );
        let mut chain = Vec::new();
        for _ in 0..128 {
            let reached = current.pid == root.0.pid;
            let parent = current.parent;
            chain.push(current);
            if reached {
                ensure!(
                    chain.last() == Some(&root.0),
                    "Codex relay launch root birth or TTY changed"
                );
                for expected in &chain {
                    ensure!(
                        process(expected.pid)? == *expected,
                        "Codex relay ancestry is no longer live"
                    );
                }
                self.pinned()?;
                return Ok(());
            }
            ensure!(
                parent > 0 && !chain.iter().any(|item| item.pid == parent),
                "Codex relay peer is outside this owned launch"
            );
            current = process(parent)?;
        }
        anyhow::bail!("Codex relay peer is outside this owned launch")
    }

    #[cfg(target_os = "linux")]
    fn pinned(&self) -> Result<()> {
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        ensure!(
            unsafe { libc::poll(&mut poll, 1, 0) } == 0 && poll.revents == 0,
            "Codex relay original socket peer exited"
        );
        Ok(())
    }
    #[cfg(target_os = "macos")]
    fn pinned(&self) -> Result<()> {
        ensure!(
            darwin_version(self.process.pid)? == self.version,
            "Codex relay original socket peer PID version changed"
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn process(pid: i32) -> Result<Process> {
    use std::os::unix::fs::MetadataExt;
    ensure!(pid > 0, "invalid Codex relay process");
    let directory = std::path::PathBuf::from(format!("/proc/{pid}"));
    ensure!(
        std::fs::metadata(&directory)?.uid() == unsafe { libc::geteuid() },
        "Codex relay process belongs to another user"
    );
    let stat = std::fs::read_to_string(directory.join("stat"))?;
    let mut fields = stat
        .rsplit_once(") ")
        .context("invalid Codex relay process identity")?
        .1
        .split_whitespace();
    let state = fields.next().context("missing Codex relay process state")?;
    ensure!(
        !matches!(state, "Z" | "X" | "x" | "T" | "t"),
        "Codex relay process is not live"
    );
    let parent = fields
        .next()
        .context("missing Codex relay process parent")?
        .parse()?;
    let tty = fields
        .nth(2)
        .context("missing Codex relay process TTY")?
        .parse::<i64>()? as u64;
    let birth = fields
        .nth(14)
        .context("missing Codex relay process birth")?
        .parse()?;
    Ok(Process {
        pid,
        parent,
        birth: [birth, 0],
        tty,
    })
}

#[cfg(target_os = "linux")]
fn platform_peer(stream: &UnixStream) -> Result<Peer> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut credential: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credential as *mut libc::ucred).cast(),
                &mut length,
            )
        } == 0
            && length as usize == std::mem::size_of::<libc::ucred>()
            && credential.uid == unsafe { libc::geteuid() },
        "invalid Codex relay socket peer credentials"
    );
    let mut fd = -1_i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                77,
                (&mut fd as *mut i32).cast(),
                &mut length,
            )
        } == 0,
        "Codex relay birth authentication requires SO_PEERPIDFD"
    );
    ensure!(
        fd >= 0 && length as usize == std::mem::size_of::<i32>(),
        "invalid Codex relay peer pidfd"
    );
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    let peer = Peer {
        process: process(credential.pid)?,
        pidfd,
    };
    peer.pinned()?;
    Ok(peer)
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct BsdInfo {
    flags: u32,
    status: u32,
    xstatus: u32,
    pid: u32,
    parent: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    reserved: u32,
    comm: [u8; 16],
    name: [u8; 32],
    nfiles: u32,
    group: u32,
    jobc: u32,
    tty: u32,
    foreground: u32,
    nice: i32,
    start_sec: u64,
    start_usec: u64,
}
#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut libc::c_void, bytes: i32) -> i32;
}
#[cfg(target_os = "macos")]
fn process(pid: i32) -> Result<Process> {
    ensure!(pid > 0, "invalid Codex relay process");
    let mut info: BsdInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<BsdInfo>();
    ensure!(
        unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut BsdInfo).cast(), size as i32) }
            as usize
            == size,
        "incomplete Darwin Codex relay process identity"
    );
    ensure!(
        info.uid == unsafe { libc::geteuid() }
            && !matches!(info.status, 4 | 5)
            && info.flags & 4 == 0,
        "Darwin Codex relay process is not a live same-user process"
    );
    Ok(Process {
        pid,
        parent: info.parent as i32,
        birth: [info.start_sec, info.start_usec],
        tty: u64::from(info.tty),
    })
}
#[cfg(target_os = "macos")]
fn darwin_version(pid: i32) -> Result<u32> {
    #[repr(C)]
    struct Unique {
        uuid: [u8; 16],
        unique: u64,
        parent_unique: u64,
        version: i32,
        original_parent_version: i32,
        reserved2: u64,
        reserved3: u64,
    }
    let mut unique: Unique = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<Unique>();
    ensure!(
        unsafe { proc_pidinfo(pid, 17, 0, (&mut unique as *mut Unique).cast(), size as i32) }
            as usize
            == size,
        "incomplete Darwin Codex relay peer PID version"
    );
    Ok(unique.version as u32)
}
#[cfg(target_os = "macos")]
fn platform_peer(stream: &UnixStream) -> Result<Peer> {
    let mut token = [0_u32; 8];
    let mut size = std::mem::size_of_val(&token) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                6,
                token.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
            && size as usize == std::mem::size_of_val(&token)
            && token[1] == unsafe { libc::geteuid() },
        "invalid Darwin Codex relay socket audit token"
    );
    let peer = Peer {
        process: process(token[5] as i32)?,
        version: token[7],
    };
    peer.pinned()?;
    Ok(peer)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;

    struct Child(i32);
    impl Drop for Child {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
                libc::waitpid(self.0, std::ptr::null_mut(), 0);
            }
        }
    }
    fn child_connect(path: &std::path::Path) -> Child {
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_os_str().as_bytes();
        assert!(bytes.len() < address.sun_path.len());
        for (target, source) in address.sun_path.iter_mut().zip(bytes) {
            *target = *source as libc::c_char;
        }
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // Only async-signal-safe libc calls after fork; no Rust allocation or locks.
            unsafe {
                let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
                if fd < 0
                    || libc::connect(
                        fd,
                        (&address as *const libc::sockaddr_un).cast(),
                        std::mem::size_of_val(&address) as libc::socklen_t,
                    ) != 0
                {
                    libc::_exit(2);
                }
                let ready = b"x";
                libc::write(fd, ready.as_ptr().cast(), 1);
                let mut byte = 0_u8;
                libc::read(fd, (&mut byte as *mut u8).cast(), 1);
                libc::_exit(0);
            }
        }
        Child(pid)
    }

    #[test]
    fn original_socket_peer_requires_live_birth_and_owned_root_ancestry() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("peer.sock");
        let listener = UnixListener::bind(&path)?;
        let first = child_connect(&path);
        let (mut first_stream, _) = listener.accept()?;
        first_stream.read_exact(&mut [0])?;
        let first_peer = Peer::authenticate(&first_stream)?;
        first_peer.validate(&Root::current()?)?;
        let launch_root = Root(process(first.0)?);
        first_peer.validate(&launch_root)?;
        let second = child_connect(&path);
        let (mut second_stream, _) = listener.accept()?;
        second_stream.read_exact(&mut [0])?;
        let sibling_peer = Peer::authenticate(&second_stream)?;
        assert!(
            sibling_peer.validate(&launch_root).is_err(),
            "same-user sibling borrowed another launch root"
        );
        drop(second);
        drop(first);
        assert!(
            first_peer.validate(&launch_root).is_err(),
            "exited original peer retained relay authority"
        );
        Ok(())
    }
}
