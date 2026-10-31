//! Launch-local capture and recursion-safe native harness execution.
pub(super) mod agent;
mod agy;
mod claude;
mod claude_inbox;
mod codex;
mod omp;

use anyhow::{Context, Result, bail};
use bkpr_protocol::{CaptureEvent, CaptureGrant, ConversationSource, HarnessKind, Operation};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

const HARNESS_HELP: &str = "Usage: bkpr run HARNESS [--] ARGS...\nHarnesses: claude, codex, omp, agy, opencode, cursor, copilot, aider, cline, goose";
const MANAGED_NAMES: &[&str] = &[
    "claude",
    "claude-code",
    "claude.exe",
    "codex",
    "omp",
    "agy",
    "antigravity",
    "opencode",
    "cursor",
    "cursor-agent",
    "copilot",
    "copilot-cli",
    "github-copilot",
    "aider",
    "cline",
    ".cline",
    "goose",
];

/// Utilities run before the TUI's UTF-8 parser, preserving native argv bytes.
pub(crate) fn dispatch(program: &OsStr, arguments: Vec<OsString>) -> Result<Option<i32>> {
    let invoked = Path::new(program).file_name().and_then(OsStr::to_str);
    if let Some(name) = invoked
        && name != "cursor"
        && let Some(kind) = HarnessKind::from_command(name)
    {
        return launch(kind, name, arguments).map(Some);
    }
    let mut arguments = arguments.into_iter();
    match arguments.next().as_deref() {
        Some(command) if command == "run" => {
            let name = arguments.next().context(HARNESS_HELP)?;
            if name == "--help" || name == "-h" {
                println!("{HARNESS_HELP}");
                return Ok(Some(0));
            }
            let name = name
                .into_string()
                .map_err(|_| anyhow::anyhow!("harness name must be UTF-8"))?;
            let kind = explicit_kind(&name)?;
            let mut arguments = arguments.peekable();
            if arguments.peek().is_some_and(|argument| argument == "--") {
                arguments.next();
            }
            // `cursor` is the public selector for Cursor Agent, not its GUI.
            let executable_name = if kind == HarnessKind::Cursor {
                "cursor-agent"
            } else {
                &name
            };
            launch(kind, executable_name, arguments.collect()).map(Some)
        }
        Some(command) if command == "capture" => capture_command(arguments.collect()).map(Some),
        Some(command) if command == "install" => {
            if arguments.next().as_deref() != Some(OsStr::new("agy")) {
                bail!("usage: bkpr install agy");
            }
            agy::install(arguments.collect()).map(Some)
        }
        _ => Ok(None),
    }
}

fn explicit_kind(name: &str) -> Result<HarnessKind> {
    if name == "cursor" {
        return Ok(HarnessKind::Cursor);
    }
    if !MANAGED_NAMES.contains(&name) {
        bail!("unknown harness; {HARNESS_HELP}");
    }
    HarnessKind::from_command(name).context("unsupported harness command")
}

fn launch(kind: HarnessKind, name: &str, arguments: Vec<OsString>) -> Result<i32> {
    // Ordinary wrapper aliases are executable choices; only explicit selectors
    // may map to a differently named native command before reaching this point.
    let executable = resolve(name)?;
    match kind {
        HarnessKind::Claude => claude::launch(executable, arguments),
        HarnessKind::Codex => codex::launch(executable, arguments),
        HarnessKind::Omp => omp::launch(executable, arguments),
        HarnessKind::Agy => agy::launch(executable, arguments),
        _ => passthrough(&executable, arguments),
    }
}

fn capture_command(arguments: Vec<OsString>) -> Result<i32> {
    let mut arguments = arguments.into_iter();
    match arguments.next().as_deref() {
        Some(command) if command == "claude-hook" => {
            if arguments.next().is_some() {
                bail!("capture claude-hook accepts no arguments");
            }
            claude::hook()?;
            Ok(0)
        }
        Some(command) if command == "bind" => {
            let (source, id) = bind_arguments(arguments.collect())?;
            Capture::from_environment()?.bind(source, &id)?;
            Ok(0)
        }
        Some(command) if command == "agent" => agent::cli(arguments.collect()),
        Some(command) if command == "agent-subscribe" => agent::subscribe(arguments.collect()),
        Some(command) if command == "claude-wait" => {
            if arguments.next().is_some() {
                bail!("capture claude-wait accepts no arguments");
            }
            claude::wait()
        }
        Some(command) if command == "agy-receiver" => agy::receiver(arguments.collect()),
        Some(command) if command == "mcp" => {
            if arguments.next().as_deref() != Some(OsStr::new("--harness")) {
                bail!("usage: bkpr capture mcp --harness HARNESS");
            }
            let kind = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .and_then(|value| HarnessKind::from_command(&value))
                .filter(|kind| kind.is_managed())
                .context("invalid MCP harness")?;
            if arguments.next().is_some() {
                bail!("unexpected MCP argument");
            }
            if kind == HarnessKind::Agy {
                agy::mcp()
            } else {
                agent::mcp(kind)
            }
        }
        _ => bail!("unknown internal capture command"),
    }
}

fn bind_arguments(arguments: Vec<OsString>) -> Result<(ConversationSource, String)> {
    let mut source = None;
    let mut id = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_bytes() {
            b"--source" if source.is_none() => {
                let value = arguments
                    .next()
                    .context("--source needs a capture source")?;
                source = Some(match value.as_bytes() {
                    b"claude_hook" => ConversationSource::ClaudeHook,
                    b"omp_extension" => ConversationSource::OmpExtension,
                    b"codex_start" => ConversationSource::CodexStart,
                    b"codex_resume" => ConversationSource::CodexResume,
                    b"codex_fork" => ConversationSource::CodexFork,
                    b"agy_mcp" => ConversationSource::AgyMcp,
                    _ => bail!("invalid capture source"),
                });
            }
            b"--session-id" if id.is_none() => {
                let value = arguments.next().context("--session-id needs a native ID")?;
                let value = value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("native ID must be UTF-8"))?;
                validate_text(&value, 1024, "native conversation ID")?;
                id = Some(value);
            }
            _ => bail!("usage: bkpr capture bind --source SOURCE --session-id ID"),
        }
    }
    Ok((
        source.context("--source is required")?,
        id.context("--session-id is required")?,
    ))
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn managed_script(path: &Path, length: u64) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buffer = [0; 8192];
    let bytes = &mut buffer[..length.min(8192) as usize];
    if file.read_exact(bytes).is_err() || !bytes.starts_with(b"#!") {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    (text.contains("--wrapper-dir")
        && ["claude-launch", "codex-launch", "omp-launch", "agy-launch"]
            .iter()
            .any(|command| text.contains(command)))
        || (text.contains("BKPR_HARNESS_WRAPPER_DIR") && text.contains(" run "))
}

fn managed_link(path: &Path) -> bool {
    if fs::read_link(path).is_err() {
        return false;
    }
    let Ok(target) = fs::canonicalize(path) else {
        return false;
    };
    matches!(
        target.file_name().and_then(OsStr::to_str),
        Some("bkpr" | ".bkpr-wrapped")
    )
}

pub(super) fn resolve(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.as_bytes().contains(&b'/') || name == "." || name == ".." {
        bail!("harness executable name must be a single PATH component");
    }
    let mut excluded = Vec::new();
    if let Some(directory) = env::var_os("BKPR_HARNESS_WRAPPER_DIR") {
        for name in MANAGED_NAMES {
            if let Ok(metadata) = fs::metadata(Path::new(&directory).join(name)) {
                excluded.push(metadata);
            }
        }
    }
    if let Ok(program) = env::current_exe().and_then(fs::metadata) {
        excluded.push(program);
    }
    resolve_on_path(
        name,
        &env::var_os("PATH").unwrap_or_default(),
        &env::current_dir()?,
        &excluded,
    )
}

fn resolve_on_path(
    name: &str,
    path: &OsStr,
    cwd: &Path,
    excluded: &[fs::Metadata],
) -> Result<PathBuf> {
    for directory in env::split_paths(path) {
        let candidate = if directory.is_absolute() {
            directory.join(name)
        } else {
            cwd.join(directory).join(name)
        };
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && !excluded.iter().any(|other| same_file(&metadata, other))
            && !managed_link(&candidate)
            && !managed_script(&candidate, metadata.len())
        {
            // Canonicalizing here breaks Nix/native executable symlink dispatch.
            return Ok(candidate);
        }
    }
    bail!("{name} is not installed on PATH outside Bkpr-managed wrappers")
}

fn private_environment(name: &OsStr) -> bool {
    let name = name.as_bytes();
    matches!(
        name,
        b"ANTIGRAVITY_LS_ADDRESS" | b"ANTIGRAVITY_CSRF_TOKEN" | b"ANTIGRAVITY_CONVERSATION_ID"
    ) || [
        b"BKPR_CAPTURE_".as_slice(),
        b"BKPR_AGENT_",
        b"BKPR_RELAY_",
        b"BKPR_CODEX_",
        b"BKPR_OMP_",
        b"BKPR_CLAUDE_",
        b"BKPR_AGY_",
        b"BKPR_NATIVE_",
        b"BKPR_INSTALLED_",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

pub(super) fn scrub(command: &mut Command) {
    // Also remove credentials explicitly installed on this Command before scrub.
    let keys: Vec<OsString> = env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| private_environment(name))
        .collect();
    for key in keys {
        command.env_remove(key);
    }
}

pub(super) fn passthrough(executable: &Path, arguments: Vec<OsString>) -> Result<i32> {
    let mut command = Command::new(executable);
    scrub(&mut command);
    Err(command.args(arguments).exec()).context("execute native harness")
}

fn validate_text(value: &str, maximum: usize, what: &str) -> Result<()> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        bail!("invalid {what}");
    }
    Ok(())
}

pub(super) struct Capture {
    pub grant: CaptureGrant,
    pub program: PathBuf,
    pub socket: PathBuf,
    revoke_on_drop: bool,
    ended: AtomicBool,
}

impl Capture {
    pub(super) fn begin(kind: HarnessKind) -> Result<Self> {
        let socket = socket_from_environment()?;
        let program = env::current_exe().context("locate Bkpr capture executable")?;
        let value = bkpr_client::transport::checked(&socket, Operation::HarnessBegin { kind })
            .context("begin authenticated harness capture")?;
        let grant: CaptureGrant =
            serde_json::from_value(value).context("invalid harness capture grant")?;
        if grant.kind != kind || !grant.kind.is_managed() {
            bail!("server returned a mismatched harness capture grant");
        }
        validate_text(&grant.launch_id, 512, "capture launch ID")?;
        validate_text(&grant.token, 512, "capture capability")?;
        validate_text(&grant.pane_id, 512, "capture pane ID")?;
        Ok(Self {
            grant,
            program,
            socket,
            revoke_on_drop: true,
            ended: AtomicBool::new(false),
        })
    }

    pub(super) fn from_environment() -> Result<Self> {
        // VarError::NotUnicode embeds the value; never attach it to a credential error.
        let value = |name| env::var(name).map_err(|_| anyhow::anyhow!("missing or invalid {name}"));
        let launch_id = value("BKPR_CAPTURE_LAUNCH_ID")?;
        let token = value("BKPR_CAPTURE_TOKEN")?;
        let harness = value("BKPR_CAPTURE_HARNESS")?;
        let kind = HarnessKind::from_command(&harness).context("invalid capture harness")?;
        if !kind.is_managed() || harness != kind.as_str() {
            bail!("invalid capture harness");
        }
        validate_text(&launch_id, 512, "capture launch ID")?;
        validate_text(&token, 512, "capture capability")?;
        let pane_id = value("BKPR_PANE_ID")?;
        validate_text(&pane_id, 512, "capture pane ID")?;
        let program =
            PathBuf::from(env::var_os("BKPR_CAPTURE_PROGRAM").context("missing capture program")?);
        if !program.is_absolute() {
            bail!("capture program must be absolute");
        }
        Ok(Self {
            grant: CaptureGrant {
                launch_id,
                token,
                pane_id,
                kind,
            },
            program,
            socket: socket_from_environment()?,
            revoke_on_drop: false,
            ended: AtomicBool::new(false),
        })
    }

    pub(super) fn apply(&self, command: &mut Command) {
        scrub(command);
        command
            .env("BKPR_CAPTURE_LAUNCH_ID", &self.grant.launch_id)
            .env("BKPR_CAPTURE_TOKEN", &self.grant.token)
            .env("BKPR_CAPTURE_HARNESS", self.grant.kind.as_str())
            .env("BKPR_CAPTURE_PROGRAM", &self.program)
            .env("BKPR_SOCKET", &self.socket)
            .env("BKPR_PANE_ID", &self.grant.pane_id);
    }

    pub(super) fn request(&self, operation: Operation) -> Result<serde_json::Value> {
        bkpr_client::transport::checked(&self.socket, operation).map_err(|error| {
            anyhow::anyhow!(
                "{}",
                format!("{error:#}").replace(&self.grant.token, "[REDACTED]")
            )
        })
    }

    pub(super) fn bind(&self, source: ConversationSource, id: &str) -> Result<()> {
        if source.harness() != self.grant.kind {
            bail!("capture source does not match harness");
        }
        validate_text(id, 1024, "native conversation ID")?;
        self.request(Operation::HarnessEvent {
            launch_id: self.grant.launch_id.clone(),
            token: self.grant.token.clone(),
            event: CaptureEvent::Bind {
                conversation_id: id.to_owned(),
                source,
            },
        })?;
        Ok(())
    }

    pub(super) fn error(&self, message: &str) -> Result<()> {
        validate_text(message, 2048, "capture diagnostic")?;
        let message = message.replace(&self.grant.token, "[REDACTED]");
        self.request(Operation::HarnessEvent {
            launch_id: self.grant.launch_id.clone(),
            token: self.grant.token.clone(),
            event: CaptureEvent::Error { message },
        })?;
        Ok(())
    }

    pub(super) fn context(&self) -> Result<serde_json::Value> {
        self.request(Operation::HarnessContext {
            launch_id: self.grant.launch_id.clone(),
            token: self.grant.token.clone(),
        })
    }

    pub(super) fn end(&self) -> Result<()> {
        if self.ended.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.request(Operation::HarnessEvent {
            launch_id: self.grant.launch_id.clone(),
            token: self.grant.token.clone(),
            event: CaptureEvent::End,
        })?;
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.revoke_on_drop {
            let _ = self.end();
        }
    }
}

fn socket_from_environment() -> Result<PathBuf> {
    let socket =
        PathBuf::from(env::var_os("BKPR_SOCKET").context("managed capture requires a Bkpr pane")?);
    if !socket.is_absolute() {
        bail!("Bkpr capture socket must be absolute");
    }
    Ok(socket)
}

struct SignalGuards(Vec<(signal_hook::SigId, i32, libc::sigaction)>);
impl SignalGuards {
    fn register(&mut self, signal: i32, action: impl Fn() + Send + Sync + 'static) -> Result<()> {
        let mut previous = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(signal, std::ptr::null(), &mut previous) } != 0 {
            return Err(std::io::Error::last_os_error()).context("read native signal disposition");
        }
        let id = unsafe { signal_hook::low_level::register(signal, action) }?;
        self.0.push((id, signal, previous));
        Ok(())
    }
}
impl Drop for SignalGuards {
    fn drop(&mut self) {
        for (id, signal, previous) in self.0.drain(..).rev() {
            signal_hook::low_level::unregister(id);
            unsafe {
                libc::sigaction(signal, &previous, std::ptr::null_mut());
            }
        }
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // No temp package can be removed while a child may still consume it.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) fn run_managed(command: &mut Command, capture: &Capture) -> Result<i32> {
    let pid = Arc::new(AtomicI32::new(0));
    let pending = Arc::new(AtomicI32::new(0));
    let mut handlers = SignalGuards(Vec::new());
    for signal in [libc::SIGINT, libc::SIGQUIT] {
        // The native child receives foreground-group interrupts. The wrapper
        // must survive a cancelled model turn to retain its package and grant.
        handlers.register(signal, || {})?;
    }
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        let pid = Arc::clone(&pid);
        let pending = Arc::clone(&pending);
        handlers.register(signal, move || {
            pending.store(signal, Ordering::SeqCst);
            let pid = pid.load(Ordering::SeqCst);
            if pid > 0 {
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        })?;
    }
    unsafe {
        command.pre_exec(|| {
            for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
                if libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => OwnedChild(child),
        Err(error) => {
            let _ = capture.end();
            return Err(error).context("execute managed native harness");
        }
    };
    pid.store(child.0.id() as i32, Ordering::SeqCst);
    let interrupted = pending.load(Ordering::SeqCst);
    if interrupted != 0 {
        unsafe {
            libc::kill(child.0.id() as i32, interrupted);
        }
    }
    // Keep the exited child unreaped until forwarding is disabled. Otherwise a
    // late signal could target a reused PID between wait() and pid.store(0).
    loop {
        let mut information = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.0.id(),
                &mut information,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            pid.store(0, Ordering::SeqCst);
            return Err(error).context("observe managed native harness exit");
        }
    }
    pid.store(0, Ordering::SeqCst);
    let status = child.0.wait().context("reap managed native harness")?;
    // Capture cleanup must not change the native program's actual exit status.
    if let Err(error) = capture.end() {
        eprintln!("bkpr: capture cleanup: {error}");
    }
    Ok(exit_code(status))
}

/// Negative values distinguish a signal from a native `exit(128 + signal)`.
pub(super) fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(libc::SIGTERM))
}

pub(crate) fn terminate(code: i32) -> ! {
    if code < 0 {
        let signal = -code;
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut());
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    // Run the real dispatch/exec path in a dedicated test subprocess, never in
    // the shared test runner and without invoking any installed harness/provider.
    #[test]
    #[ignore = "subprocess dispatch fixture"]
    fn native_dispatch_fixture() {
        let invocation =
            env::var("BKPR_TEST_NATIVE_DISPATCH").expect("dispatch fixture invocation");
        let invocation: Vec<String> = serde_json::from_str(&invocation).unwrap();
        let mut invocation = invocation.into_iter();
        let program = invocation.next().unwrap();
        let arguments = invocation.map(OsString::from).collect();
        let code = dispatch(OsStr::new(&program), arguments)
            .unwrap()
            .expect("fixture utility dispatch");
        terminate(code);
    }

    fn execute_fixture(root: &Path, invocation: &[&str]) -> std::process::Output {
        Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "--ignored",
                "harness::tests::native_dispatch_fixture",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", root)
            .env("HOME", root)
            .env(
                "BKPR_TEST_NATIVE_DISPATCH",
                serde_json::to_string(invocation).unwrap(),
            )
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    #[test]
    fn explicit_cursor_selector_executes_agent_and_never_gui() {
        let root = tempfile::tempdir().unwrap();
        executable(&root.path().join("cursor"), "#!/bin/sh\nexit 81\n");
        executable(&root.path().join("cursor-agent"), "#!/bin/sh\nexit 82\n");
        for selector in ["cursor", "cursor-agent"] {
            let output = execute_fixture(root.path(), &["bkpr", "run", selector, "--version"]);
            assert_eq!(output.status.code(), Some(82), "{output:?}");
        }
        fs::remove_file(root.path().join("cursor-agent")).unwrap();
        let output = execute_fixture(root.path(), &["bkpr", "run", "cursor", "--version"]);
        assert!(!output.status.success());
        assert_ne!(output.status.code(), Some(81));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cursor-agent is not installed"));
    }

    #[test]
    fn aliases_execute_the_selected_native_program_instead_of_another_installed_alias() {
        let root = tempfile::tempdir().unwrap();
        for (name, code) in [
            ("claude-code", 41),
            ("agy", 42),
            ("antigravity", 43),
            ("copilot", 44),
            ("copilot-cli", 45),
        ] {
            executable(
                &root.path().join(name),
                &format!("#!/bin/sh\nexit {code}\n"),
            );
        }
        for (name, code) in [
            ("claude-code", 41),
            ("antigravity", 43),
            ("copilot-cli", 45),
        ] {
            for invocation in [
                vec![name, "--version"],
                vec!["bkpr", "run", name, "--version"],
            ] {
                let output = execute_fixture(root.path(), &invocation);
                assert_eq!(
                    output.status.code(),
                    Some(code),
                    "{invocation:?}: {output:?}"
                );
            }
        }
    }

    #[test]
    fn lookup_avoids_old_scripts_and_all_same_inode_wrapper_spellings() {
        let root = tempfile::tempdir().unwrap();
        for name in ["old", "alias", "hard", "real"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        executable(
            &root.path().join("old/codex"),
            "#!/bin/sh\nexec bkpr codex-launch --wrapper-dir /old -- \"$@\"\n",
        );
        executable(&root.path().join("managed"), "#!/bin/sh\nexit 0\n");
        symlink("../managed", root.path().join("alias/codex")).unwrap();
        fs::hard_link(root.path().join("managed"), root.path().join("hard/codex")).unwrap();
        executable(&root.path().join("real/codex"), "#!/bin/sh\nexit 0\n");
        let path = env::join_paths(["old", "alias", "hard", "real"]).unwrap();
        let excluded = [fs::metadata(root.path().join("managed")).unwrap()];
        assert_eq!(
            resolve_on_path("codex", &path, root.path(), &excluded).unwrap(),
            root.path().join("real/codex")
        );
    }

    #[test]
    fn lookup_retains_symlink_spelling_and_non_utf8_path_components() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root
            .path()
            .join(OsString::from_vec(b"native-\xff".to_vec()));
        fs::create_dir(&directory).unwrap();
        executable(&directory.join(".omp-wrapped"), "#!/bin/sh\nexit 0\n");
        symlink(".omp-wrapped", directory.join("omp")).unwrap();
        let path = env::join_paths([directory.as_path()]).unwrap();
        assert_eq!(
            resolve_on_path("omp", &path, root.path(), &[]).unwrap(),
            directory.join("omp")
        );
    }

    #[test]
    fn empty_path_component_is_cwd_and_non_executable_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("claude"), "not executable").unwrap();
        assert!(resolve_on_path("claude", OsStr::new(""), root.path(), &[]).is_err());
        executable(&root.path().join("claude"), "#!/bin/sh\nexit 0\n");
        assert_eq!(
            resolve_on_path("claude", OsStr::new(""), root.path(), &[]).unwrap(),
            root.path().join("claude")
        );
    }

    #[test]
    fn binding_arguments_reject_duplicates_controls_unknown_sources_and_extra_positionals() {
        let parse = |args: &[&str]| bind_arguments(args.iter().map(OsString::from).collect());
        assert!(
            parse(&[
                "--source",
                "omp_extension",
                "--session-id",
                "native/exact-id"
            ])
            .is_ok()
        );
        for args in [
            vec![
                "--source",
                "omp_extension",
                "--session-id",
                "one",
                "--session-id",
                "two",
            ],
            vec!["--source", "unknown", "--session-id", "one"],
            vec!["--source", "claude_hook", "--session-id", "one\nforged"],
            vec!["--source", "codex_start", "--session-id", "one", "extra"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    fn fixture_capture(socket: PathBuf) -> Capture {
        Capture {
            grant: CaptureGrant {
                launch_id: "launch-owned".into(),
                token: "private-capability-do-not-print".into(),
                pane_id: "pane-authenticated".into(),
                kind: HarnessKind::Omp,
            },
            program: PathBuf::from("/private/bkpr"),
            socket,
            revoke_on_drop: false,
            ended: AtomicBool::new(false),
        }
    }

    #[test]
    fn a_callback_cannot_use_another_harness_capture_source() {
        let capture = fixture_capture(PathBuf::from("/absent/control.sock"));
        let error = capture
            .bind(ConversationSource::ClaudeHook, "exact-native-id")
            .unwrap_err();
        assert_eq!(error.to_string(), "capture source does not match harness");
    }

    #[test]
    fn rejected_capture_requests_redact_capabilities_and_end_is_not_replayed() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixListener,
            thread,
        };
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("capture.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let capture = fixture_capture(socket);
        let token = capture.grant.token.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).unwrap();
            let request: bkpr_protocol::Request = serde_json::from_str(&line).unwrap();
            assert!(matches!(
                request.operation,
                Operation::HarnessEvent {
                    event: CaptureEvent::End,
                    ..
                }
            ));
            serde_json::to_writer(
                &mut stream,
                &bkpr_protocol::Response {
                    version: bkpr_protocol::PROTOCOL_VERSION,
                    ok: false,
                    result: None,
                    error: Some(format!("denied capability {token}")),
                },
            )
            .unwrap();
            stream.write_all(b"\n").unwrap();
        });
        let error = capture.end().unwrap_err();
        assert!(!format!("{error:#}").contains(&capture.grant.token));
        assert!(error.to_string().contains("[REDACTED]"));
        server.join().unwrap();
        // The first mutation may have executed despite a rejected/unknown reply.
        // The second call is local and cannot reconnect or replay the mutation.
        capture.end().unwrap();
    }

    #[test]
    fn stale_managed_symlink_is_skipped_without_canonicalizing_real_native_link() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["stale", "native"] {
            fs::create_dir(root.path().join(directory)).unwrap();
        }
        executable(&root.path().join("bkpr"), "#!/bin/sh\nexit 0\n");
        executable(
            &root.path().join("native/.claude-wrapped"),
            "#!/bin/sh\nexit 0\n",
        );
        symlink("../bkpr", root.path().join("stale/claude")).unwrap();
        symlink(".claude-wrapped", root.path().join("native/claude")).unwrap();
        let path = env::join_paths(["stale", "native"]).unwrap();
        assert_eq!(
            resolve_on_path("claude", &path, root.path(), &[]).unwrap(),
            root.path().join("native/claude")
        );
    }

    #[test]
    fn native_nonzero_exit_is_not_confused_with_a_signal() {
        assert_eq!(exit_code(ExitStatus::from_raw(143 << 8)), 143);
        assert_eq!(
            exit_code(ExitStatus::from_raw(libc::SIGTERM)),
            -libc::SIGTERM
        );
    }

    #[test]
    #[ignore = "subprocess inherited-capability boundary fixture"]
    fn recognition_environment_fixture() {
        for name in [
            "BKPR_AGENT_RELAY_TOKEN",
            "BKPR_AGENT_RELAY_SOCKET",
            "BKPR_CAPTURE_TOKEN",
            "BKPR_CAPTURE_LAUNCH_ID",
        ] {
            assert!(
                env::var_os(name).is_none(),
                "inherited parent capability: {name}"
            );
        }
        assert_eq!(env::var("BKPR_SOCKET").unwrap(), "/private/backend.sock");
        assert_eq!(env::var("BKPR_PANE_ID").unwrap(), "recognized-pane");
    }

    #[test]
    fn recognition_children_do_not_inherit_capture_or_agent_relay_authority() {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "--ignored",
                "harness::tests::recognition_environment_fixture",
                "--nocapture",
            ])
            .env("BKPR_AGENT_RELAY_TOKEN", "parent-private-relay-token")
            .env("BKPR_AGENT_RELAY_SOCKET", "/private/parent-agent.sock")
            .env("BKPR_CAPTURE_TOKEN", "parent-private-capture-token")
            .env("BKPR_CAPTURE_LAUNCH_ID", "parent-launch")
            .env("BKPR_SOCKET", "/private/backend.sock")
            .env("BKPR_PANE_ID", "recognized-pane");
        scrub(&mut command);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
