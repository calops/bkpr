use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use std::fs::{self, DirBuilder, Metadata};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(2);

pub fn request(socket: &Path, request: &shephrd::Request) -> Result<shephrd::Response> {
    let mut stream = connect(socket, Duration::from_secs(2))
        .with_context(|| format!("connecting to companion {}", socket.display()))?;
    check_peer(&stream)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    write_json(&mut stream, request)?;
    let response: shephrd::Response = read_json(&mut BufReader::new(stream))?;
    if response.protocol != shephrd::PROTOCOL_VERSION {
        bail!(
            "unsupported companion response protocol {}",
            response.protocol
        );
    }
    if response.request_id != request.request_id {
        bail!("companion response request ID does not match");
    }
    Ok(response)
}

pub(crate) fn connect(path: &Path, timeout: Duration) -> std::io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
    {
        address.sun_len = length as u8;
    }
    #[cfg(target_os = "linux")]
    let flags = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = libc::SOCK_STREAM;
    let raw = unsafe { libc::socket(libc::AF_UNIX, flags, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let deadline = Instant::now() + timeout;
    loop {
        let result =
            unsafe { libc::connect(raw, (&address as *const libc::sockaddr_un).cast(), length) };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EISCONN) {
            break;
        }
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS | libc::EALREADY | libc::EAGAIN | libc::EINTR)
        ) {
            return Err(error);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Unix socket connection timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub(crate) fn read_json<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let mut bytes = Vec::new();
    reader
        .take((shephrd::MAX_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > shephrd::MAX_MESSAGE_BYTES {
        bail!("message exceeds {} bytes", shephrd::MAX_MESSAGE_BYTES);
    }
    if bytes.last() != Some(&b'\n') {
        bail!("connection closed before a complete newline JSON message");
    }
    serde_json::from_slice(&bytes).context("invalid JSON message")
}

pub(crate) fn write_json(writer: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() >= shephrd::MAX_MESSAGE_BYTES {
        bail!("message exceeds {} bytes", shephrd::MAX_MESSAGE_BYTES);
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn check_peer(stream: &UnixStream) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("reading socket peer credentials");
        }
        if credentials.uid != unsafe { libc::geteuid() } {
            bail!("companion socket peer is not the current user");
        }
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
    {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(std::io::Error::last_os_error()).context("reading socket peer credentials");
        }
        if uid != unsafe { libc::geteuid() } {
            bail!("companion socket peer is not the current user");
        }
    }
    Ok(())
}

pub(crate) struct Listener {
    pub socket: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
}

impl Listener {
    pub fn bind(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!("companion socket path must be absolute");
        }
        let parent = path.parent().context("socket has no parent directory")?;
        match fs::symlink_metadata(parent) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DirBuilder::new().mode(0o700).create(parent)?;
            }
            Err(error) => return Err(error.into()),
        }
        let directory = fs::symlink_metadata(parent)?;
        if !directory.is_dir()
            || directory.uid() != unsafe { libc::geteuid() }
            || directory.permissions().mode() & 0o077 != 0
        {
            bail!(
                "socket directory {} must be a private directory owned by the current user",
                parent.display()
            );
        }
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
                bail!(
                    "refusing to replace a foreign or non-socket path {}",
                    path.display()
                );
            }
            match connect(path, Duration::from_secs(1)) {
                Ok(_) => bail!("a companion already owns {}", path.display()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    if fs::symlink_metadata(path)
                        .is_ok_and(|current| identity(&current) == identity(&metadata))
                    {
                        fs::remove_file(path)?;
                    }
                }
                Err(error) => return Err(error).context("checking existing companion socket"),
            }
        }
        let socket = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        socket.set_nonblocking(true)?;
        let identity = identity(&fs::symlink_metadata(path)?);
        Ok(Self {
            socket,
            path: path.to_owned(),
            identity,
        })
    }
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

impl Drop for Listener {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| identity(&metadata) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framing_rejects_oversize_and_truncated_messages() {
        let mut oversized = Cursor::new(vec![b' '; shephrd::MAX_MESSAGE_BYTES + 2]);
        assert!(
            read_json::<serde_json::Value>(&mut oversized)
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
        let mut truncated = Cursor::new(b"{}".as_slice());
        assert!(read_json::<serde_json::Value>(&mut truncated).is_err());
        let mut malformed = Cursor::new(b"{broken}\n".as_slice());
        assert!(read_json::<serde_json::Value>(&mut malformed).is_err());
    }
}
