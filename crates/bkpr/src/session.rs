//! Local session addressing and daemon bootstrap belong to the launcher, not a frontend.
use anyhow::{Context, Result, bail, ensure};
use bkpr_client::{Options, transport};
use bkpr_protocol::{Operation, SessionInfo, Size};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const DEFAULT_SESSION: &str = "bkpr";

pub struct LocalPaths {
    pub socket: PathBuf,
    pub state_root: PathBuf,
    /// None means explicit/headless socket routing, without a claimed session name.
    pub name: Option<String>,
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

    pub fn named(&self, name: &str) -> Result<LocalPaths> {
        validate_name(name)?;
        let hash = name_hash(name);
        let socket = self.runtime.join(&hash).join("s");
        // Leave room for the trailing NUL in sockaddr_un.sun_path on Linux/macOS.
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            socket.as_os_str().as_bytes().len() < 104,
            "session socket path is too long; select a shorter XDG_RUNTIME_DIR"
        );
        Ok(LocalPaths {
            socket,
            state_root: self.state.join(hash),
            name: Some(name.to_owned()),
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

fn name_hash(name: &str) -> String {
    format!("{:x}", Sha256::digest(name.as_bytes()))
}

impl LocalPaths {
    /// Explicit session names always override inherited pane routing. Normal TUI
    /// and stop commands use the default named session; only headless pane tools
    /// inherit BKPR_SOCKET. Explicit fixture paths never select a named root.
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
            return Ok(Self {
                socket: crate::cli::absolute(socket)?,
                state_root: state_root
                    .map(crate::cli::absolute)
                    .transpose()?
                    .unwrap_or_default(),
                name: None,
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
    let value = transport::checked(&paths.socket, Operation::SessionInfo)?;
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
    Ok(info)
}

/// Headless commands never bootstrap. Validate exact named identity before their
/// request, including stop, instead of trusting the socket filename alone.
pub fn validate_target(paths: &LocalPaths) -> Result<SessionInfo> {
    inspect(paths)
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
pub fn ensure_server(
    paths: &LocalPaths,
    binary: Option<&Path>,
    options: &Options,
    size: Size,
) -> Result<()> {
    named_directories(paths, true)?;
    private_directory(paths.socket.parent().context("socket needs parent")?, true)?;
    private_directory(&paths.state_root, true)?;
    let lock = LaunchLock(private_file(&paths.state_root.join("launch.lock"), true)?);
    lock.0
        .lock_exclusive()
        .context("locking session bootstrap")?;
    if let Some(name) = &paths.name
        && let Some(stored) = read_manifest(&paths.state_root)?
    {
        ensure!(
            &stored.name == name,
            "stored session name does not match selected name"
        );
    }
    match inspect(paths) {
        Ok(_) => return Ok(()),
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
    let log = private_file(&log_path, true)?;
    let mut command = Command::new(server);
    command
        .arg("--socket")
        .arg(&paths.socket)
        .arg("--state-root")
        .arg(&paths.state_root)
        .arg(format!(
            "--session={}",
            paths.name.as_deref().unwrap_or(DEFAULT_SESSION)
        ))
        .arg("--bkpr")
        .arg(&bkpr)
        .arg("--cwd")
        .arg(&options.cwd)
        .arg("--cols")
        .arg(size.cols.to_string())
        .arg("--rows")
        .arg(size.rows.to_string())
        .arg("--cell-width")
        .arg(size.cell_width.to_string())
        .arg("--cell-height")
        .arg(size.cell_height.to_string());
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
    if options.isolated_environment {
        command.arg("--home").arg(&options.home);
    }
    command
        .arg("--")
        .args(&options.shell)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("starting bkprd")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match inspect(paths) {
            Ok(_) => return Ok(()),
            Err(error) if unavailable(&error) => {}
            Err(error) => return Err(error),
        }
        if let Some(status) = child.try_wait()? {
            // A daemon started by another owner may have won the backend lease.
            if inspect(paths).is_ok() {
                return Ok(());
            }
            bail!("bkprd exited ({status}); see {}", log_path.display());
        }
        if Instant::now() >= deadline {
            bail!(
                "bkprd did not publish its local socket; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
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
            state: root.join("state"),
        }
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
        use std::{
            io::{BufRead, BufReader, Write},
            os::{fd::AsRawFd, unix::net::UnixListener},
        };
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
            let mut request = String::new();
            BufReader::new(&mut stream).read_line(&mut request).unwrap();
            let request: bkpr_protocol::Request = serde_json::from_str(&request).unwrap();
            assert!(matches!(request.operation, Operation::SessionInfo));
            serde_json::to_writer(
                &mut stream,
                &serde_json::json!({
                    "version": bkpr_protocol::PROTOCOL_VERSION, "ok": true, "result": result
                }),
            )
            .unwrap();
            stream.write_all(b"\n").unwrap();
        })
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
            ("selected", "logical", true),
            ("unrelated", "logical", false),
            ("selected", "replacement", false),
        ] {
            let server = reply_once(
                &paths.socket,
                serde_json::json!({
                    "session": {"name":name, "session_id":session_id, "daemon_id":"birth"}
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
        };
        let server = reply_once(
            &paths.socket,
            serde_json::json!({"not_session_identity":true}),
        );
        let options = Options {
            read_only: false,
            detach_others: false,
            clipboard_policy: "deny".into(),
            shell: vec!["/bin/sh".into()],
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
                &options,
                size
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
