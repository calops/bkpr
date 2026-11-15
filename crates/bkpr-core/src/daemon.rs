//! Host-process ownership of independently lived native sessions.

use crate::{
    registry::{self, Registry},
    transport::{self, BoundSocket, Stop},
};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{
    DaemonInfo, Operation, PaneSpawnSpec, Payload, SessionEndpoint, SpawnDefaults,
};
use fs2::FileExt;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use uuid::Uuid;

/// Trusted host paths and the matching utility used by session-local wrappers.
pub struct HostConfig {
    /// Private host control socket; its parent contains derived session sockets.
    pub socket: PathBuf,
    /// Private host metadata root containing the durable `sessions` directory.
    pub state_root: PathBuf,
    /// Matching Bkpr utility executable, never executed by host administration.
    pub bkpr: PathBuf,
}
impl HostConfig {
    /// Resolves the current user's default private host control endpoint.
    pub fn default_socket() -> Result<PathBuf> {
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(path) => PathBuf::from(path),
            None => {
                std::fs::canonicalize("/tmp")?.join(format!("bkpr-{}", unsafe { libc::geteuid() }))
            }
        };
        ensure!(runtime.is_absolute(), "runtime directory must be absolute");
        Ok(runtime.join("bkpr/d"))
    }
    /// Resolves the current user's default durable host metadata root.
    pub fn default_state_root() -> Result<PathBuf> {
        let state = match std::env::var_os("XDG_STATE_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(
                std::env::var_os("HOME").context("HOME is unavailable; supply --state-root")?,
            )
            .join(".local/state"),
        };
        ensure!(state.is_absolute(), "state directory must be absolute");
        Ok(state.join("bkpr"))
    }
}

struct HostLease(File);
impl Drop for HostLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
struct Inner {
    config: HostConfig,
    info: DaemonInfo,
    listener: Mutex<Option<BoundSocket>>,
    wake: Mutex<Option<UnixStream>>,
    stop: Stop,
    stopping: AtomicBool,
    sessions: Mutex<BTreeMap<String, Arc<SessionSlot>>>,
    changed: Condvar,
    failure: Mutex<Option<String>>,
    _lease: HostLease,
    #[cfg(test)]
    opening: Mutex<Option<OpeningHook>>,
}
#[cfg(test)]
type OpeningHook = Arc<dyn Fn(&str, &Registry) + Send + Sync>;
struct SessionSlot {
    phase: Mutex<Phase>,
    changed: Condvar,
    stop: Stop,
}
enum Phase {
    Opening,
    Running(SessionEndpoint),
    Failed { message: String, ended: bool },
    Closed,
}

/// One long-lived host owner; an idle host remains available after all sessions end.
///
/// Session startup and cleanup never hold the host map lock. Same-name opens
/// share one opening, while different names progress independently.
#[derive(Clone)]
pub struct HostDaemon {
    inner: Arc<Inner>,
}
impl HostDaemon {
    /// Acquires exclusive host metadata and socket leases without opening sessions.
    ///
    /// Rejects unsafe paths, live listeners, existing owners and storage failures.
    pub fn bind(config: HostConfig) -> Result<Self> {
        ensure!(config.socket.is_absolute(), "host socket must be absolute");
        ensure!(
            config.state_root.is_absolute(),
            "host state root must be absolute"
        );
        registry::private_directory(&config.state_root)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(config.state_root.join("host.lock"))?;
        registry::check_file(&lease)?;
        ensure!(
            std::os::unix::fs::MetadataExt::nlink(&lease.metadata()?) == 1,
            "host lease cannot have hard links"
        );
        lease
            .try_lock_exclusive()
            .context("host state root already owned by another daemon")?;
        let lease = HostLease(lease);
        registry::private_directory(&config.state_root.join("sessions"))?;
        let listener = transport::bind(&config.socket)?;
        let (stop, wake) = Stop::new()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                info: DaemonInfo {
                    daemon_id: Uuid::new_v4().to_string(),
                    pid: std::process::id(),
                },
                listener: Mutex::new(Some(listener)),
                wake: Mutex::new(Some(wake)),
                stop,
                stopping: AtomicBool::new(false),
                sessions: Mutex::new(BTreeMap::new()),
                changed: Condvar::new(),
                failure: Mutex::new(None),
                _lease: lease,
                #[cfg(test)]
                opening: Mutex::new(None),
            }),
        })
    }
    /// Returns the immutable process incarnation advertised by host control.
    pub fn info(&self) -> &DaemonInfo {
        &self.inner.info
    }
    /// Runs host control until explicit shutdown, SIGINT, SIGTERM or serving failure.
    ///
    /// Installs process signals once and drains all session owners before returning.
    /// Host readiness does not imply that an independently opening session is ready.
    pub fn serve(&self) -> Result<()> {
        let listener = self
            .inner
            .listener
            .lock()
            .unwrap()
            .take()
            .context("host already serving")?;
        let wake = self
            .inner
            .wake
            .lock()
            .unwrap()
            .take()
            .context("host already serving")?;
        let serving = listener.serve_host(self.clone(), self.inner.stop.clone(), wake);
        let cleanup = self.shutdown_sessions();
        serving.and(cleanup)
    }
    /// Serves the host while opening one CLI-authorized initial session.
    ///
    /// Bootstrap failure shuts down the host and returns its error; a naturally
    /// ended initial session leaves the host idle rather than ending the process.
    pub fn serve_initial(
        &self,
        name: String,
        defaults: Option<SpawnDefaults>,
        initial: Option<PaneSpawnSpec>,
    ) -> Result<()> {
        let host = self.clone();
        let opening = thread::Builder::new()
            .name("bkpr-initial-session".into())
            .spawn(move || {
                let result = host.open_session(name, defaults, initial);
                match result {
                    Err(error) if error.is::<registry::SessionClosing>() => Ok(()),
                    Err(error) => {
                        host.request_shutdown();
                        Err(error)
                    }
                    Ok(_) => Ok(()),
                }
            })?;
        let serving = self.serve();
        let opened = opening
            .join()
            .map_err(|_| anyhow::anyhow!("initial session worker panicked"))?;
        opened.and(serving)
    }
    /// Requests host-wide shutdown without waiting on a session or storage lock.
    pub fn request_shutdown(&self) {
        self.inner.stop.request();
    }
    pub(crate) fn stop_requested(&self) -> bool {
        self.inner.stop.flag.load(Ordering::Acquire)
    }
    pub(crate) fn request(&self, operation: Operation) -> Result<Payload> {
        let value = match operation {
            Operation::DaemonInfo => json!({"daemon":self.info()}),
            Operation::SessionOpen {
                name,
                defaults,
                initial,
            } => serde_json::to_value(self.open_session(name, defaults, initial)?)?,
            Operation::DaemonShutdown { daemon_id } => {
                ensure!(
                    daemon_id == self.info().daemon_id,
                    "host daemon identity changed"
                );
                self.request_shutdown();
                self.shutdown_sessions()?;
                json!({"shutdown":true})
            }
            _ => bail!("operation requires a session endpoint"),
        };
        Ok(Payload::Control(value))
    }
    /// Opens or joins one named session, admitting explicit bootstrap at most once.
    ///
    /// Defaults and initial execution apply only to a newly opened instance.
    /// Existing active sessions are never reconfigured or bootstrapped. Closing
    /// instances retain their slot until sockets, response drains and root leases
    /// are released. Failed openings return errors to all concurrent waiters.
    /// Malformed names, defaults and initial specs are rejected before admission,
    /// including requests for an already active session.
    pub fn open_session(
        &self,
        name: String,
        defaults: Option<SpawnDefaults>,
        initial: Option<PaneSpawnSpec>,
    ) -> Result<SessionEndpoint> {
        let key = bkpr_protocol::session_key(&name)?;
        if let Some(defaults) = &defaults {
            registry::validate_spawn_defaults(defaults)?;
        }
        if let Some(initial) = &initial {
            registry::validate_spawn_intent(initial)?;
        }
        let mut intent = Some((defaults, initial));
        loop {
            let (slot, opening, retry_closing) = {
                let mut sessions = self.inner.sessions.lock().unwrap();
                ensure!(
                    !self.inner.stopping.load(Ordering::Acquire) && !self.stop_requested(),
                    "host is shutting down"
                );
                if let Some(slot) = sessions.get(&name) {
                    (
                        slot.clone(),
                        None,
                        slot.stop.closing.load(Ordering::Acquire),
                    )
                } else {
                    let (stop, wake) = Stop::new()?;
                    let slot = Arc::new(SessionSlot {
                        phase: Mutex::new(Phase::Opening),
                        changed: Condvar::new(),
                        stop,
                    });
                    sessions.insert(name.clone(), slot.clone());
                    (slot, Some(wake), false)
                }
            };
            if let Some(wake) = opening {
                let (defaults, initial) = intent.take().expect("one opening per request");
                let host = self.clone();
                let session_slot = slot.clone();
                let session_name = name.clone();
                let session_key = key.clone();
                if let Err(error) =
                    thread::Builder::new()
                        .name("bkpr-session".into())
                        .spawn(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    host.run_session(
                                        &session_name,
                                        &session_key,
                                        &session_slot,
                                        wake,
                                        defaults,
                                        initial,
                                    )
                                }))
                                .map_err(|_| anyhow::anyhow!("session service panicked"))
                                .and_then(|result| result);
                            host.finished(&session_name, &session_slot, result);
                        })
                {
                    self.finished(&name, &slot, Err(error.into()));
                }
            }
            let mut phase = slot.phase.lock().unwrap();
            loop {
                match &*phase {
                    Phase::Running(endpoint) if !slot.stop.closing.load(Ordering::Acquire) => {
                        return Ok(endpoint.clone());
                    }
                    Phase::Failed { message, ended } => {
                        if *ended {
                            return Err(
                                registry::SessionClosing("session ended while opening").into()
                            );
                        }
                        bail!("{message}");
                    }
                    Phase::Closed if retry_closing => break,
                    Phase::Closed => {
                        return Err(registry::SessionClosing("session ended while opening").into());
                    }
                    _ => phase = slot.changed.wait(phase).unwrap(),
                }
            }
        }
    }
    fn run_session(
        &self,
        name: &str,
        key: &str,
        slot: &Arc<SessionSlot>,
        wake: UnixStream,
        defaults: Option<SpawnDefaults>,
        initial: Option<PaneSpawnSpec>,
    ) -> Result<()> {
        let runtime = self
            .inner
            .config
            .socket
            .parent()
            .context("host socket needs parent")?;
        let socket = runtime.join(key).join("s");
        match UnixStream::connect(&socket) {
            Ok(_) => bail!(
                "session has a live foreign or old daemon owner; explicitly stop it before reopening"
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) => {}
            Err(error) => return Err(error).context("checking existing session owner"),
        }
        let root = self.inner.config.state_root.join("sessions").join(key);
        let registry = Registry::open_named_in_host(&root, name, &self.info().daemon_id)?;
        let released = registry.lease_released();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            ensure!(
                !slot.stop.flag.load(Ordering::Acquire),
                "host is shutting down"
            );
            if let Some(defaults) = defaults {
                registry.configure_spawn_defaults(defaults)?;
            }
            let listener = transport::bind(&socket)?;
            registry.configure_harnesses(&socket, &self.inner.config.bkpr)?;
            registry.configure_agent_forwarding(&socket)?;
            #[cfg(test)]
            let hook = self.inner.opening.lock().unwrap().clone();
            #[cfg(test)]
            if let Some(hook) = hook {
                hook(name, &registry);
            }
            let endpoint = SessionEndpoint {
                session: registry.session_info().clone(),
                socket,
            };
            let ready_slot = slot.clone();
            let ready = Arc::new(move |result: std::result::Result<(), String>| {
                if let Ok(()) = result {
                    let mut phase = ready_slot.phase.lock().unwrap();
                    if matches!(&*phase, Phase::Opening)
                        && !ready_slot.stop.closing.load(Ordering::Acquire)
                    {
                        *phase = Phase::Running(endpoint.clone());
                        ready_slot.changed.notify_all();
                    }
                }
            });
            listener.serve_session(registry.clone(), initial, slot.stop.clone(), wake, ready)
        }))
        .map_err(|_| anyhow::anyhow!("session service panicked"))
        .and_then(|result| result);
        let cleanup = registry.shutdown();
        drop(registry);
        released.wait();
        match (result, cleanup) {
            (result, Ok(())) => result,
            (result, Err(cleanup)) => {
                self.inner
                    .failure
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| format!("{cleanup:#}"));
                match result {
                    Err(startup) => Err(cleanup.context(format!(
                        "session service failed: {startup:#}; cleanup also failed"
                    ))),
                    Ok(()) => Err(cleanup),
                }
            }
        }
    }
    fn finished(&self, name: &str, slot: &Arc<SessionSlot>, result: Result<()>) {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let mut phase = slot.phase.lock().unwrap();
        let opening = matches!(&*phase, Phase::Opening);
        *phase = match result {
            Err(error) if opening => Phase::Failed {
                message: format!("{error:#}"),
                ended: error.is::<registry::SessionClosing>(),
            },
            Err(error) => {
                self.inner
                    .failure
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| format!("{error:#}"));
                Phase::Closed
            }
            Ok(()) if opening => Phase::Failed {
                message: "session ended before opening completed".into(),
                ended: true,
            },
            Ok(()) => Phase::Closed,
        };
        if sessions
            .get(name)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            sessions.remove(name);
        }
        slot.changed.notify_all();
        self.inner.changed.notify_all();
    }
    pub(crate) fn shutdown_sessions(&self) -> Result<()> {
        let mut sessions = self.inner.sessions.lock().unwrap();
        self.inner.stopping.store(true, Ordering::Release);
        for slot in sessions.values() {
            slot.stop.request();
        }
        sessions = self
            .inner
            .changed
            .wait_while(sessions, |sessions| !sessions.is_empty())
            .unwrap();
        drop(sessions);
        if let Some(error) = self.inner.failure.lock().unwrap().as_ref() {
            bail!("session cleanup failed: {error}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
