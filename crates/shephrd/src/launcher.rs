use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;
use shephrd::{Action, Request, Snapshot};

use crate::{backend, herdr, transport};

#[derive(Debug, Args)]
pub struct Options {
    /// Open or attach to a named session.
    #[arg(long, help_heading = "Session")]
    pub session: Option<String>,
    /// Connect using a saved Herdr machine label or ID.
    #[arg(long, conflicts_with = "remote", help_heading = "Remote sessions")]
    pub machine: Option<String>,
    /// Connect to a remote Herdr session at an SSH destination.
    #[arg(long, conflicts_with = "machine", help_heading = "Remote sessions")]
    pub remote: Option<String>,
    /// Explicit remote Herdr executable; otherwise discover the stock installation.
    #[arg(long, env = "SHEPHRD_REMOTE_HERDR", hide = true)]
    pub remote_herdr: Option<String>,
    /// Local Herdr executable used only to resolve saved remote machines.
    #[arg(long, env = "SHEPHRD_HERDR", default_value = "herdr", hide = true)]
    pub herdr: PathBuf,
    /// Herdr configuration used only for saved remote-machine lookup.
    #[arg(long, env = "SHEPHRD_HERDR_CONFIG", hide = true)]
    pub herdr_config: Option<PathBuf>,
    #[arg(long, env = "SHEPHRD_ZELLIJ", default_value = "zellij", hide = true)]
    pub zellij: PathBuf,
    #[arg(long, env = "SHEPHRD_SSH", default_value = "ssh", hide = true)]
    pub ssh: PathBuf,
    /// Frontend WASM artifact; bundled automatically by the product package.
    #[arg(long, env = "SHEPHRD_PLUGIN", hide = true)]
    pub plugin: Option<PathBuf>,
    /// Sidebar width in columns (16–120).
    #[arg(long, default_value_t = 38, value_parser = clap::value_parser!(u16).range(16..=120), help_heading = "Session")]
    pub sidebar_width: u16,
}

pub fn run(options: Options) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("launch requires an interactive terminal");
    }
    let program = env::current_exe().context("locate native shephrd command")?;
    let zellij = executable(&options.zellij)?;
    let plugin = plugin_path(options.plugin.clone(), &program)?;
    check_zellij(&zellij)?;
    if options.machine.is_none() && options.remote.is_none() {
        return run_native(&options, &program, &zellij, &plugin);
    }
    let ssh = executable(&options.ssh)?;

    let (machine, target, saved_session) = if let Some(selector) = options.machine.as_deref() {
        let herdr_bin = executable(&options.herdr)?;
        let herdr_config = options
            .herdr_config
            .map(fs::canonicalize)
            .transpose()
            .context("locate explicitly selected Herdr configuration")?;
        let machine = herdr::resolve_machine(&herdr_bin, selector, herdr_config.as_deref())?;
        if !machine.enabled {
            bail!("saved machine {} is disabled", machine.label);
        }
        (
            Some(machine.label),
            Some(machine.target),
            Some(machine.session),
        )
    } else {
        (options.remote.clone(), options.remote, None)
    };
    let session = options
        .session
        .or(saved_session)
        .unwrap_or_else(|| "shephrd".into());
    backend::validate_session(&session)?;
    let target = target.context("remote endpoint is required")?;
    let remote = if let Some(executable) = options.remote_herdr {
        backend::RemoteTarget {
            target,
            session: session.clone(),
            executable,
        }
    } else {
        herdr::discover_remote(&ssh, &target, &session)?
    };
    let identity = format!("{}\0{}", remote.target, session);
    let identity_hash = hash_identity(&identity);
    let runtime = runtime_root()?.join(format!("{identity_hash:016x}"));
    private_directory(&runtime)?;
    let config_dir = runtime.join("zellij");
    private_directory(&config_dir)?;
    let socket = runtime.join("backend.sock");
    let zellij_session = format!("shephrd-{:016x}", identity_hash);
    let plugin_url = format!("file:{}", plugin.display());
    let plugin_configuration = BTreeMap::from([
        ("socket".into(), socket.to_string_lossy().into_owned()),
        ("program".into(), program.to_string_lossy().into_owned()),
        ("plugin_url".into(), plugin_url.clone()),
        ("role".into(), "sidebar".into()),
    ]);
    if plugin_configuration
        .values()
        .any(|value| value.contains(','))
    {
        bail!(
            "Zellij plugin routing cannot represent commas in the plugin, program or runtime path"
        );
    }
    let config = backend::Config {
        socket: socket.clone(),
        session,
        machine,
        remote,
        ssh,
        zellij: zellij.clone(),
        zellij_session: zellij_session.clone(),
        plugin_url,
        plugin_configuration,
        program: program.clone(),
        cwd: env::current_dir()?,
        log_path: runtime.join("herdr.log"),
    };
    let lock = startup_lock(&runtime)?;
    let mut owned_backend = OwnedBackend(None);
    let state = match snapshot(&socket) {
        Ok(state) => state,
        Err(_) => {
            let config_file = runtime.join("backend.json");
            private_file(&config_file, &serde_json::to_vec_pretty(&config)?)?;
            let log = private_log(&runtime.join("backend.log"))?;
            let mut command = Command::new(&program);
            command
                .args(["backend", "--config"])
                .arg(&config_file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log);
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().context("start native companion")?;
            owned_backend.0 = Some(child);
            match await_backend(&socket, owned_backend.0.as_mut().unwrap()) {
                Ok(state) => state,
                Err(error) => {
                    return Err(error).context(format!(
                        "companion log: {}",
                        runtime.join("backend.log").display()
                    ));
                }
            }
        }
    };
    if state.session != config.session || state.machine != config.machine {
        bail!("running companion does not match the requested Herdr endpoint");
    }
    if !state.connected {
        bail!(
            "Herdr is unavailable: {}",
            state.error.as_deref().unwrap_or("connection lost")
        );
    }
    let state = if state.panes.is_empty() {
        let response = transport::request(
            &socket,
            &Request::new(
                2,
                Action::CreateWorkspace {
                    label: None,
                    cwd: None,
                },
            ),
        )?;
        if let Some(error) = response.error {
            bail!("create initial Herdr workspace: {error}");
        }
        response
            .state
            .context("workspace creation returned no authoritative snapshot")?
    } else {
        state
    };
    let initial = initial_pane(&state).context("Herdr session contains no attachable terminal")?;
    let config_path = config_dir.join("config.kdl");
    private_file(&config_path, ZELLIJ_CONFIG.as_bytes())?;
    let layout_path = runtime.join("layout.kdl");
    private_file(
        &layout_path,
        layout(&config, &state, initial, options.sidebar_width)?.as_bytes(),
    )?;
    drop(lock);

    let existing = session_exists(&zellij, &config_dir, &zellij_session)?;
    let mut client = zellij_command(&zellij, &config_dir);
    client.arg("--config").arg(&config_path);
    if existing {
        client.args(["attach", &zellij_session]);
    } else {
        client
            .args(["--session", &zellij_session, "--new-session-with-layout"])
            .arg(&layout_path);
    }
    let status = match client.status() {
        Ok(status) => status,
        Err(error) => {
            return Err(error).context("launch Zellij client");
        }
    };
    // A detached client is not an ended session. The companion monitors server lifetime.
    let server_alive = session_exists(&zellij, &config_dir, &zellij_session)?;
    if !status.success() && !server_alive {
        bail!("Zellij exited with {status}");
    }
    if server_alive && let Some(mut child) = owned_backend.0.take() {
        let _ = child.try_wait();
    }
    Ok(())
}

fn run_native(options: &Options, program: &Path, zellij: &Path, plugin: &Path) -> Result<()> {
    let session = options.session.as_deref().unwrap_or("shephrd");
    backend::validate_session(session)?;
    let identity_hash = hash_identity(&format!("native-local\0{session}"));
    let runtime = runtime_root()?.join(format!("native-{identity_hash:016x}"));
    private_directory(&runtime)?;
    let config_dir = runtime.join("zellij");
    private_directory(&config_dir)?;
    let lock = startup_lock(&runtime)?;
    let zellij_session = format!("shephrd-native-{identity_hash:016x}");
    let existing = session_exists(zellij, &config_dir, &zellij_session)?;
    let config_path = config_dir.join("config.kdl");
    let layout_path = runtime.join("layout.kdl");
    if !existing {
        let bin = runtime.join("bin");
        private_directory(&bin)?;
        let wrapper = bin.join("claude");
        private_file(&wrapper, crate::claude::wrapper(program, &bin)?.as_bytes())?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        let cwd = env::current_dir().context("locate native launch directory")?;
        let cwd = cwd
            .to_str()
            .context("native launch directory is not UTF-8")?;
        let program = program
            .to_str()
            .context("native program path is not UTF-8")?;
        let plugin = plugin.to_str().context("native plugin path is not UTF-8")?;
        let plugin_url = format!("file:{plugin}");
        let configuration = BTreeMap::from([
            ("role".into(), "sidebar".into()),
            ("mode".into(), "native".into()),
            ("session".into(), session.into()),
            ("program".into(), program.into()),
            ("plugin_url".into(), plugin_url.clone()),
            ("initial_cwd".into(), cwd.into()),
            ("sidebar_width".into(), options.sidebar_width.to_string()),
        ]);
        let mut config = ZELLIJ_CONFIG.to_owned();
        let mut owner_configuration = configuration.clone();
        owner_configuration.insert("role".into(), "native-owner".into());
        owner_configuration.insert("caller_cwd".into(), cwd.into());
        config.push_str(&format!(
            "\nload_plugins {{\n {} {{\n",
            serde_json::to_string(&plugin_url)?,
        ));
        for (key, value) in &owner_configuration {
            config.push_str(&format!("  {key} {}\n", serde_json::to_string(value)?));
        }
        config.push_str(" }\n}\n");
        private_file(&config_path, config.as_bytes())?;
        private_file(
            &layout_path,
            shephrd::native_layout(
                &plugin_url,
                &configuration,
                "Workspace",
                cwd,
                usize::from(options.sidebar_width),
            )?
            .as_bytes(),
        )?;
    }
    drop(lock);
    let mut client = zellij_command(zellij, &config_dir);
    client
        .arg("--config")
        .arg(&config_path)
        .env("SHEPHRD_ZELLIJ", zellij);
    if !existing {
        let bin = runtime.join("bin");
        let inherited_wrapper = env::var_os("SHEPHRD_CLAUDE_WRAPPER_DIR").map(PathBuf::from);
        let mut paths = vec![bin.clone()];
        paths.extend(
            env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .filter(|path| inherited_wrapper.as_ref() != Some(path)),
        );
        client
            .env(
                "PATH",
                env::join_paths(paths).context("construct native session PATH")?,
            )
            .env("SHEPHRD_CLAUDE_WRAPPER_DIR", bin);
    }
    if existing {
        client.args(["attach", &zellij_session]);
    } else {
        client
            .args(["--session", &zellij_session, "--new-session-with-layout"])
            .arg(&layout_path);
    }
    let status = client.status().context("launch native Zellij client")?;
    if !status.success() && !session_exists(zellij, &config_dir, &zellij_session)? {
        bail!("Zellij exited with {status}");
    }
    Ok(())
}

fn snapshot(socket: &Path) -> Result<Snapshot> {
    let response = transport::request(socket, &Request::new(0, Action::Snapshot))?;
    if let Some(error) = response.error {
        bail!("{error}");
    }
    response.state.context("companion returned no snapshot")
}

fn await_backend(socket: &Path, child: &mut Child) -> Result<Snapshot> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_error = String::from("companion has not bound its socket");
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("companion exited with {status}");
        }
        match snapshot(socket) {
            Ok(state) if state.connected => return Ok(state),
            Ok(state) => last_error = state.error.unwrap_or_else(|| "waiting for Herdr".into()),
            Err(error) => {
                if socket.exists() {
                    last_error = error.to_string();
                }
            }
        }
        if Instant::now() >= deadline {
            bail!("companion startup timed out: {last_error}");
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn initial_pane(state: &Snapshot) -> Option<&str> {
    state
        .focused_pane_id
        .as_deref()
        .filter(|id| state.panes.iter().any(|pane| pane.id == *id))
        .or_else(|| {
            state.workspaces.first().and_then(|workspace| {
                state
                    .panes
                    .iter()
                    .find(|pane| {
                        pane.workspace_id == workspace.id && pane.tab_id == workspace.active_tab_id
                    })
                    .map(|pane| pane.id.as_str())
            })
        })
        .or_else(|| state.panes.first().map(|pane| pane.id.as_str()))
}

fn layout(config: &backend::Config, state: &Snapshot, pane: &str, width: u16) -> Result<String> {
    let terminal = state
        .panes
        .iter()
        .find(|terminal| terminal.id == pane)
        .context("initial terminal is absent from snapshot")?;
    let workspace = state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == terminal.workspace_id)
        .context("initial workspace is absent from snapshot")?;
    let command = shephrd::CommandSpec {
        executable: config.program.to_string_lossy().into_owned(),
        args: vec![
            "attach".into(),
            "--socket".into(),
            config.socket.to_string_lossy().into_owned(),
            "--pane".into(),
            pane.into(),
        ],
        cwd: Some(config.cwd.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
    };
    Ok(shephrd::attachment_layout(
        &config.plugin_url,
        &config.plugin_configuration,
        pane,
        &command,
        &format!("{} · {}", workspace.name, terminal.name),
        usize::from(width),
    )?)
}

fn zellij_command(program: &Path, config_dir: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("ZELLIJ") {
            command.env_remove(key);
        }
    }
    command
        .arg("--config-dir")
        .arg(config_dir)
        .env("ZELLIJ_CONFIG_DIR", config_dir);
    command
}

fn session_exists(program: &Path, config_dir: &Path, name: &str) -> Result<bool> {
    let result = zellij_command(program, config_dir)
        .args(["list-sessions", "--no-formatting"])
        .output()?;
    let stdout = String::from_utf8(result.stdout)?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        if stdout.trim().is_empty() && (stderr.contains("No active") || stderr.trim().is_empty()) {
            return Ok(false);
        }
        bail!("inspect Zellij sessions: {stderr}{stdout}");
    }
    Ok(stdout
        .lines()
        .any(|line| line.split_whitespace().next() == Some(name) && !line.contains("EXITED")))
}

fn executable(path: &Path) -> Result<PathBuf> {
    let candidate = if path.components().count() == 1 {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(path))
            .find(|candidate| {
                candidate.is_file()
                    && fs::metadata(candidate)
                        .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
            })
            .with_context(|| format!("{} is not executable on PATH", path.display()))?
    } else {
        path.to_path_buf()
    };
    fs::canonicalize(&candidate).with_context(|| format!("locate {}", candidate.display()))
}

fn plugin_path(explicit: Option<PathBuf>, program: &Path) -> Result<PathBuf> {
    let path = explicit.unwrap_or_else(|| {
        program
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("target"))
            .join("wasm32-wasip1/release/shephrd-zellij.wasm")
    });
    fs::canonicalize(&path).with_context(|| format!("frontend not found at {}; build `cargo build -p shephrd-zellij --target wasm32-wasip1 --release` or use the bundled Nix package", path.display()))
}

fn check_zellij(program: &Path) -> Result<()> {
    let result = Command::new(program).arg("--version").output()?;
    let version = String::from_utf8(result.stdout)?;
    if !result.status.success() || version.trim() != "zellij 0.45.1" {
        bail!(
            "this frontend requires Zellij 0.45.1; detected {}",
            version.trim()
        );
    }
    Ok(())
}

pub(crate) fn runtime_root() -> Result<PathBuf> {
    let base = if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("shephrd")
    } else {
        let state = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .context("HOME or XDG_STATE_HOME is required")?;
        state.join("shephrd/run")
    };
    private_directory(&base)?;
    Ok(base)
}

pub(crate) fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!(
            "runtime directory is not a real current-user-owned directory: {}",
            path.display()
        );
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn private_file(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(data)?;
    Ok(())
}

fn private_log(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

pub(crate) fn startup_lock(runtime: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(runtime.join("startup.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(std::io::Error::last_os_error()).context("lock companion startup");
    }
    Ok(file)
}

fn stop_owned_backend(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

struct OwnedBackend(Option<Child>);

impl Drop for OwnedBackend {
    fn drop(&mut self) {
        stop_owned_backend(&mut self.0);
    }
}

fn hash_identity(identity: &str) -> u64 {
    identity.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

use std::os::fd::AsRawFd;

// The sidebar accepts keyboard focus only through the explicit control binding.
const ZELLIJ_CONFIG: &str = r#"default_mode "normal"
pane_frames false
mouse_hover_effects true
focus_follows_mouse false
mouse_click_through true
session_serialization false
show_startup_tips false
show_release_notes false
keybinds {
    locked {
        bind "Ctrl Space" { MessagePlugin { name "shephrd-controls"; payload "toggle"; }; }
    }
    normal {
        bind "Ctrl Space" { MessagePlugin { name "shephrd-controls"; payload "toggle"; }; }
        bind "Esc" { MessagePlugin { name "shephrd-controls"; payload "leave"; }; SwitchToMode "Locked"; }
        bind "Tab" { FocusNextPane; }
        bind "Shift Tab" { FocusPreviousPane; }
        bind "Ctrl d" { Detach; }
    }
}
"#;
