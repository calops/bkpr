//! Launch-owned Codex app-server and exact, pre-response native thread capture.
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bkpr_protocol::{ConversationSource, HarnessKind};

use super::{Capture, exit_code, passthrough, scrub};
#[path = "codex_startup.rs"]
mod startup;

struct LaunchPlan {
    overrides: Vec<OsString>,
    cwd: PathBuf,
    cwd_argument: Option<(usize, bool)>,
    passthrough: bool,
}

fn launch_plan(arguments: &[OsString]) -> Result<LaunchPlan> {
    let mut plan = LaunchPlan {
        overrides: Vec::new(),
        cwd: env::current_dir()?,
        cwd_argument: None,
        passthrough: false,
    };
    let mut first_positional = true;
    let mut profile = false;
    let mut resume_or_fork = false;
    let mut permission_override = false;
    let mut index = 0;
    while index < arguments.len() {
        let bytes = arguments[index].as_os_str().as_bytes();
        if let Some(path) = bytes
            .strip_prefix(b"--cd=")
            .or_else(|| bytes.strip_prefix(b"-C="))
            .or_else(|| bytes.strip_prefix(b"-C").filter(|path| !path.is_empty()))
        {
            plan.cwd = env::current_dir()?.join(std::ffi::OsStr::from_bytes(path));
            plan.cwd_argument = Some((index, false));
            index += 1;
            continue;
        }
        let Some(argument) = arguments[index].to_str() else {
            first_positional = false;
            index += 1;
            continue;
        };
        if argument == "--" {
            break;
        }
        let (option, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(key, value)| (key, Some(value)));
        match option {
            "--help"
            | "-h"
            | "--version"
            | "-V"
            | "--no-daemon"
            | "--remote"
            | "--remote-auth-token-env"
            | "--add-dir"
            | "--worktree" => {
                // Explicit remote/standalone choices never acquire our capture grant.
                plan.passthrough = true;
                return Ok(plan);
            }
            "-p" | "--profile" => {
                profile = true;
                if inline.is_none() {
                    index += 1;
                    arguments
                        .get(index)
                        .context("Codex --profile requires a name")?;
                }
            }
            "-c" | "--config" => {
                let value = if let Some(value) = inline {
                    OsString::from(value)
                } else {
                    index += 1;
                    arguments
                        .get(index)
                        .context("Codex --config requires key=value")?
                        .clone()
                };
                let (local_only, permissions) = config_boundaries(&value);
                if local_only {
                    plan.passthrough = true;
                    return Ok(plan);
                }
                permission_override |= permissions;
                plan.overrides.extend([OsString::from("-c"), value]);
            }
            "--enable" | "--disable" => {
                plan.overrides.push(arguments[index].clone());
                if inline.is_none() {
                    index += 1;
                    plan.overrides.push(
                        arguments
                            .get(index)
                            .context("Codex feature option requires a feature name")?
                            .clone(),
                    );
                }
            }
            "--strict-config" => plan.overrides.push(arguments[index].clone()),
            "-C" | "--cd" => {
                plan.cwd_argument = Some((index, inline.is_none()));
                let value = if let Some(value) = inline {
                    OsString::from(value)
                } else {
                    index += 1;
                    arguments
                        .get(index)
                        .context("Codex --cd requires a directory")?
                        .clone()
                };
                plan.cwd = env::current_dir()?.join(value);
            }
            "-s" | "--sandbox" | "-a" | "--ask-for-approval" => {
                permission_override = true;
                if inline.is_none() {
                    index += 1;
                    arguments
                        .get(index)
                        .context("Codex permission option requires a value")?;
                }
            }
            "--dangerously-bypass-approvals-and-sandbox"
            | "--yolo"
            | "--full-auto"
            | "--auto-review" => permission_override = true,
            "-m" | "--model" | "-i" | "--image" | "--local-provider" => {
                if inline.is_none() {
                    index += 1;
                    arguments
                        .get(index)
                        .context("Codex option requires a value")?;
                }
            }
            _ if option.starts_with("-c") && !option.starts_with("--") => {
                let value = OsString::from(&argument[2..]);
                let (local_only, permissions) = config_boundaries(&value);
                if local_only {
                    plan.passthrough = true;
                    return Ok(plan);
                }
                permission_override |= permissions;
                plan.overrides.extend([OsString::from("-c"), value]);
            }
            _ if option.starts_with("-p") && !option.starts_with("--") => profile = true,
            _ if (option.starts_with("-s") || option.starts_with("-a"))
                && !option.starts_with("--") =>
            {
                permission_override = true
            }
            _ if option.starts_with('-') => {}
            _ if first_positional => {
                first_positional = false;
                match argument {
                    "resume" | "fork" => resume_or_fork = true,
                    "exec" | "e" | "review" | "agents" | "login" | "logout" | "mcp" | "plugin"
                    | "app-server" | "remote-control" | "completion" | "update" | "doctor"
                    | "sandbox" | "debug" | "apply" | "a" | "queue" | "archive" | "delete"
                    | "migrate-rollouts" | "unarchive" | "cloud" | "exec-server" | "features"
                    | "help" => {
                        plan.passthrough = true;
                        return Ok(plan);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        index += 1;
    }
    if resume_or_fork && permission_override {
        plan.passthrough = true;
        return Ok(plan);
    }
    if profile {
        bail!(
            "Codex profiles cannot be applied to its launch-local app-server; use base config or explicit -c overrides, or choose explicit --remote/--no-daemon"
        );
    }
    Ok(plan)
}

// These are native remote-mode restrictions, not a new Bkpr permission policy.
fn config_boundaries(value: &std::ffi::OsStr) -> (bool, bool) {
    let Some((key, value)) = value.to_str().and_then(|value| value.split_once('=')) else {
        return (false, false);
    };
    let key = key.trim();
    let local_only = key == "sandbox_workspace_write.writable_roots"
        || (key == "sandbox_workspace_write"
            && value
                .parse::<toml::Value>()
                .ok()
                .is_some_and(|parsed| parsed.get("writable_roots").is_some()));
    let root = key.split('.').next().unwrap_or(key);
    let permissions = matches!(
        root,
        "approval_policy"
            | "approvals_reviewer"
            | "default_permissions"
            | "sandbox_mode"
            | "sandbox_workspace_write"
            | "permissions"
            | "network"
    );
    (local_only, permissions)
}

fn native_tui_arguments(
    mut arguments: Vec<OsString>,
    plan: &LaunchPlan,
    cwd: &Path,
) -> Vec<OsString> {
    // An explicit remote interprets relative CWD against the owned server's CWD.
    // Resolve once, otherwise `-C project` would become project/project.
    if let Some((index, separate)) = plan.cwd_argument {
        if separate {
            arguments[index + 1] = cwd.as_os_str().to_owned();
        } else {
            let mut argument = OsString::from("--cd=");
            argument.push(cwd);
            arguments[index] = argument;
        }
    }
    arguments
}

// Every return path reaps the app-server before the private runtime is removed.
struct OwnedServer(Child);
impl Drop for OwnedServer {
    fn drop(&mut self) {
        // The app-server has its own group; terminate its launch-owned helpers too.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return passthrough(&executable, arguments);
    }
    let plan = launch_plan(&arguments)?;
    if plan.passthrough {
        return passthrough(&executable, arguments);
    }
    let capture = Capture::begin(HarnessKind::Codex)?;
    let result = managed(&executable, arguments, &plan, &capture);
    if let Err(error) = &result {
        let _ = capture.error(&format!("Codex capture failed: {error:#}"));
    }
    if capture.end().is_err() {
        eprintln!("bkpr: failed to revoke Codex capture after native exit");
    }
    result
}

fn managed(
    executable: &Path,
    arguments: Vec<OsString>,
    plan: &LaunchPlan,
    capture: &Capture,
) -> Result<i32> {
    let cwd = fs::canonicalize(&plan.cwd).context("resolve Codex working directory")?;
    let arguments = native_tui_arguments(arguments, plan, &cwd);
    let runtime = tempfile::Builder::new()
        .prefix("bkpr-codex-")
        .tempdir_in("/tmp")?;
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700))?;
    let socket = runtime.path().join("server.sock");
    let endpoint = format!(
        "unix://{}",
        socket.to_str().context("Codex socket path is not UTF-8")?
    );
    let log_path = runtime.path().join("server.log");
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&log_path)?;
    let cancelled = Arc::new(AtomicUsize::new(0));
    let mut signals = Signals(Vec::new());
    for signal in [libc::SIGHUP, libc::SIGTERM] {
        signals.0.push(signal_hook::flag::register_usize(
            signal,
            Arc::clone(&cancelled),
            signal as usize,
        )?);
    }
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
    let mut command = Command::new(executable);
    scrub(&mut command);
    command
        .args(["app-server", "--listen", &endpoint])
        .args(&plan.overrides)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // The foreground TUI receives terminal interrupts, not the waiting supervisor.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut server = OwnedServer(
        command
            .spawn()
            .context("start launch-local Codex app-server")?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let signal = cancelled.load(Ordering::Acquire);
        if signal != 0 {
            return Ok(-(signal as i32));
        }
        if let Some(status) = server.0.try_wait()? {
            bail!(
                "Codex app-server exited ({status}): {}",
                startup_log(&log_path)
            );
        }
        if UnixStream::connect(&socket).is_ok() {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "Codex app-server did not open its socket: {}",
                startup_log(&log_path)
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    validate_endpoint(&socket)?;
    let proxy_socket = runtime.path().join("tui.sock");
    let listener = UnixListener::bind(&proxy_socket).context("open private Codex TUI transport")?;
    let tui_endpoint = format!(
        "unix://{}",
        proxy_socket
            .to_str()
            .context("Codex TUI socket path is not UTF-8")?
    );
    let mut tui = Command::new(executable);
    scrub(&mut tui);
    // The proxy alone holds capture authority; native children do not need its token.
    unsafe {
        tui.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    let tui = tui
        .args(["--remote", &tui_endpoint])
        .args(arguments)
        .spawn()
        .context("start Codex TUI")?;
    let status = startup::supervise(
        tui,
        listener,
        &socket,
        &mut server.0,
        &cancelled,
        |method, id| {
            let source = match method {
                "thread/start" => ConversationSource::CodexStart,
                "thread/resume" => ConversationSource::CodexResume,
                "thread/fork" => ConversationSource::CodexFork,
                _ => bail!("unexpected Codex selection method"),
            };
            capture
                .bind(source, id)
                .context("bind TUI-selected Codex native thread before response")
        },
    );
    let signal = cancelled.load(Ordering::Acquire);
    if signal != 0 {
        return Ok(-(signal as i32));
    }
    Ok(exit_code(status?))
}

fn startup_log(path: &Path) -> String {
    use std::io::Read;
    let mut bytes = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(16 * 1024).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn validate_endpoint(socket: &Path) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let validate_parent = |socket: &Path| -> Result<()> {
        let parent = fs::symlink_metadata(socket.parent().context("locate Codex runtime")?)?;
        if !parent.is_dir() || parent.uid() != uid || parent.mode() & 0o077 != 0 {
            bail!("Codex endpoint must be in a private user-owned directory");
        }
        Ok(())
    };
    validate_parent(socket)?;
    let mut metadata = fs::symlink_metadata(socket)?;
    if metadata.uid() != uid {
        bail!("Codex endpoint must be user-owned");
    }
    if metadata.file_type().is_symlink() {
        let target = fs::canonicalize(socket).context("resolve Codex socket")?;
        validate_parent(&target)?;
        metadata = fs::symlink_metadata(target)?;
    }
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        bail!("Codex endpoint must resolve to a user-owned Unix socket");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn selection_modes_preserve_native_picker_and_configuration() -> Result<()> {
        for mode in [
            vec![],
            vec!["resume"],
            vec!["resume", "--last"],
            vec!["fork", "native-id"],
        ] {
            assert!(!launch_plan(&args(&mode))?.passthrough);
        }
        let plan = launch_plan(&args(&[
            "-C",
            "project",
            "-c",
            "model_provider=\"fixture\"",
            "--enable",
            "native_feature",
            "resume",
        ]))?;
        assert_eq!(plan.cwd, env::current_dir()?.join("project"));
        assert_eq!(
            plan.overrides,
            args(&[
                "-c",
                "model_provider=\"fixture\"",
                "--enable",
                "native_feature"
            ])
        );
        Ok(())
    }

    #[test]
    fn explicit_opt_outs_and_noninteractive_profiles_remain_native() -> Result<()> {
        for values in [
            vec!["-p", "custom", "exec", "hello"],
            vec!["-pcustom", "--remote", "unix:///private/native.sock"],
            vec!["review"],
            vec!["--profile=custom", "--help"],
            vec!["--no-daemon"],
        ] {
            assert!(launch_plan(&args(&values))?.passthrough);
        }
        assert!(launch_plan(&args(&["-p", "custom", "resume"])).is_err());
        assert!(launch_plan(&args(&["--config"])).is_err());
        Ok(())
    }

    #[test]
    fn native_local_only_choices_never_acquire_managed_remote_mode() -> Result<()> {
        for values in [
            vec!["--add-dir", "../shared"],
            vec!["resume", "--worktree"],
            vec!["-c", "sandbox_workspace_write.writable_roots=[\"/tmp\"]"],
            vec!["-csandbox_workspace_write={writable_roots=[\"/tmp\"]}"],
            vec!["resume", "--sandbox", "workspace-write"],
            vec!["-a", "never", "fork", "native-id"],
            vec!["fork", "-c", "permissions.fixture={network_access=true}"],
            vec!["-c", "approval_policy=\"never\"", "resume"],
        ] {
            assert!(launch_plan(&args(&values))?.passthrough, "{values:?}");
        }
        assert!(!launch_plan(&args(&["--sandbox", "workspace-write"]))?.passthrough);
        assert!(!launch_plan(&args(&["resume", "-c", "model=\"fixture\""]))?.passthrough);
        assert!(
            !launch_plan(&args(&[
                "-c",
                "sandbox_workspace_write={network_access=true}"
            ]))?
            .passthrough
        );
        Ok(())
    }

    #[test]
    fn endpoint_rejects_nonprivate_parent_and_nonsocket() -> Result<()> {
        let runtime = tempfile::tempdir()?;
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700))?;
        let socket = runtime.path().join("native.sock");
        let _listener = UnixListener::bind(&socket)?;
        validate_endpoint(&socket)?;
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o755))?;
        assert!(validate_endpoint(&socket).is_err());
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700))?;
        let file = runtime.path().join("not-socket");
        fs::write(&file, b"")?;
        assert!(validate_endpoint(&file).is_err());
        Ok(())
    }

    #[test]
    fn remote_cwd_is_resolved_once_without_touching_prompt_or_other_arguments() -> Result<()> {
        for values in [
            vec!["-C", "project", "resume", "--last"],
            vec!["-Cproject", "fork", "native-id"],
            vec!["--cd=project", "prompt"],
            vec!["--cd", "project", "--", "-C-not-an-option"],
        ] {
            let original = args(&values);
            let plan = launch_plan(&original)?;
            let absolute = env::current_dir()?.join("project");
            let normalized = native_tui_arguments(original.clone(), &plan, &absolute);
            let reparsed = launch_plan(&normalized)?;
            assert_eq!(reparsed.cwd, absolute);
            assert_eq!(
                &normalized[if values[0] == "-C" || values[0] == "--cd" {
                    2
                } else {
                    1
                }..],
                &original[if values[0] == "-C" || values[0] == "--cd" {
                    2
                } else {
                    1
                }..]
            );
        }
        let non_utf8 = std::ffi::OsStr::from_bytes(b"project-\xff");
        for attached in [false, true] {
            let original = if attached {
                let mut argument = OsString::from("--cd=");
                argument.push(non_utf8);
                vec![argument]
            } else {
                vec![OsString::from("-C"), non_utf8.to_owned()]
            };
            let plan = launch_plan(&original)?;
            let absolute = env::current_dir()?.join(non_utf8);
            assert_eq!(plan.cwd, absolute);
            let normalized = native_tui_arguments(original, &plan, &absolute);
            assert_eq!(launch_plan(&normalized)?.cwd, absolute);
        }
        Ok(())
    }
}
