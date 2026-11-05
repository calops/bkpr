//! Ephemeral launch authority carried through native command admission.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::harness_process;
use crate::harness_process::Process;
use anyhow::{Context, Result, ensure};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::FromRawFd;
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default)]
pub(crate) struct ControlLease(Arc<Authority>);
struct Authority {
    active: AtomicBool,
    owners: OnceLock<std::result::Result<Owners, String>>,
}
impl Default for Authority {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(true),
            owners: OnceLock::new(),
        }
    }
}
struct Owners {
    source: ProcessLifetime,
    pane: Option<ProcessLifetime>,
}
impl ControlLease {
    pub(crate) fn bind(&self, source: &Process, pane: &Process) -> Result<()> {
        ensure!(self.is_valid(), "control launch already ended");
        let owners = self.0.owners.get_or_init(|| {
            (|| -> Result<Owners> {
                let source_owner = ProcessLifetime::observe(source)?;
                let pane_owner = if source.pid == pane.pid && source.identity == pane.identity {
                    None
                } else {
                    Some(ProcessLifetime::observe(pane)?)
                };
                Ok(Owners {
                    source: source_owner,
                    pane: pane_owner,
                })
            })()
            .map_err(|error| format!("{error:#}"))
        });
        owners
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        self.ensure_active()
    }
    /// An in-memory fence for registry critical sections, not a liveness proof.
    pub(crate) fn is_valid(&self) -> bool {
        self.0.active.load(Ordering::Acquire)
    }
    pub(crate) fn revoke(&self) {
        self.0.active.store(false, Ordering::Release);
    }
    /// Like the native client-owner check, this polls retained kernel lifetime
    /// descriptors without walking procfs, ancestry, or any registry state.
    pub(crate) fn ensure_active(&self) -> Result<()> {
        ensure!(
            self.is_valid(),
            "control launch ended before input acceptance"
        );
        let owners = self
            .0
            .owners
            .get()
            .context("control lease has not been bound")?
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        if !owners.source.is_alive() || owners.pane.as_ref().is_some_and(|pane| !pane.is_alive()) {
            self.revoke();
        }
        ensure!(
            self.is_valid(),
            "control launch exited or ended before input acceptance"
        );
        Ok(())
    }
}

/// The same pidfd/kqueue lifetime primitives used by managed client ownership.
/// Descriptor acquisition and birth checks run only on the control worker.
struct ProcessLifetime(OwnedFd);
impl ProcessLifetime {
    fn observe(expected: &Process) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let descriptor = {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) } as i32;
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).context("control launch pidfd");
            }
            unsafe { OwnedFd::from_raw_fd(fd) }
        };
        #[cfg(target_os = "macos")]
        let descriptor = {
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).context("control launch kqueue");
            }
            let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("control launch close-on-exec");
            }
            let event = libc::kevent {
                ident: expected.pid as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if unsafe { libc::kevent(fd, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("observing control launch exit");
            }
            descriptor
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = expected;
            anyhow::bail!("process-bound control input is unsupported on this platform");
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let current =
                harness_process::process(expected.pid)?.context("control launch exited")?;
            ensure!(
                current.identity == expected.identity,
                "control launch birth changed"
            );
            let lifetime = Self(descriptor);
            ensure!(
                lifetime.is_alive(),
                "control launch exited during authorization"
            );
            Ok(lifetime)
        }
    }
    fn is_alive(&self) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut descriptor, 1, 0) == 0 && descriptor.revents == 0 }
    }
}
