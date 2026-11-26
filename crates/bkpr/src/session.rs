//! Local session addressing and daemon bootstrap belong to the launcher, not a frontend.
use anyhow::{Context, Result, bail, ensure};
use bkpr_client::{SpawnDefaults, transport};
use bkpr_protocol::{DaemonInfo, Operation, PaneSpawnSpec, SessionEndpoint, SessionInfo, Size};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;

mod administration;

pub const DEFAULT_SESSION: &str = "bkpr";

pub struct LocalPaths {
    pub socket: PathBuf,
    pub state_root: PathBuf,
    /// None means explicit/headless socket routing, without a claimed session name.
    pub name: Option<String>,
    /// Host namespace for named routes; inherited data sockets have no host route.
    pub host: Option<HostPaths>,
}

/// The single control endpoint and durable root of a local host daemon.
#[derive(Clone)]
pub struct HostPaths {
    pub socket: PathBuf,
    pub state_root: PathBuf,
}
pub struct Roots {
    runtime: PathBuf,
    state: PathBuf,
}

// Resolve only the ambient system base, before adding Bkpr-owned components.
// macOS exposes /tmp through /private/tmp; owned descendants still reject aliases.
fn default_runtime_root(system_tmp: &Path) -> Result<PathBuf> {
    Ok(system_tmp
        .canonicalize()
        .context("resolving system temporary directory")?
        .join(format!("bkpr-{}", unsafe { libc::geteuid() })))
}

impl Roots {
    pub fn inherited() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required")?;
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(path) => PathBuf::from(path),
            None => default_runtime_root(Path::new("/tmp"))?,
        };
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        Ok(Self {
            runtime: crate::cli::absolute(runtime)?.join("bkpr"),
            state: crate::cli::absolute(state)?.join("bkpr/sessions"),
        })
    }

    /// Resolve the host owner without opening a session or creating directories.
    pub fn host(&self) -> Result<HostPaths> {
        Ok(HostPaths {
            socket: self.runtime.join("d"),
            state_root: self
                .state
                .parent()
                .context("host state root missing")?
                .to_owned(),
        })
    }

    pub fn named(&self, name: &str) -> Result<LocalPaths> {
        validate_name(name)?;
        let hash = bkpr_protocol::session_key(name)?;
        let socket = self.runtime.join(&hash).join("s");
        // Both control and its `.agent` companion need a trailing NUL in
        // sockaddr_un.sun_path on Linux/macOS.
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            socket.as_os_str().as_bytes().len() + ".agent".len() < 104,
            "session control/agent socket paths are too long; select a shorter XDG_RUNTIME_DIR"
        );
        Ok(LocalPaths {
            socket,
            state_root: self.state.join(hash),
            name: Some(name.to_owned()),
            host: Some(self.host()?),
        })
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "session name must be nonempty and contain no control characters"
    );
    Ok(())
}

impl LocalPaths {
    /// Explicit names override inherited pane routing. A fixture socket plus
    /// state root selects a private host namespace; a socket alone is data-only.
    pub fn resolve(
        roots: &Roots,
        name: Option<&str>,
        socket: Option<PathBuf>,
        state_root: Option<PathBuf>,
        inherited_socket: Option<PathBuf>,
        headless: bool,
        bootstrap: bool,
    ) -> Result<Self> {
        ensure!(
            name.is_none() || (socket.is_none() && state_root.is_none()),
            "--session cannot be combined with --socket or --state-root"
        );
        ensure!(
            state_root.is_none() || socket.is_some(),
            "--state-root requires --socket"
        );
        if let Some(socket) = socket {
            ensure!(
                !bootstrap || state_root.is_some(),
                "explicit TUI --socket requires --state-root for unambiguous bootstrap"
            );
            let socket = crate::cli::absolute(socket)?;
            if let Some(state_root) = state_root {
                let state_root = crate::cli::absolute(state_root)?;
                let fixture = Roots {
                    runtime: socket
                        .parent()
                        .context("host socket needs parent")?
                        .to_owned(),
                    state: state_root.join("sessions"),
                };
                let mut paths = fixture.named(DEFAULT_SESSION)?;
                paths.host = Some(HostPaths { socket, state_root });
                return Ok(paths);
            }
            return Ok(Self {
                socket,
                state_root: PathBuf::new(),
                name: None,
                host: None,
            });
        }
        if headless
            && name.is_none()
            && let Some(socket) = inherited_socket
        {
            return Ok(Self {
                socket: crate::cli::absolute(socket)?,
                state_root: PathBuf::new(),
                name: None,
                host: None,
            });
        }
        roots.named(name.unwrap_or(DEFAULT_SESSION))
    }
}

fn owned_private(metadata: &std::fs::Metadata) -> bool {
    metadata.uid() == unsafe { libc::geteuid() } && metadata.permissions().mode() & 0o077 == 0
}

/// Refuse symlink traversal, including ancestors, before creating or opening any
/// session-owned file. Existing non-private ancestors (e.g. /tmp) are not changed.
fn directory_chain(path: &Path, create: bool) -> Result<bool> {
    ensure!(path.is_absolute(), "daemon directories must be absolute");
    let mut current = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir),
            "daemon paths cannot contain '..'"
        );
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_dir(),
                    "{} is not a non-symlink directory",
                    current.display()
                );
                let mode = metadata.permissions().mode();
                ensure!(
                    (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
                        && (mode & 0o022 == 0 || (metadata.uid() == 0 && mode & 0o1000 != 0)),
                    "{} has an untrusted owner or writable ancestors",
                    current.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(false);
                }
                match std::fs::DirBuilder::new().mode(0o700).create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                let metadata = std::fs::symlink_metadata(&current)?;
                ensure!(
                    metadata.file_type().is_dir() && owned_private(&metadata),
                    "private daemon directory was replaced during creation"
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn private_directory(path: &Path, create: bool) -> Result<bool> {
    if !directory_chain(path, create)? {
        return Ok(false);
    }
    ensure!(
        owned_private(&std::fs::symlink_metadata(path)?),
        "{} must be a private owned directory (0700)",
        path.display()
    );
    Ok(true)
}

fn private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .append(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.file_type().is_file() && owned_private(&metadata) && metadata.nlink() == 1,
        "{} must be a private owned regular file",
        path.display()
    );
    Ok(file)
}

#[derive(Deserialize)]
struct StoredSession {
    version: u32,
    name: String,
    session_id: String,
}

fn read_manifest(root: &Path) -> Result<Option<StoredSession>> {
    if !private_directory(root, false)? {
        return Ok(None);
    }
    let path = root.join("session.json");
    let file = match private_file(&path, false) {
        Ok(file) => file,
        Err(error) if unavailable(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    ensure!(
        file.metadata()?.len() <= 4 * 1024 * 1024,
        "session manifest exceeds limit"
    );
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "session manifest exceeds limit"
    );
    let stored: StoredSession =
        serde_json::from_slice(&bytes).context("decoding session manifest")?;
    ensure!(
        stored.version == 1 && !stored.session_id.is_empty(),
        "unsupported session manifest"
    );
    validate_name(&stored.name)?;
    Ok(Some(stored))
}

fn unavailable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

fn named_directories(paths: &LocalPaths, create: bool) -> Result<bool> {
    if paths.name.is_none() {
        return Ok(true);
    }
    let runtime = paths
        .socket
        .parent()
        .and_then(Path::parent)
        .context("named runtime root missing")?;
    let state = paths
        .state_root
        .parent()
        .context("named state root missing")?;
    let state_base = state.parent().context("named state base missing")?;
    for directory in [runtime, state_base, state] {
        if !private_directory(directory, create)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn inspect(paths: &LocalPaths) -> Result<SessionInfo> {
    inspect_cancellable(paths, None)
}

fn inspect_cancellable(
    paths: &LocalPaths,
    cancellation: Option<&CancellationToken>,
) -> Result<SessionInfo> {
    let parent = paths.socket.parent().context("socket needs parent")?;
    if !named_directories(paths, false)? || !private_directory(parent, false)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "session runtime directory is absent",
        )
        .into());
    }
    match std::fs::symlink_metadata(&paths.socket) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_socket() && owned_private(&metadata),
            "{} must be a private owned Unix socket",
            paths.socket.display()
        ),
        Err(error) => return Err(error.into()),
    }
    let value = match cancellation {
        Some(cancellation) => {
            transport::checked_cancellable(&paths.socket, Operation::SessionInfo, cancellation)
        }
        None => transport::checked(&paths.socket, Operation::SessionInfo),
    }.context("cannot inspect the selected live session; use bkpr stop for version-independent shutdown before retrying")?;
    let info: SessionInfo = serde_json::from_value(
        value
            .get("session")
            .context("server omitted session identity")?
            .clone(),
    )?;
    if let Some(name) = &paths.name {
        ensure!(
            &info.name == name,
            "server session name does not match selected name"
        );
        let stored =
            read_manifest(&paths.state_root)?.context("named session has no identity manifest")?;
        ensure!(
            &stored.name == name && stored.session_id == info.session_id,
            "server session identity does not match stored named session"
        );
    }
    ensure!(
        !info.session_id.is_empty()
            && !info.daemon_id.is_empty()
            && !info.session_instance_id.is_empty(),
        "session endpoint omitted its complete runtime identity"
    );
    if let Some(host) = &paths.host {
        let owner = inspect_host(host, cancellation).map_err(|error| {
            anyhow::anyhow!(
                "live session has no compatible host owner: {error:#}; explicitly stop the selected session with bkpr stop before retrying"
            )
        })?;
        ensure!(
            owner.daemon_id == info.daemon_id,
            "live session belongs to another daemon; explicitly stop the selected session with bkpr stop before retrying"
        );
    }
    Ok(info)
}

fn validate_endpoint_socket(socket: &Path) -> Result<()> {
    if !private_directory(
        socket.parent().context("endpoint socket needs parent")?,
        false,
    )? {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
    }
    let metadata = std::fs::symlink_metadata(socket)?;
    ensure!(
        metadata.file_type().is_socket() && owned_private(&metadata),
        "endpoint must be a private owned Unix socket"
    );
    Ok(())
}

fn inspect_host(paths: &HostPaths, cancellation: Option<&CancellationToken>) -> Result<DaemonInfo> {
    validate_endpoint_socket(&paths.socket)?;
    let value = match cancellation {
        Some(cancellation) => {
            transport::checked_cancellable(&paths.socket, Operation::DaemonInfo, cancellation)
        }
        None => transport::checked(&paths.socket, Operation::DaemonInfo),
    }
    .context(
        "incompatible live daemon; bkpr daemon stop shuts down this host and all its sessions without requiring a matching wire version",
    )?;
    let info: DaemonInfo = serde_json::from_value(
        value
            .get("daemon")
            .context("endpoint is not a host daemon; explicitly stop the old daemon first")?
            .clone(),
    )?;
    ensure!(
        !info.daemon_id.is_empty() && info.pid > 0,
        "host omitted its runtime identity"
    );
    Ok(info)
}

/// Inspect or explicitly stop the current host incarnation without opening sessions.
pub fn administer(host: &HostPaths, stop: bool) -> Result<serde_json::Value> {
    if stop {
        administration::stop_host(host)
    } else {
        Ok(serde_json::json!({ "daemon": inspect_host(host, None)? }))
    }
}

/// Dispatch one raw host request to an explicitly selected compatible host, without replay.
pub fn request_host(
    socket: &Path,
    request: &bkpr_protocol::Request,
) -> Result<bkpr_protocol::Response> {
    let host = HostPaths {
        socket: socket.to_owned(),
        state_root: PathBuf::new(),
    };
    let daemon = inspect_host(&host, None)?;
    match &request.operation {
        Operation::DaemonInfo | Operation::SessionOpen { .. } => {}
        Operation::Notifications { request } => request.validate()?,
        Operation::DaemonShutdown { daemon_id } => {
            ensure!(
                daemon_id == &daemon.daemon_id,
                "host shutdown incarnation no longer matches"
            );
        }
        _ => bail!("operation requires a session data endpoint"),
    }
    transport::request(socket, request)
}

/// Dispatch a notification request once to an explicit host or session endpoint.
/// Host-only policy and push management inspect host identity and cannot target a session.
pub fn request_notifications(
    socket: &Path,
    request: &bkpr_protocol::Request,
) -> Result<bkpr_protocol::Response> {
    let Operation::Notifications {
        request: notification,
    } = &request.operation
    else {
        bail!("expected a notification request");
    };
    notification.validate()?;
    if notification.host_only() {
        return request_host(socket, request);
    }
    validate_endpoint_socket(socket)?;
    transport::request(socket, request)
}

/// Headless data-plane commands never bootstrap and require a compatible exact
/// named identity. Explicit shutdown uses independent administration instead.
pub fn validate_target(paths: &LocalPaths) -> Result<SessionInfo> {
    inspect(paths)
}

/// Stop only the selected session without requiring its native data-plane version.
pub fn stop(paths: &LocalPaths) -> Result<serde_json::Value> {
    administration::stop_session(paths)
}

#[derive(Serialize)]
pub struct ListedSession {
    name: String,
    #[serde(with = "bkpr_protocol::path_wire")]
    socket: PathBuf,
    #[serde(with = "bkpr_protocol::path_wire")]
    state_root: PathBuf,
    session_id: String,
    running: bool,
    daemon_id: Option<String>,
}

pub fn list(roots: &Roots) -> Result<Vec<ListedSession>> {
    if !private_directory(
        roots.state.parent().context("state root needs parent")?,
        false,
    )? || !private_directory(&roots.state, false)?
    {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(&roots.state)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_dir(),
            "unexpected entry in named session directory"
        );
        let Some(stored) = read_manifest(&entry.path())? else {
            continue;
        };
        let paths = roots.named(&stored.name)?;
        ensure!(
            entry.path() == paths.state_root,
            "session manifest name does not match hashed root"
        );
        let live = match inspect(&paths) {
            Ok(info) => Some(info),
            Err(error) if unavailable(&error) => None,
            Err(error) => return Err(error),
        };
        sessions.push(ListedSession {
            name: stored.name,
            socket: paths.socket,
            state_root: paths.state_root,
            session_id: stored.session_id,
            running: live.is_some(),
            daemon_id: live.map(|info| info.daemon_id),
        });
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

struct LaunchLock(File);
impl Drop for LaunchLock {
    fn drop(&mut self) {
        // An inherited descriptor must not prolong the launch lock in a child.
        let _ = FileExt::unlock(&self.0);
    }
}

/// Only known connection absence permits startup. Protocol/authentication errors
/// on a live server are never hidden by restarting it. A launch lock makes
/// concurrent launchers converge before either process can reset/bootstrap state.
/// This one-shot hook runs in the attachment's bounded connection worker, never
/// on reconnect. Cancellation abandons lock/readiness waits, not daemon ownership:
/// a daemon (and its restored panes) already started here continues independently.
pub fn ensure_server(
    paths: &LocalPaths,
    binary: Option<&Path>,
    defaults: &SpawnDefaults,
    size: Size,
    cancellation: &CancellationToken,
) -> Result<()> {
    ensure!(
        !cancellation.is_cancelled(),
        "local session preparation cancelled"
    );
    let host = paths
        .host
        .as_ref()
        .context("TUI preparation requires a host namespace")?;
    match inspect_cancellable(paths, Some(cancellation)) {
        Ok(_) => {}
        Err(error) if unavailable(&error) => {}
        Err(error) => return Err(error),
    }
    if inspect_host(host, Some(cancellation)).is_err_and(|error| unavailable(&error))
        && private_directory(&paths.state_root, false)?
    {
        match private_file(&paths.state_root.join("owner.lock"), false) {
            Ok(file) => match file.try_lock_exclusive() {
                Ok(()) => FileExt::unlock(&file)?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    bail!(
                        "session storage is owned by a live daemon; explicitly stop it with its matching bkpr before retrying"
                    );
                }
                Err(error) => return Err(error).context("checking existing session owner"),
            },
            Err(error) if unavailable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    let owner = ensure_host(host, binary, cancellation)?;
    let value = transport::checked_cancellable(
        &host.socket,
        Operation::SessionOpen {
            name: paths.name.clone().context("host session needs a name")?,
            defaults: Some(defaults.clone()),
            initial: Some(PaneSpawnSpec {
                argv: defaults.argv.clone(),
                cwd: defaults.cwd.clone(),
                home: defaults.home.clone(),
                isolated_environment: defaults.isolated_environment,
                size,
                title: None,
            }),
        },
        cancellation,
    )?;
    let endpoint: SessionEndpoint = serde_json::from_value(value)?;
    ensure!(
        endpoint.socket == paths.socket && endpoint.session.daemon_id == owner.daemon_id,
        "host opened a session outside the selected namespace or incarnation"
    );
    let observed = inspect_cancellable(paths, Some(cancellation))?;
    ensure!(
        observed == endpoint.session,
        "session changed during preparation"
    );
    Ok(())
}

fn ensure_host(
    paths: &HostPaths,
    binary: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<DaemonInfo> {
    let check_cancelled = || -> Result<()> {
        ensure!(
            !cancellation.is_cancelled(),
            "local session preparation cancelled"
        );
        Ok(())
    };
    check_cancelled()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating local startup wait runtime")?;
    private_directory(paths.socket.parent().context("socket needs parent")?, true)?;
    private_directory(&paths.state_root, true)?;
    let lock = LaunchLock(private_file(&paths.state_root.join("launch.lock"), true)?);
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut retry = Duration::from_millis(25);
        loop {
            check_cancelled()?;
            match lock.0.try_lock_exclusive() {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("locking host bootstrap"),
            }
            // flock has no pollable unlock notification. Bound this kernel-lock
            // retry fallback, back off while idle, and wake immediately on cancel.
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => bail!("local session preparation cancelled"),
                _ = tokio::time::sleep_until(deadline) => {
                    bail!("timed out waiting for the host bootstrap lock");
                }
                _ = tokio::time::sleep(retry) => {}
            }
            retry = (retry * 2).min(Duration::from_millis(500));
        }
    })?;
    check_cancelled()?;
    match inspect_host(paths, Some(cancellation)) {
        Ok(info) => return Ok(info),
        Err(error) if unavailable(&error) => {}
        Err(error) => return Err(error),
    }
    let bkpr = std::env::current_exe().context("locating bkpr for managed harness launches")?;
    let server = match binary {
        Some(binary) => binary.to_owned(),
        None => {
            let sibling = bkpr.with_file_name("bkprd");
            ensure!(
                sibling.is_file(),
                "matching sibling bkprd is absent; use --bkprd PATH"
            );
            sibling
        }
    };
    let log_path = paths.state_root.join("bkprd.log");
    let mut log = private_file(&log_path, true)?;
    let mut command = Command::new(server);
    command
        .arg("--socket")
        .arg(&paths.socket)
        .arg("--state-root")
        .arg(&paths.state_root)
        .arg("--bkpr")
        .arg(&bkpr);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("BKPR_")
            || name.starts_with("ZELLIJ")
            || matches!(
                name.as_ref(),
                "TMUX"
                    | "TMUX_PANE"
                    | "STY"
                    | "WINDOW"
                    | "KITTY_WINDOW_ID"
                    | "WEZTERM_PANE"
                    | "TERM_SESSION_ID"
                    | "ITERM_SESSION_ID"
            )
        {
            command.env_remove(key);
        }
    }
    if let Some(directory) = std::env::var_os("BKPR_TERMINFO_DIR") {
        command.env("BKPR_TERMINFO_DIR", directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log.try_clone()?));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    check_cancelled()?;
    let mut child = {
        let _entered = runtime.enter();
        tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()
            .context("starting bkprd")?
    };
    const MAX_READY_BYTES: usize = 8192;
    let stdout = child.stdout.take().context("opening bkprd stdout")?;
    let mut output = BufReader::new(stdout.take((MAX_READY_BYTES + 1) as u64));
    let mut record = Vec::with_capacity(512);
    let exited = runtime.block_on(async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("local session preparation cancelled"),
            result = async {
                output.read_until(b'\n', &mut record).await?;
                if record.is_empty() {
                    return Ok(Some(child.wait().await?));
                }
                Ok::<_, std::io::Error>(None)
            } => Ok(result?),
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                bail!("bkprd did not report readiness; see {}", log_path.display());
            }
        }
    })?;
    if !record.is_empty() {
        log.write_all(&record)?;
        ensure!(
            record.len() <= MAX_READY_BYTES,
            "bkprd startup output exceeded its bound"
        );
        ensure!(record.ends_with(b"\n"), "incomplete bkprd startup record");
        #[derive(Deserialize)]
        struct Ready {
            version: u32,
            ready: bool,
            #[serde(with = "bkpr_protocol::path_wire")]
            socket: PathBuf,
        }
        let ready: Ready =
            serde_json::from_slice(&record).context("decoding bkprd readiness record")?;
        ensure!(
            ready.version == bkpr_protocol::PROTOCOL_VERSION
                && ready.ready
                && ready.socket == paths.socket,
            "bkprd readiness did not identify the selected socket and protocol"
        );
    }
    // Exactly one authoritative owner validation follows readiness (or an exit
    // where another launcher may have won the daemon lease). Never reconnect by
    // replaying bootstrap and never kill a started daemon on wait cancellation.
    match inspect_host(paths, Some(cancellation)) {
        Ok(info) => Ok(info),
        Err(error) => match exited {
            Some(status) => Err(error)
                .with_context(|| format!("bkprd exited ({status}); see {}", log_path.display())),
            None => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn private_fixture() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("bs-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(Path::new("/tmp").canonicalize().unwrap())
            .unwrap()
    }

    fn roots(root: &Path) -> Roots {
        Roots {
            runtime: root.join("r"),
            state: root.join("state/sessions"),
        }
    }

    struct HostFixture {
        child: std::process::Child,
        roots: Roots,
        home: PathBuf,
        _directory: tempfile::TempDir,
    }

    impl HostFixture {
        fn start() -> Self {
            use std::io::BufRead;
            use std::os::fd::AsRawFd;
            let directory = private_fixture();
            let roots = roots(directory.path());
            let host = roots.host().unwrap();
            let home = directory.path().join("home");
            let daemon_home = directory.path().join("daemon-home");
            private_directory(&home, true).unwrap();
            private_directory(&daemon_home, true).unwrap();
            let server = std::env::var_os("BKPR_TEST_SERVER")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("bkprd")
                });
            assert!(
                server.is_file(),
                "build matching bkprd or set BKPR_TEST_SERVER"
            );
            let mut child = Command::new(server)
                .env_clear()
                .env("HOME", &daemon_home)
                .env(
                    "PATH",
                    std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
                )
                .env("TERM", "xterm-256color")
                .args(["--socket"])
                .arg(&host.socket)
                .args(["--state-root"])
                .arg(&host.state_root)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let output = child.stdout.take().unwrap();
            let mut poll = libc::pollfd {
                fd: output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 10_000) } != 1 {
                let _ = child.kill();
                let _ = child.wait();
                panic!("host did not report readiness");
            }
            let mut ready = String::new();
            std::io::BufReader::new(output)
                .read_line(&mut ready)
                .unwrap();
            let mut fixture = Self {
                child,
                roots,
                home,
                _directory: directory,
            };
            let record: serde_json::Value = serde_json::from_str(&ready).unwrap();
            assert_eq!(record["ready"], true);
            assert!(fixture.child.try_wait().unwrap().is_none());
            fixture
        }

        fn prepare(&self, name: &str) -> SessionInfo {
            let paths = self.roots.named(name).unwrap();
            let defaults = SpawnDefaults {
                argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                cwd: self._directory.path().to_owned(),
                home: self.home.clone(),
                isolated_environment: true,
            };
            ensure_server(
                &paths,
                Some(Path::new("/must-not-launch-another-bkprd")),
                &defaults,
                Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
                &CancellationToken::new(),
            )
            .unwrap();
            validate_target(&paths).unwrap()
        }
    }

    impl Drop for HostFixture {
        fn drop(&mut self) {
            if let Ok(host) = self.roots.host() {
                let _ = administer(&host, true);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }

    #[test]
    fn local_preparation_reuses_one_host_and_preserves_other_sessions() {
        let fixture = HostFixture::start();
        let first = fixture.prepare("alpha");
        let second = fixture.prepare("beta");
        assert_eq!(first.daemon_id, second.daemon_id);
        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.session_instance_id, second.session_instance_id);
        assert_eq!(fixture.prepare("alpha"), first);
        let host = fixture.roots.host().unwrap();
        assert_eq!(inspect_host(&host, None).unwrap().pid, fixture.child.id());
        let alpha = fixture.roots.named("alpha").unwrap();
        stop(&alpha).unwrap();
        assert_eq!(
            validate_target(&fixture.roots.named("beta").unwrap()).unwrap(),
            second
        );
        let listed = list(&fixture.roots).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .find(|session| session.name == "beta")
                .unwrap()
                .running
        );
    }

    #[test]
    fn host_shutdown_rejects_stale_incarnation_without_stopping_sessions() {
        let fixture = HostFixture::start();
        let session = fixture.prepare("kept");
        let host = fixture.roots.host().unwrap();
        assert!(
            transport::checked(
                &host.socket,
                Operation::DaemonShutdown {
                    daemon_id: format!("{}-stale", session.daemon_id),
                }
            )
            .is_err()
        );
        assert_eq!(
            validate_target(&fixture.roots.named("kept").unwrap()).unwrap(),
            session
        );
        assert_eq!(
            administer(&host, false).unwrap()["daemon"]["pid"],
            fixture.child.id()
        );
        let socket = fixture.roots.named("kept").unwrap().socket;
        assert!(
            transport::checked(
                &socket,
                Operation::ClientRegister {
                    session_id: session.session_id.clone(),
                    daemon_id: session.daemon_id.clone(),
                    session_instance_id: format!("{}-stale", session.session_instance_id),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_raw_host_open_is_headless_and_checks_shutdown_incarnation() {
        let fixture = HostFixture::start();
        let host = fixture.roots.host().unwrap();
        let response = request_host(
            &host.socket,
            &bkpr_protocol::Request::new(Operation::SessionOpen {
                name: "headless".into(),
                defaults: None,
                initial: None,
            }),
        )
        .unwrap();
        assert!(response.ok);
        let paths = fixture.roots.named("headless").unwrap();
        let info = validate_target(&paths).unwrap();
        let snapshot = transport::checked(&paths.socket, Operation::Inspect).unwrap();
        assert!(
            snapshot["registry"]["workspaces"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            request_host(
                &host.socket,
                &bkpr_protocol::Request::new(Operation::DaemonShutdown {
                    daemon_id: format!("{}-stale", info.daemon_id)
                },)
            )
            .is_err()
        );
        assert_eq!(validate_target(&paths).unwrap(), info);
    }

    #[test]
    fn cancellation_releases_the_single_host_launch_lock_without_starting() {
        let directory = private_fixture();
        let host = roots(directory.path()).host().unwrap();
        private_directory(&host.state_root, true).unwrap();
        let lock = LaunchLock(private_file(&host.state_root.join("launch.lock"), true).unwrap());
        lock.0.lock_exclusive().unwrap();
        let cancellation = CancellationToken::new();
        let worker_token = cancellation.clone();
        let worker_host = host.clone();
        let worker = std::thread::spawn(move || {
            ensure_host(
                &worker_host,
                Some(Path::new("/must-not-start")),
                &worker_token,
            )
        });
        cancellation.cancel();
        assert!(worker.join().unwrap().is_err());
        assert!(!host.socket.exists());
        drop(lock);
        let next = private_file(&host.state_root.join("launch.lock"), false).unwrap();
        next.try_lock_exclusive().unwrap();
        FileExt::unlock(&next).unwrap();
    }

    #[test]
    fn explicit_fixture_routes_through_its_private_host_and_default_session() {
        let directory = private_fixture();
        let normal = roots(directory.path());
        let host_socket = directory.path().join("private/d");
        let host_state = directory.path().join("private-state");
        let fixture = LocalPaths::resolve(
            &normal,
            None,
            Some(host_socket.clone()),
            Some(host_state.clone()),
            Some("/ignored/inherited".into()),
            false,
            true,
        )
        .unwrap();
        let key = bkpr_protocol::session_key(DEFAULT_SESSION).unwrap();
        assert_eq!(
            fixture.socket,
            host_socket.parent().unwrap().join(&key).join("s")
        );
        assert_eq!(fixture.state_root, host_state.join("sessions").join(key));
        assert_eq!(fixture.host.unwrap().socket, host_socket);
        assert!(!host_state.exists());
        assert!(!directory.path().join("private").exists());
    }

    #[test]
    fn live_session_lease_refuses_host_start_even_without_a_data_socket() {
        let directory = private_fixture();
        let paths = roots(directory.path()).named("legacy").unwrap();
        private_directory(&paths.state_root, true).unwrap();
        let lease = LaunchLock(private_file(&paths.state_root.join("owner.lock"), true).unwrap());
        lease.0.lock_exclusive().unwrap();
        let defaults = SpawnDefaults {
            argv: vec!["/bin/sh".into()],
            cwd: directory.path().to_owned(),
            home: directory.path().to_owned(),
            isolated_environment: true,
        };
        let error = ensure_server(
            &paths,
            Some(Path::new("/must-not-start")),
            &defaults,
            Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("owned by a live daemon"));
        assert!(!paths.host.unwrap().state_root.join("bkprd.log").exists());
    }

    #[test]
    fn names_are_exact_and_traversal_cannot_escape_hashed_roots() {
        let roots = roots(Path::new("/tmp/bkpr-test"));
        let mut sockets = std::collections::HashSet::new();
        for name in [
            "bkpr",
            " BKPR ",
            "λ / 🐝",
            ".",
            "..",
            "a/b",
            "a_b",
            "foo:bar",
            "é",
            "e\u{301}",
        ] {
            let paths = roots.named(name).unwrap();
            assert_eq!(paths.name.as_deref(), Some(name));
            assert_eq!(
                paths.socket.parent().unwrap().parent().unwrap(),
                roots.runtime
            );
            assert_eq!(paths.state_root.parent().unwrap(), roots.state);
            assert!(sockets.insert(paths.socket));
        }
        for name in ["", "\n", "hello\0world", "x\u{7f}"] {
            assert!(roots.named(name).is_err());
        }
    }

    #[test]
    fn explicit_names_override_inherited_routes_and_fixtures_are_unambiguous() {
        let roots = roots(Path::new("/tmp/bkpr-test"));
        let inherited = Some(PathBuf::from("/tmp/inherited.sock"));
        let named = LocalPaths::resolve(
            &roots,
            Some("chosen"),
            None,
            None,
            inherited.clone(),
            true,
            false,
        )
        .unwrap();
        assert_eq!(named.socket, roots.named("chosen").unwrap().socket);
        let pane =
            LocalPaths::resolve(&roots, None, None, None, inherited.clone(), true, false).unwrap();
        assert_eq!(Some(&pane.socket), inherited.as_ref());
        let bare = LocalPaths::resolve(
            &roots,
            None,
            None,
            None,
            Some("/tmp/other".into()),
            false,
            true,
        )
        .unwrap();
        assert_eq!(bare.name.as_deref(), Some(DEFAULT_SESSION));
        assert!(
            LocalPaths::resolve(
                &roots,
                Some("x"),
                Some("/tmp/s".into()),
                None,
                None,
                false,
                true
            )
            .is_err()
        );
        assert!(
            LocalPaths::resolve(&roots, None, Some("/tmp/s".into()), None, None, false, true)
                .is_err()
        );
        assert!(
            LocalPaths::resolve(
                &roots,
                None,
                None,
                Some("/tmp/state".into()),
                None,
                false,
                true
            )
            .is_err()
        );
    }

    #[test]
    fn discovery_is_read_only_and_rejects_manifest_misrouting() {
        let directory = private_fixture();
        let roots = roots(directory.path());
        assert!(list(&roots).unwrap().is_empty());
        assert!(!roots.state.exists());
        let paths = roots.named("stopped").unwrap();
        private_directory(&roots.state, true).unwrap();
        private_directory(&paths.state_root, true).unwrap();
        let manifest = paths.state_root.join("session.json");
        std::fs::write(
            &manifest,
            br#"{"version":1,"name":"stopped","session_id":"logical"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        let sessions = list(&roots).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].running);
        assert!(!roots.runtime.exists());
        std::fs::write(
            &manifest,
            br#"{"version":1,"name":"different","session_id":"logical"}"#,
        )
        .unwrap();
        assert!(list(&roots).is_err());
    }

    #[test]
    fn private_files_and_directories_never_follow_symlinks_or_relax_permissions() {
        let directory = private_fixture();
        let unrelated = directory.path().join("unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        let alias = directory.path().join("alias");
        symlink(&unrelated, &alias).unwrap();
        assert!(private_directory(&alias.join("new"), true).is_err());
        assert!(!unrelated.join("new").exists());
        let file = unrelated.join("file");
        std::fs::write(&file, b"keep").unwrap();
        let link = directory.path().join("file-link");
        symlink(&file, &link).unwrap();
        assert!(private_file(&link, true).is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"keep");
        std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&unrelated, true).is_err());
        assert_eq!(
            std::fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    fn reply_once(socket: &Path, result: serde_json::Value) -> std::thread::JoinHandle<()> {
        reply_operation_once(socket, result, Operation::SessionInfo)
    }

    fn reply_operation_once(
        socket: &Path,
        result: serde_json::Value,
        expected: Operation,
    ) -> std::thread::JoinHandle<()> {
        use std::os::{fd::AsRawFd, unix::net::UnixListener};
        let listener = UnixListener::bind(socket).unwrap();
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::thread::spawn(move || {
            let mut ready = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut ready, 1, 3000) },
                1,
                "fixture did not receive the expected session handshake"
            );
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let bytes = bkpr_protocol::wire::read_frame(
                &mut stream,
                bkpr_protocol::wire::Kind::Request,
                8 * 1024 * 1024,
            )
            .unwrap();
            let mut codec = bkpr_protocol::wire::Codec::new();
            let request = bkpr_protocol::wire::decode_request(&bytes).unwrap();
            assert_eq!(
                serde_json::to_value(request.operation).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            let response = bkpr_protocol::Response {
                version: bkpr_protocol::PROTOCOL_VERSION,
                ok: true,
                result: Some(result.into()),
                error: None,
                failure: None,
            };
            let bytes = codec.encode_response(&response).unwrap();
            bkpr_protocol::wire::write_frame(
                &mut stream,
                bkpr_protocol::wire::Kind::Response,
                bytes,
            )
            .unwrap();
        })
    }

    #[test]
    fn explicit_notification_reads_dispatch_once_without_session_selection() {
        use bkpr_protocol::{NotificationQuery, NotificationRequest, Request};
        for notification in [
            NotificationRequest::Inspect {
                query: NotificationQuery::default(),
                after: None,
            },
            NotificationRequest::Subscribe {
                query: NotificationQuery::default(),
                after: None,
                wait_ms: 0,
            },
            NotificationRequest::PolicyGet {},
            NotificationRequest::Health {},
        ] {
            let directory = private_fixture();
            let socket = directory.path().join("s");
            let operation = Operation::Notifications {
                request: notification,
            };
            let server = reply_operation_once(
                &socket,
                serde_json::json!({"selected":"exact-endpoint"}),
                operation.clone(),
            );
            let response = request_notifications(&socket, &Request::new(operation)).unwrap();
            assert!(response.ok);
            assert_eq!(
                response.result.unwrap().into_json().unwrap()["selected"],
                "exact-endpoint"
            );
            server.join().unwrap();
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn explicit_notification_policy_write_requires_a_verified_host_before_mutation() {
        use bkpr_protocol::{NotificationPolicy, NotificationRequest, Request};
        let directory = private_fixture();
        let socket = directory.path().join("s");
        let server = reply_operation_once(
            &socket,
            serde_json::json!({"session":{"session_id":"not-a-host"}}),
            Operation::DaemonInfo,
        );
        let request = Request::new(Operation::Notifications {
            request: NotificationRequest::PolicySet {
                policy: NotificationPolicy::default(),
            },
        });
        assert!(request_notifications(&socket, &request).is_err());
        server.join().unwrap();
    }

    #[test]
    fn named_paths_reserve_portable_space_for_the_agent_companion() {
        for (runtime_bytes, accepted) in [(30, true), (31, false)] {
            let roots = Roots {
                runtime: PathBuf::from(format!("/{}", "r".repeat(runtime_bytes - 1))),
                state: PathBuf::from("/unused"),
            };
            assert_eq!(roots.named("agent-budget").is_ok(), accepted);
        }
    }

    #[test]
    fn exact_live_identity_is_required_before_named_attachment() {
        let directory = private_fixture();
        let roots = roots(directory.path());
        let paths = roots.named("selected").unwrap();
        named_directories(&paths, true).unwrap();
        private_directory(paths.socket.parent().unwrap(), true).unwrap();
        private_directory(&paths.state_root, true).unwrap();
        let manifest = paths.state_root.join("session.json");
        std::fs::write(
            &manifest,
            br#"{"version":1,"name":"selected","session_id":"logical"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        for (name, session_id, accepted) in [
            ("selected", "logical", false),
            ("unrelated", "logical", false),
            ("selected", "replacement", false),
        ] {
            let server = reply_once(
                &paths.socket,
                serde_json::json!({
                    "session": {"name":name, "session_id":session_id, "daemon_id":"birth", "session_instance_id":"instance"}
                }),
            );
            assert_eq!(validate_target(&paths).is_ok(), accepted);
            server.join().unwrap();
            std::fs::remove_file(&paths.socket).unwrap();
        }
    }

    #[test]
    fn live_protocol_failure_never_falls_back_to_starting_another_daemon() {
        let directory = private_fixture();
        let paths = LocalPaths {
            socket: directory.path().join("socket"),
            state_root: directory.path().join("state"),
            name: None,
            host: Some(HostPaths {
                socket: directory.path().join("d"),
                state_root: directory.path().join("host"),
            }),
        };
        let server = reply_once(
            &paths.socket,
            serde_json::json!({"not_session_identity":true}),
        );
        let defaults = SpawnDefaults {
            argv: vec!["/bin/sh".into()],
            cwd: directory.path().to_owned(),
            home: directory.path().to_owned(),
            isolated_environment: true,
        };
        let size = Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        assert!(
            ensure_server(
                &paths,
                Some(Path::new("/nonexistent-bkprd")),
                &defaults,
                size,
                &CancellationToken::new(),
            )
            .is_err()
        );
        server.join().unwrap();
        assert!(!paths.state_root.join("bkprd.log").exists());
    }

    #[test]
    fn concurrent_launcher_lease_releases_explicitly_with_its_guard() {
        let directory = private_fixture();
        let path = directory.path().join("launch.lock");
        let first = LaunchLock(private_file(&path, true).unwrap());
        first.0.lock_exclusive().unwrap();
        let second = private_file(&path, true).unwrap();
        assert_eq!(
            second.try_lock_exclusive().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(first);
        second.try_lock_exclusive().unwrap();
        FileExt::unlock(&second).unwrap();
    }

    #[test]
    fn system_tmp_alias_is_resolved_before_owned_descendants_are_checked() {
        let fixture = private_fixture();
        let base = fixture.path().join("real-tmp");
        private_directory(&base, true).unwrap();
        let alias = fixture.path().join("tmp");
        symlink(&base, &alias).unwrap();
        let owned = default_runtime_root(&alias).unwrap();
        private_directory(&owned, true).unwrap();
        assert!(owned.starts_with(&base));
        std::fs::remove_dir(&owned).unwrap();
        let unrelated = fixture.path().join("unrelated");
        private_directory(&unrelated, true).unwrap();
        symlink(&unrelated, &owned).unwrap();
        assert!(private_directory(&owned, true).is_err());
    }
}
