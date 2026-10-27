//! Opt-in installed adapters rendezvous by verified native process birth, never
//! by CWD, terminal title, session-id guesses, or inherited parent credentials.
use std::ffi::{CString, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::agent::NativeRoot;
use crate::relay::RelayLaunch;

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    harness: String,
    role: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<OsString>,
}

fn role_command(
    harness: &str,
    role: &str,
    arguments: &[OsString],
) -> Result<(Vec<OsString>, Option<u32>)> {
    let command = match (harness, role) {
        ("claude" | "omp" | "agy", "mcp") => "relay-mcp",
        ("claude", "claude-title") => "claude-title",
        ("claude", "claude-wait") => "claude-wait",
        ("claude", "claude-inbox") => "claude-inbox",
        ("agy", "agy-receiver") => "agy-receiver",
        ("omp", "omp-receiver") => "relay-receiver",
        ("omp", "bind-session") => "relay-bind-session",
        _ => bail!("unsupported installed harness/role combination"),
    };
    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(OsString::from(command));
    argv.extend_from_slice(arguments);
    // Reuse the actual role parsers, including their rejection of unknown flags,
    // before claiming any identity or starting a broker.
    let parsed = crate::Cli::try_parse_from(
        std::iter::once(OsString::from("bkpr")).chain(argv.iter().cloned()),
    )?;
    let receiver_pid = match parsed.command {
        Some(crate::Commands::RelayReceiver { pid, .. }) => Some(pid),
        _ => None,
    };
    Ok((argv, receiver_pid))
}

pub(crate) fn run(args: Args) -> Result<()> {
    let (argv, receiver_pid) = role_command(&args.harness, &args.role, &args.arguments)?;
    let Some(root) = crate::agent::installed_ancestor(&args.harness)? else {
        if args.role == "mcp" {
            bail!("installed Bkpr MCP requires a live managed Bkpr pane");
        }
        // Ordinary installed hooks remain harmless outside Bkpr. No unbound
        // global agent or persistent launch secret is made.
        return Ok(());
    };
    let public_program = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .context("missing installed public Bkpr executable path")?;
    crate::install::validate_public_program(&public_program)?;
    if receiver_pid.is_some_and(|pid| pid != root.pid as u32) {
        bail!("Omp receiver PID does not match its verified native harness root");
    }
    let record = claim(&root)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(argv);
    // Do not mutate the parent harness environment, and never carry a parent's
    // credentials or callback state through the bootstrap boundary.
    for (key, _) in std::env::vars_os() {
        if key.as_bytes().starts_with(b"BKPR_RELAY_") || key.as_bytes().starts_with(b"BKPR_CLAUDE_")
        {
            command.env_remove(key);
        }
    }
    record.launch.apply(&mut command);
    command.env("BKPR_INSTALLED_INTEGRATION", &root.harness);
    command.env("BKPR_INSTALLED_PROGRAM", public_program);
    if root.harness == "claude" {
        let runtime = registry_path()?.join(&record.launch.agent_id);
        Directory::open(&runtime, true)?;
        crate::claude::prepare_installed(
            &mut command,
            &runtime,
            crate::agent::claude_resumed(&root)?,
        )?;
        command.env("BKPR_CLAUDE_TTY", crate::agent::root_terminal(&root)?);
    }
    ensure_native_association(&root, &record.launch.agent_id)?;
    Err(command.exec()).context("execute installed Bkpr adapter")
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    root: NativeRoot,
    launch: RelayLaunch,
}

fn association_published(
    root: &NativeRoot,
    agent_id: &str,
    session: &crate::context::LiveSession,
) -> Result<bool> {
    if session.session_id != root.session_id || session.server != root.server {
        bail!("native owner publication no longer belongs to the verified harness root");
    }
    let mut matches = session
        .panes
        .iter()
        .filter(|pane| pane.agent_id.as_deref() == Some(agent_id));
    let associated = matches.next().is_some();
    if matches.next().is_some() {
        bail!("native owner published an ambiguous installed agent association");
    }
    Ok(associated)
}

fn ensure_native_association(root: &NativeRoot, agent_id: &str) -> Result<()> {
    let change = bkpr::agent_contract::AgentRegistryChanged {
        session_id: root.session_id.clone(),
        agent_id: agent_id.to_owned(),
    };
    await_association(
        Duration::from_secs(15),
        || {
            let session = crate::agent::native_session(root)?
                .context("verified harness root exited, reparented, or lost its live native owner")?;
            let associated = association_published(root, agent_id, &session)?;
            Ok((session, associated))
        },
        |session| crate::control::notify_agent_registry_changed(session, &change),
    ).context("installed Bkpr integration requires a fresh live managed session with installed-agent registry support")
}

fn await_association(
    timeout: Duration,
    mut observe: impl FnMut() -> Result<(crate::context::LiveSession, bool)>,
    notify: impl FnOnce(&crate::context::LiveSession) -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let (session, associated) = observe()?;
    if associated {
        return Ok(());
    }
    // Every unbound helper sends once, including a claimant following one that
    // died after publication. CLI acknowledgement only accepts invalidation;
    // attachment is proved separately by the authenticated owner publication.
    notify(&session)?;
    loop {
        let (_, associated) = observe()?;
        if associated {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for the native owner to publish this exact installed agent");
        }
        // Startup-only publication barrier, not cache-refresh polling: the
        // single push above is what invalidates stale native inspection.
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

fn registry_path() -> Result<PathBuf> {
    Ok(crate::launcher::runtime_path()?.join("installed"))
}

fn record_name(pid: i32) -> String {
    format!("{pid}.json")
}

fn claim(root: &NativeRoot) -> Result<Record> {
    let path = registry_path()?;
    let directory = Directory::open(&path, true)?;
    let _lock = directory
        .lock(false)?
        .context("installed rendezvous lock unavailable")?;
    prune(&directory, &path)?;
    claim_with(
        &directory,
        root,
        || {
            if let Some(launch) = crate::agent::root_launch(root) {
                return Ok(launch);
            }
            crate::relay::prepare_for_process(
                &root.harness,
                &crate::agent::root_directory(root)?,
                root.pid as u32,
                &root.identity,
            )
        },
        |record| {
            crate::relay::verify_launch(&record.launch, root.pid as u32)?;
            if !crate::agent::revalidate_native_root(root)? {
                bail!("native harness root changed during installed bootstrap");
            }
            Ok(())
        },
    )
}

fn claim_with(
    directory: &Directory,
    root: &NativeRoot,
    prepare: impl FnOnce() -> Result<RelayLaunch>,
    verify: impl Fn(&Record) -> Result<()>,
) -> Result<Record> {
    let name = record_name(root.pid);
    if let Some(record) = directory.read_optional::<Record>(&name)? {
        if record.version == 1 && record.root == *root && record.launch.harness == root.harness {
            verify(&record)?;
            return Ok(record);
        }
        // A PID can be reused, but never reuse the exited launch's inbox.
        if crate::agent::root_alive(&record.root)? {
            bail!("conflicting live installed rendezvous for this process");
        }
    }
    let record = Record {
        version: 1,
        root: root.clone(),
        launch: prepare()?,
    };
    verify(&record)?;
    directory.replace(&name, &record)?;
    Ok(record)
}

/// Read-only native inspector lookup. The caller supplies a freshly verified
/// exact root; a stale record, PID reuse, or another harness grants no binding.
pub(crate) fn lookup(root: &NativeRoot) -> Result<Option<String>> {
    let directory = match Directory::open(&registry_path()?, false) {
        Ok(directory) => directory,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    lookup_in(&directory, root)
}

fn lookup_in(directory: &Directory, root: &NativeRoot) -> Result<Option<String>> {
    let Some(record) = directory.read_optional::<Record>(&record_name(root.pid))? else {
        return Ok(None);
    };
    if record.version != 1 || record.root != *root || record.launch.harness != root.harness {
        return Ok(None);
    }
    validate_id(&record.launch.agent_id)?;
    // Do not RPC back into a broker while its native context is being inspected.
    // Registration was authenticated before this atomic private publication.
    Ok(Some(record.launch.agent_id))
}

fn prune(directory: &Directory, path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name
            .strip_suffix(".json")
            .is_some_and(|pid| pid.parse::<i32>().is_ok_and(|pid| pid > 0))
        {
            continue;
        }
        let record: Record = directory.read(&name)?;
        if !crate::agent::root_alive(&record.root)? {
            remove_record(directory, path, &name, &record)?;
        }
    }
    Ok(())
}

/// Called by broker liveness maintenance. A nonblocking lock avoids inversion
/// with bootstrap's register RPC; maintenance retries exited launches next time.
pub(crate) fn retire(pid: u32, agent_id: &str) -> Result<()> {
    let path = registry_path()?;
    let directory = match Directory::open(&path, false) {
        Ok(directory) => directory,
        Err(error) if missing(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_id(agent_id)?;
    let name = record_name(pid as i32);
    let temporary = format!(".claim-{agent_id}");
    let pending = match directory.file(&temporary, libc::O_RDONLY) {
        Ok(_) => true,
        Err(error) if missing(&error) => false,
        Err(error) => return Err(error),
    };
    let record = directory.read_optional::<Record>(&name)?;
    if !pending
        && record
            .as_ref()
            .is_none_or(|record| record.launch.agent_id != agent_id)
    {
        return Ok(());
    }
    let Some(_lock) = directory.lock(true)? else {
        return Ok(());
    };
    // Even a helper killed during atomic publication must not leave its token
    // in an unbounded temporary file after the registered root exits.
    if pending {
        match directory.file(&temporary, libc::O_RDONLY) {
            Ok(_) => directory.unlink(&temporary, false)?,
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(record) = directory.read_optional::<Record>(&name)?
        && record.launch.agent_id == agent_id
        && !crate::agent::root_alive(&record.root)?
    {
        remove_record(&directory, &path, &name, &record)?;
    }
    Ok(())
}

fn remove_record(directory: &Directory, path: &Path, name: &str, record: &Record) -> Result<()> {
    validate_id(&record.launch.agent_id)?;
    directory.remove_tree(path, &record.launch.agent_id)?;
    directory.unlink(name, false)
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid installed relay identity");
    }
    Ok(())
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

struct Directory(File);
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        // Closing alone does not release flock if a concurrent fork inherited
        // the open file description. Explicit unlock matches context leases.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Self> {
        if !path.is_absolute() {
            bail!("installed runtime path must be absolute");
        }
        let mut directory = File::open("/")?;
        for component in path.components() {
            let Component::Normal(name) = component else {
                if component == Component::RootDir {
                    continue;
                }
                bail!("unsafe installed runtime path component");
            };
            let name = CString::new(name.as_bytes())?;
            if create && unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
            }
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            directory = unsafe { File::from_raw_fd(fd) };
        }
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            bail!("installed runtime requires a private current-user-owned directory");
        }
        Ok(Self(directory))
    }

    fn file(&self, name: &str, flags: i32) -> Result<File> {
        if name.contains('/') || name == "." || name == ".." {
            bail!("unsafe installed runtime filename");
        }
        let name = CString::new(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() > 1
            || (metadata.nlink() == 0 && flags & libc::O_ACCMODE != libc::O_RDONLY)
        {
            bail!("unsafe installed runtime file type, ownership, permissions, or links");
        }
        Ok(file)
    }

    fn read<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let file = self.file(name, libc::O_RDONLY)?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 {
            bail!("installed record exceeds size limit");
        }
        serde_json::from_slice(&bytes).context("invalid private installed runtime record")
    }

    fn read_optional<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match self.read(name) {
            Ok(value) => Ok(Some(value)),
            Err(error) if missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn lock(&self, nonblocking: bool) -> Result<Option<Lock>> {
        let file = self.file("claim.lock", libc::O_RDWR | libc::O_CREAT)?;
        let flags = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
        if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
            let error = std::io::Error::last_os_error();
            if nonblocking && error.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error.into());
        }
        Ok(Some(Lock(file)))
    }

    fn replace(&self, name: &str, value: &Record) -> Result<()> {
        let _: Option<serde_json::Value> = self.read_optional(name)?;
        validate_id(&value.launch.agent_id)?;
        let temporary = format!(".claim-{}", value.launch.agent_id);
        let result = (|| -> Result<()> {
            let mut file = self.file(&temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            serde_json::to_writer(&mut file, value)?;
            file.flush()?;
            file.sync_all()?;
            let from = CString::new(temporary.as_str())?;
            let to = CString::new(name)?;
            if unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    from.as_ptr(),
                    self.0.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.0.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.unlink(&temporary, false);
        }
        result
    }

    fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        let name = CString::new(name)?;
        if unsafe {
            libc::unlinkat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        Ok(())
    }

    fn remove_tree(&self, path: &Path, name: &str) -> Result<()> {
        let child_path = path.join(name);
        let child = match Self::open(&child_path, false) {
            Ok(child) => child,
            Err(error) if missing(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in fs::read_dir(&child_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .context("non-UTF-8 installed runtime filename")?;
            if entry.file_type()?.is_dir() {
                child.remove_tree(&child_path, name)?;
            } else {
                child.file(name, libc::O_RDONLY)?;
                child.unlink(name, false)?;
            }
        }
        self.unlink(name, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    fn root() -> NativeRoot {
        NativeRoot {
            pid: i32::MAX,
            parent: i32::MAX - 1,
            identity: "fixture:birth-one".into(),
            harness: "claude".into(),
            tty: 17,
            server: i32::MAX - 2,
            server_identity: "fixture:server-birth".into(),
            session_id: "a".repeat(48),
        }
    }

    fn launch(number: usize, harness: &str) -> RelayLaunch {
        RelayLaunch {
            agent_id: format!("{number:032x}"),
            token: "b".repeat(64),
            socket: "/private/relay.sock".into(),
            harness: harness.into(),
        }
    }

    struct FixtureDirectory {
        _temporary: tempfile::TempDir,
        path: PathBuf,
    }

    impl FixtureDirectory {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn directory() -> (FixtureDirectory, Directory) {
        let temporary = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temporary.path()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = Directory::open(&path, false).unwrap();
        (
            FixtureDirectory {
                _temporary: temporary,
                path,
            },
            directory,
        )
    }

    #[test]
    fn roles_are_whitelisted_and_reuse_real_argument_validation() {
        for (harness, role, arguments) in [
            ("claude", "mcp", vec![]),
            ("claude", "claude-title", vec![]),
            ("claude", "claude-inbox", vec![]),
            (
                "claude",
                "claude-wait",
                vec![OsString::from("--active-turn")],
            ),
            ("omp", "mcp", vec![]),
            ("omp", "omp-receiver", vec!["--pid".into(), "42".into()]),
            (
                "omp",
                "bind-session",
                vec!["--session-id".into(), "root".into()],
            ),
            ("agy", "mcp", vec![]),
            ("agy", "agy-receiver", vec![]),
        ] {
            assert!(
                role_command(harness, role, &arguments).is_ok(),
                "{harness}/{role}"
            );
        }
        for (harness, role) in [
            ("codex", "mcp"),
            ("agy", "claude-title"),
            ("omp", "agy-receiver"),
            ("claude", "omp-receiver"),
            ("agy", "bind-session"),
            ("unknown", "mcp"),
        ] {
            assert!(role_command(harness, role, &[]).is_err());
        }
        assert!(role_command("claude", "mcp", &["--active-turn".into()]).is_err());
        assert!(role_command("omp", "omp-receiver", &[]).is_err());
    }

    #[test]
    fn hidden_integration_cli_preserves_receiver_argument_boundaries() {
        let cli = crate::Cli::try_parse_from([
            "bkpr",
            "integration",
            "omp",
            "omp-receiver",
            "--pid",
            "42",
            "--active-turn",
        ])
        .unwrap();
        let Some(crate::Commands::Integration(args)) = cli.command else {
            panic!("expected installed integration command");
        };
        assert_eq!(args.harness, "omp");
        assert_eq!(args.role, "omp-receiver");
        assert_eq!(
            args.arguments,
            ["--pid", "42", "--active-turn"].map(OsString::from)
        );
        let (argv, pid) = role_command(&args.harness, &args.role, &args.arguments).unwrap();
        assert_eq!(pid, Some(42));
        assert_eq!(
            argv,
            ["relay-receiver", "--pid", "42", "--active-turn"].map(OsString::from)
        );
    }

    #[test]
    fn simultaneous_hook_and_mcp_claim_exactly_one_root_identity() {
        let (temporary, _) = directory();
        let barrier = Arc::new(Barrier::new(12));
        let prepares = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let path = temporary.path().to_owned();
                let barrier = Arc::clone(&barrier);
                let prepares = Arc::clone(&prepares);
                std::thread::spawn(move || {
                    let directory = Directory::open(&path, false).unwrap();
                    barrier.wait();
                    let _lock = directory.lock(false).unwrap().unwrap();
                    claim_with(
                        &directory,
                        &root(),
                        || {
                            let count = prepares.fetch_add(1, Ordering::SeqCst) + 1;
                            Ok(launch(count, "claude"))
                        },
                        |_| Ok(()),
                    )
                    .unwrap()
                    .launch
                    .agent_id
                })
            })
            .collect();
        let ids: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(prepares.load(Ordering::SeqCst), 1);
        assert!(ids.iter().all(|id| id == &ids[0]));
    }

    #[test]
    fn inspector_binds_only_exact_root_birth_harness_terminal_and_native_server() {
        let (_temporary, directory) = directory();
        let root = root();
        let record = claim_with(&directory, &root, || Ok(launch(1, "claude")), |_| Ok(())).unwrap();
        assert_eq!(
            lookup_in(&directory, &root).unwrap(),
            Some(record.launch.agent_id)
        );
        for changed in [
            NativeRoot {
                pid: root.pid - 1,
                ..root.clone()
            },
            NativeRoot {
                parent: root.parent - 1,
                ..root.clone()
            },
            NativeRoot {
                identity: "fixture:reused-pid".into(),
                ..root.clone()
            },
            NativeRoot {
                harness: "omp".into(),
                ..root.clone()
            },
            NativeRoot {
                tty: root.tty + 1,
                ..root.clone()
            },
            NativeRoot {
                server: root.server - 1,
                ..root.clone()
            },
            NativeRoot {
                server_identity: "fixture:reused-server".into(),
                ..root.clone()
            },
            NativeRoot {
                session_id: "c".repeat(48),
                ..root.clone()
            },
        ] {
            assert!(lookup_in(&directory, &changed).unwrap().is_none());
        }
    }

    #[test]
    fn exited_or_reused_pid_claims_fresh_inbox_and_stale_credentials_fail_closed() {
        let (_temporary, directory) = directory();
        let first = root();
        let old = claim_with(&directory, &first, || Ok(launch(1, "claude")), |_| Ok(())).unwrap();
        let second = NativeRoot {
            identity: "fixture:birth-two".into(),
            ..first.clone()
        };
        let new = claim_with(&directory, &second, || Ok(launch(2, "claude")), |_| Ok(())).unwrap();
        assert_ne!(old.launch.agent_id, new.launch.agent_id);
        assert!(lookup_in(&directory, &first).unwrap().is_none());
        let calls = AtomicUsize::new(0);
        assert!(
            claim_with(
                &directory,
                &second,
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(launch(3, "claude"))
                },
                |_| bail!("stale broker credentials")
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rendezvous_rejects_symlinks_hardlinks_and_public_permissions() {
        let (temporary, directory) = directory();
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"{}").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        let name = record_name(root().pid);
        symlink(&outside, temporary.path().join(&name)).unwrap();
        assert!(directory.read_optional::<Record>(&name).is_err());
        let record = Record {
            version: 1,
            root: root(),
            launch: launch(1, "claude"),
        };
        assert!(directory.replace(&name, &record).is_err());
        fs::remove_file(temporary.path().join(&name)).unwrap();
        fs::hard_link(&outside, temporary.path().join(&name)).unwrap();
        assert!(directory.read_optional::<Record>(&name).is_err());
        fs::remove_file(temporary.path().join(&name)).unwrap();
        fs::write(temporary.path().join(&name), b"{}").unwrap();
        fs::set_permissions(
            temporary.path().join(&name),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(directory.read_optional::<Record>(&name).is_err());
        let link = temporary.path().join("redirect");
        symlink(temporary.path(), &link).unwrap();
        assert!(Directory::open(&link, false).is_err());
    }

    #[test]
    fn explicit_unlock_releases_even_an_inherited_open_description() {
        let (_temporary, directory) = directory();
        let lock = directory.lock(false).unwrap().unwrap();
        let inherited = lock.0.try_clone().unwrap();
        drop(lock);
        assert!(directory.lock(true).unwrap().is_some());
        drop(inherited);
    }

    #[test]
    fn retiring_a_dead_root_removes_private_runtime_without_touching_reused_pid() {
        let (temporary, directory) = directory();
        let record =
            claim_with(&directory, &root(), || Ok(launch(1, "claude")), |_| Ok(())).unwrap();
        let runtime = temporary
            .path()
            .join(&record.launch.agent_id)
            .join("claude-titles");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(runtime.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(runtime.join("claim"), b"private").unwrap();
        fs::set_permissions(runtime.join("claim"), fs::Permissions::from_mode(0o600)).unwrap();
        prune(&directory, temporary.path()).unwrap();
        assert!(!runtime.parent().unwrap().exists());
        assert!(
            directory
                .read_optional::<Record>(&record_name(root().pid))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn broker_retirement_removes_interrupted_private_publication() {
        let _runtime = crate::launcher::test_support::isolate_runtime();
        let path = registry_path().unwrap();
        let directory = Directory::open(&path, true).unwrap();
        let id = "a".repeat(32);
        let temporary = format!(".claim-{id}");
        directory
            .file(&temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)
            .unwrap()
            .write_all(b"{\"launch\":{\"token\":")
            .unwrap();
        retire(i32::MAX as u32, &id).unwrap();
        assert!(directory.file(&temporary, libc::O_RDONLY).is_err());
    }

    fn publication(agent_ids: &[&str]) -> crate::context::LiveSession {
        let root = root();
        crate::context::LiveSession {
            session_id: root.session_id,
            session_name: "owned fixture".into(),
            runtime_name: "owned-fixture".into(),
            server: root.server,
            workspaces: Vec::new(),
            panes: agent_ids
                .iter()
                .enumerate()
                .map(|(index, id)| bkpr::context_contract::ContextPane {
                    pane_id: format!("terminal_{index}"),
                    workspace_id: "fixture-workspace".into(),
                    pane_name: "fixture".into(),
                    agent_id: Some((*id).to_owned()),
                    status: None,
                })
                .collect(),
        }
    }

    #[test]
    fn native_association_requires_exact_agent_under_exact_owner_session() {
        let id = "a".repeat(32);
        let unrelated = "b".repeat(32);
        assert!(!association_published(&root(), &id, &publication(&[&unrelated])).unwrap());
        assert!(association_published(&root(), &id, &publication(&[&id])).unwrap());
        assert!(association_published(&root(), &id, &publication(&[&id, &id])).is_err());
        let mut foreign = publication(&[&id]);
        foreign.session_id = "foreign".into();
        assert!(association_published(&root(), &id, &foreign).is_err());
        foreign = publication(&[&id]);
        foreign.server -= 1;
        assert!(association_published(&root(), &id, &foreign).is_err());
    }

    #[test]
    fn acknowledged_push_is_not_attachment_and_missing_association_times_out() {
        use std::cell::Cell;
        let notifications = Cell::new(0);
        let result = await_association(
            Duration::ZERO,
            || Ok((publication(&[]), false)),
            |_| {
                notifications.set(notifications.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(notifications.get(), 1);
    }

    #[test]
    fn native_association_barrier_notifies_once_then_waits_for_real_publication() {
        use std::cell::Cell;
        let observations = Cell::new(0);
        let notifications = Cell::new(0);
        await_association(
            Duration::from_secs(1),
            || {
                let observation = observations.get() + 1;
                observations.set(observation);
                Ok((publication(&[]), observation >= 3))
            },
            |_| {
                notifications.set(notifications.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(observations.get(), 3);
        assert_eq!(notifications.get(), 1);
        await_association(
            Duration::ZERO,
            || Ok((publication(&[]), true)),
            |_| panic!("an already published association must not notify again"),
        )
        .unwrap();
    }

    #[test]
    fn a_new_helper_notifies_if_original_claimant_died_before_notification() {
        use std::cell::Cell;
        let (_temporary, directory) = directory();
        let original =
            claim_with(&directory, &root(), || Ok(launch(1, "claude")), |_| Ok(())).unwrap();
        let resumed = claim_with(
            &directory,
            &root(),
            || panic!("the second helper must reuse the authenticated root record"),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(original.launch.agent_id, resumed.launch.agent_id);
        let notified = Cell::new(false);
        await_association(
            Duration::from_secs(1),
            || Ok((publication(&[]), notified.get())),
            |_| {
                notified.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(notified.get());
    }

    #[test]
    fn native_association_barrier_rejects_owner_or_root_loss_without_notifying_again() {
        use std::cell::Cell;
        let notified = Cell::new(false);
        let result = await_association(
            Duration::from_secs(1),
            || {
                if notified.get() {
                    bail!("root exited, reparented, or owner lease expired");
                }
                Ok((publication(&[]), false))
            },
            |_| {
                notified.set(true);
                Ok(())
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("owner lease expired")
        );
        assert!(notified.get());
    }
}
