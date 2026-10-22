use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use bkpr::recovery_contract::{RecoveryRecord, RestoreLeaf, recovery_layout};
use clap::Args;

use crate::recovery::{self, PaneLaunch};
use crate::relay::RecoveryAgents;

#[derive(Debug, Args)]
pub struct Options {
    /// Open or attach to a named session.
    #[arg(long, help_heading = "Session")]
    pub session: Option<String>,
    /// Detach other clients after attaching; keep their panes and programs running.
    #[arg(short = 'd', long, help_heading = "Session")]
    pub detach_others: bool,
    #[arg(long, env = "BKPR_ZELLIJ", default_value = "zellij", hide = true)]
    pub zellij: PathBuf,
    /// Frontend WASM artifact; bundled automatically by the product package.
    #[arg(long, env = "BKPR_PLUGIN", hide = true)]
    pub plugin: Option<PathBuf>,
    /// Initial shared sidebar outer width in columns (16–120); resizable across tabs.
    #[arg(long, default_value_t = 38, value_parser = clap::value_parser!(u16).range(16..=120), help_heading = "Session")]
    pub sidebar_width: u16,
    /// Start a fresh session over this session's recovery record, discarding it.
    #[arg(long, help_heading = "Session")]
    pub overwrite: bool,
}

/// How a launch starts its native session.
#[derive(Debug)]
pub(crate) enum StartMode {
    /// Attach to the live session; its recovery record is untouched.
    Attach,
    /// Start an empty session and claim a cleared record.
    Fresh,
    /// Recreate the recorded topology and claim the record, keeping its workspaces.
    Restore(RecoveryRecord),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// `bkpr [--session X]`: attach, else start (asking before replacing a record).
    Open,
    /// `bkpr resume --session X`: restore the record of a session that is not running.
    Resume,
}

/// Pure start decision. `ask` (true = recover) is only called when `interactive`.
pub(crate) fn decide(
    session: &str,
    intent: Intent,
    live: bool,
    record: Option<RecoveryRecord>,
    overwrite: bool,
    interactive: bool,
    ask: &mut dyn FnMut() -> Result<bool>,
) -> Result<StartMode> {
    let record = record.filter(RecoveryRecord::usable);
    let name = shell_word(session);
    match intent {
        Intent::Open if live => Ok(StartMode::Attach),
        Intent::Resume if live => {
            bail!("session {name} is running; attach with `bkpr --session {name}`")
        }
        Intent::Resume => match record {
            Some(record) => Ok(StartMode::Restore(record)),
            None => bail!("no recovery record for session {name}"),
        },
        Intent::Open => match record {
            None => Ok(StartMode::Fresh),
            Some(_) if overwrite => Ok(StartMode::Fresh),
            Some(_) if !interactive => bail!(
                "session {name} has a recovery record; restore it with `bkpr resume --session {name}` or discard it with `bkpr --session {name} --overwrite`"
            ),
            Some(record) => Ok(if ask()? {
                StartMode::Restore(record)
            } else {
                StartMode::Fresh
            }),
        },
    }
}

/// The record of a session that is not live, plus a warning when recovery is off. Only an
/// unresolvable state root degrades to "no record": an unreadable record is never replaced
/// without an explicit `--overwrite`.
fn loaded_record(
    session: &str,
    intent: Intent,
    overwrite: bool,
    loaded: Result<Option<RecoveryRecord>>,
) -> Result<(Option<RecoveryRecord>, Option<String>)> {
    match loaded {
        Ok(record) => Ok((record, None)),
        Err(error) if error.downcast_ref::<recovery::StateUnavailable>().is_some() => {
            Ok((None, Some(not_resumable(&error))))
        }
        Err(_) if intent == Intent::Open && overwrite => Ok((None, None)),
        Err(error) => {
            let name = shell_word(session);
            Err(error).context(format!(
                "read the recovery record of session {name}; `bkpr --session {name} --overwrite` discards it"
            ))
        }
    }
}

/// Claim the record for a starting session. A restore fails closed; a fresh start only warns,
/// leaving its owner unclaimed so recovery stays silently off. Attach never claims.
fn claim_record(
    mode: &StartMode,
    claim: &mut dyn FnMut(Option<&RecoveryRecord>) -> Result<()>,
) -> Result<Option<String>> {
    match mode {
        StartMode::Attach => Ok(None),
        StartMode::Fresh => Ok(claim(None).err().map(|error| not_resumable(&error))),
        StartMode::Restore(record) => {
            claim(Some(record)).context("claim the session recovery record")?;
            Ok(None)
        }
    }
}

fn not_resumable(error: &anyhow::Error) -> String {
    format!("this session will not be resumable: {error:#}")
}

/// Quote a session name for a copyable command only when it needs it.
fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// `[r]ecover` is true, `[o]verwrite` is false; anything else asks again.
fn parse_answer(line: &str) -> Option<bool> {
    match line.trim().to_ascii_lowercase().as_str() {
        "r" | "recover" => Some(true),
        "o" | "overwrite" => Some(false),
        _ => None,
    }
}

fn ask_recover(session: &str) -> Result<bool> {
    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    loop {
        write!(
            stderr,
            "bkpr: session {} has a recovery record. [r]ecover / [o]verwrite? ",
            shell_word(session)
        )?;
        stderr.flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            bail!("no answer; the recovery record was left unchanged");
        }
        if let Some(recover) = parse_answer(&line) {
            return Ok(recover);
        }
    }
}

/// A restore startup layout plus the owner's restore seed.
#[derive(Debug)]
struct RestoreLayout {
    layout: String,
    /// Sticky (user-assigned) names by leaf key, for the owner's `restore_names`.
    names: BTreeMap<String, String>,
    /// Associations kept for a retry by leaf key, for the owner's `restore_links`.
    links: BTreeMap<String, bkpr::recovery_contract::RestoreLink>,
    /// Terminal panes the layout spawns, for the owner's `restore_expected_panes`. The owner
    /// counts the same unit: one removal key per recorded terminal leaf, stacks included.
    expected_panes: usize,
}

/// Build the restore layout. Each leaf runs `program restore-pane …`; when relay state is
/// unreadable (`agents` is `Err(reason)`), harness panes restore as shells noting why.
fn restore_layout(
    plugin_url: &str,
    sidebar_configuration: &BTreeMap<String, String>,
    sidebar_percent: u8,
    session_id: &str,
    record: &RecoveryRecord,
    program: &str,
    agents: std::result::Result<&RecoveryAgents, &str>,
) -> Result<RestoreLayout> {
    let mut names = BTreeMap::new();
    let mut links = BTreeMap::new();
    // Native sessions already assigned to an earlier leaf of this layout.
    let mut resumed = std::collections::BTreeSet::new();
    let layout = recovery_layout(
        plugin_url,
        sidebar_configuration,
        sidebar_percent,
        session_id,
        record,
        &mut |key, pane| {
            let mut launch = match agents {
                Ok(agents) => recovery::resolve(pane, agents),
                Err(reason) => recovery::unresolved(pane, reason),
            };
            // A retained link and its resume elsewhere can name one session twice; only the
            // first leaf resumes it. The duplicate is not transient, so it keeps no link.
            if let PaneLaunch::Harness {
                session_id, cwd, ..
            } = &launch
                && !resumed.insert(session_id.clone())
            {
                launch = PaneLaunch::Shell {
                    cwd: cwd.clone(),
                    note: Some("session is still open elsewhere".to_owned()),
                    retry: false,
                };
            }
            if pane.renamed {
                names.insert(key.to_owned(), pane.name.clone());
            }
            if let Some(link) = recovery::restore_link(pane, &launch) {
                links.insert(key.to_owned(), link);
            }
            let (PaneLaunch::Shell { cwd, .. } | PaneLaunch::Harness { cwd, .. }) = &launch;
            let mut args = vec!["restore-pane".to_owned()];
            args.extend(recovery::restore_pane_args(key, pane, &launch));
            RestoreLeaf {
                key: key.to_owned(),
                cwd: cwd.display().to_string(),
                program: program.to_owned(),
                args,
            }
        },
    )?;
    Ok(RestoreLayout {
        layout,
        names,
        links,
        expected_panes: record.pane_count(),
    })
}

pub fn run(options: Options) -> Result<()> {
    launch(options, Intent::Open)
}

pub fn run_resume(options: Options) -> Result<()> {
    if options.session.is_none() {
        bail!("bkpr resume requires --session <name>");
    }
    if options.overwrite {
        bail!(
            "bkpr resume restores a recovery record; --overwrite only applies to `bkpr --session <name>`"
        );
    }
    if options.detach_others {
        bail!("bkpr resume starts a new session; --detach-others only applies when attaching");
    }
    launch(options, Intent::Resume)
}

fn launch(options: Options, intent: Intent) -> Result<()> {
    let program = env::current_exe().context("locate native bkpr command")?;
    let bundled_zellij = program.with_file_name("zellij");
    let zellij = if options.zellij == Path::new("zellij")
        && env::var_os("BKPR_ZELLIJ").is_none()
        && bundled_zellij.is_file()
    {
        executable(&bundled_zellij)?
    } else {
        executable(&options.zellij)?
    };
    check_zellij(&zellij)?;
    run_native(&options, intent, &program, &zellij)
}

fn run_native(options: &Options, intent: Intent, program: &Path, zellij: &Path) -> Result<()> {
    // Capture the attaching terminal's source before setting the child-only
    // stable relay address. Unset/empty is intentional, never a server fallback.
    let agent_source = env::var_os("SSH_AUTH_SOCK");
    let session = options.session.as_deref().unwrap_or("bkpr");
    validate_session(session)?;
    let identity_hash = hash_identity(&format!("native-local\0{session}"));
    let runtime = runtime_root()?.join(format!("native-{identity_hash:016x}"));
    private_directory(&runtime)?;
    let config_dir = runtime.join("zellij");
    private_directory(&config_dir)?;
    let socket_dir = zellij_socket_dir()?;
    let lock = startup_lock(&runtime)?;
    let zellij_session = format!("bkpr-native-{identity_hash:016x}");
    let live = session_exists(zellij, &config_dir, &socket_dir, &zellij_session)?;
    // Decide before requiring a terminal, so a non-interactive launch over a record gets
    // the specific refusal. A live session never reads its record (legacy attach path).
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let record = if live {
        None
    } else {
        let (record, warning) =
            loaded_record(session, intent, options.overwrite, recovery::load(session))?;
        if let Some(warning) = warning {
            eprintln!("bkpr: {warning}");
        }
        record
    };
    let mode = decide(
        session,
        intent,
        live,
        record,
        options.overwrite,
        interactive,
        &mut || ask_recover(session),
    )?;
    if !interactive {
        bail!("launch requires an interactive terminal");
    }
    let existing = matches!(mode, StartMode::Attach);
    let session_context = crate::context::SessionBootstrap::prepare(&runtime, session, existing)
        .context("initialize Bkpr session context")?;
    let config_path = config_dir.join("config.kdl");
    let layout_path = runtime.join("layout.kdl");
    if !existing {
        let session_context = session_context
            .as_ref()
            .context("fresh native session has no context bootstrap")?;
        let plugin = plugin_path(options.plugin.clone(), program)?;
        let sidebar_percent =
            bkpr::native_sidebar_percent(usize::from(options.sidebar_width), terminal_columns()?)
                .context("terminal needs at least two columns for a sidebar and terminal pane")?;
        let bin = runtime.join("bin");
        private_directory(&bin)?;
        // Retired launchers can survive a previous session in this owned directory.
        for name in ["agy", "antigravity"] {
            match fs::remove_file(bin.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove retired session launcher"),
            }
        }
        let wrapper = bin.join("claude");
        private_file(&wrapper, crate::claude::wrapper(program, &bin)?.as_bytes())?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        for (name, wrapper) in [
            ("omp", crate::omp::wrapper(program, &bin)?),
            ("codex", crate::codex::wrapper(program, &bin)?),
        ] {
            let path = bin.join(name);
            private_file(&path, wrapper.as_bytes())?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
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
            ("session".into(), session.into()),
            ("session_id".into(), session_context.session_id.clone()),
            ("program".into(), program.into()),
            ("plugin_url".into(), plugin_url.clone()),
            ("initial_cwd".into(), cwd.into()),
            ("sidebar_width".into(), options.sidebar_width.to_string()),
        ]);
        let mut owner_configuration = configuration.clone();
        owner_configuration.insert("role".into(), "native-owner".into());
        owner_configuration.insert("context_owner".into(), session_context.owner_token.clone());
        owner_configuration.insert("caller_cwd".into(), cwd.into());
        owner_configuration.insert("mode_info_format".into(), "json".into());
        let layout = match &mode {
            StartMode::Restore(record) => {
                // Unreadable relay state degrades harness panes to shells; never fail resume.
                let agents = crate::relay::recovery_agents().map_err(|error| format!("{error:#}"));
                let restore = restore_layout(
                    &plugin_url,
                    &configuration,
                    sidebar_percent,
                    &session_context.session_id,
                    record,
                    program,
                    agents.as_ref().map_err(String::as_str),
                )?;
                // Creation-only owner keys; the owner strips them before validation.
                owner_configuration.insert(
                    "restore_names".into(),
                    serde_json::to_string(&restore.names)?,
                );
                owner_configuration.insert(
                    "restore_expected_panes".into(),
                    restore.expected_panes.to_string(),
                );
                owner_configuration.insert(
                    "restore_links".into(),
                    serde_json::to_string(&restore.links)?,
                );
                restore.layout
            }
            StartMode::Fresh | StartMode::Attach => bkpr::native_layout(
                &plugin_url,
                &configuration,
                "Workspace",
                cwd,
                sidebar_percent,
                None,
            )?,
        };
        let mut config = ZELLIJ_CONFIG.to_owned();
        config.push_str(&format!(
            "\nload_plugins {{\n {} {{\n",
            serde_json::to_string(&plugin_url)?,
        ));
        for (key, value) in &owner_configuration {
            config.push_str(&format!("  {key} {}\n", serde_json::to_string(value)?));
        }
        config.push_str(" }\n}\n");
        private_file(&config_path, config.as_bytes())?;
        private_file(&layout_path, layout.as_bytes())?;
    }
    let agent_lease = crate::agent_relay::acquire_locked(program, &runtime, agent_source)?;
    let mut client = zellij_command(zellij, &config_dir, &socket_dir);
    client
        .arg("--config")
        .arg(&config_path)
        .env("BKPR_ZELLIJ", zellij);
    for name in [
        "BKPR_SESSION_ID",
        "BKPR_SESSION_NAME",
        "BKPR_WORKSPACE_ID",
        "BKPR_WORKSPACE_NAME",
        "BKPR_PANE_ID",
        "BKPR_CONTEXT_OWNER",
        "BKPR_NATIVE_CONTEXT",
    ] {
        client.env_remove(name);
    }
    if let Some(context) = &session_context {
        client
            .env("BKPR_SESSION_ID", &context.session_id)
            .env("BKPR_SESSION_NAME", &context.session_name)
            .env("BKPR_NATIVE_CONTEXT", "1");
    } else {
        // LEGACY_COMPAT(next-major): attach without rewriting the old server's
        // assets or claiming context it cannot publish. Remove next major.
        eprintln!(
            "bkpr: attaching to a legacy session; workspace context requires a fresh session"
        );
    }
    client.env("SSH_AUTH_SOCK", agent_lease.socket());
    if !existing {
        let bin = runtime.join("bin");
        let inherited_wrapper = env::var_os("BKPR_HARNESS_WRAPPER_DIR").map(PathBuf::from);
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
            .env("BKPR_HARNESS_WRAPPER_DIR", bin);
    }
    if existing {
        client.args(["attach", &zellij_session]);
        if options.detach_others {
            client.arg("--detach-others");
        }
    } else {
        client
            .args(["--session", &zellij_session, "--new-session-with-layout"])
            .arg(&layout_path);
    }
    // Claim the record for this new session id before its owner can write; attach never claims.
    if let Some(context) = &session_context
        && let Some(warning) = claim_record(&mode, &mut |keep| {
            recovery::claim(session, &context.session_id, keep)
        })?
    {
        eprintln!("bkpr: {warning}");
    }
    let mut child = client.spawn().context("launch native Zellij client")?;
    if !existing {
        // Keep the bootstrap and startup configuration paired until this server
        // is observable. A concurrent first attachment must reuse this identity.
        let ready = (|| -> Result<()> {
            while child.try_wait()?.is_none() {
                if session_exists(zellij, &config_dir, &socket_dir, &zellij_session)? {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(())
        })();
        if let Err(error) = ready {
            // This is our unreaped child: its PID cannot have been reused.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            let _ = child.wait();
            return Err(error).context("wait for native session startup");
        }
    }
    drop(lock);
    let status = child.wait().context("wait for native Zellij client")?;
    agent_lease
        .release()
        .context("release SSH-agent attachment lease")?;
    if !status.success() {
        bail!("Zellij exited with {status}");
    }
    Ok(())
}

fn terminal_columns() -> Result<usize> {
    let mut dimensions = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // TIOCGWINSZ initializes winsize on success; stdin is the launcher's verified TTY.
    if unsafe {
        libc::ioctl(
            std::io::stdin().as_raw_fd(),
            libc::TIOCGWINSZ,
            dimensions.as_mut_ptr(),
        )
    } == -1
    {
        return Err(std::io::Error::last_os_error())
            .context("read terminal size; launch from a TTY with a configured window size");
    }
    let columns = usize::from(unsafe { dimensions.assume_init() }.ws_col);
    if columns == 0 {
        bail!("terminal reports zero columns; configure the TTY window size before launching");
    }
    Ok(columns)
}

fn zellij_command(program: &Path, config_dir: &Path, socket_dir: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("ZELLIJ") {
            command.env_remove(key);
        }
    }
    command
        .arg("--config-dir")
        .arg(config_dir)
        .env("ZELLIJ_CONFIG_DIR", config_dir)
        .env("ZELLIJ_SOCKET_DIR", socket_dir);
    command
}

fn session_exists(
    program: &Path,
    config_dir: &Path,
    socket_dir: &Path,
    name: &str,
) -> Result<bool> {
    let result = zellij_command(program, config_dir, socket_dir)
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
    let bundled = program.with_file_name("bkpr-zellij.wasm");
    let path = explicit.unwrap_or_else(|| {
        if bundled.is_file() {
            bundled
        } else {
            program
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new("target"))
                .join("wasm32-wasip1/release/bkpr-zellij.wasm")
        }
    });
    fs::canonicalize(&path).with_context(|| format!("frontend not found at {}; build `cargo build -p bkpr-zellij --target wasm32-wasip1 --release` or use the bundled Nix package", path.display()))
}

fn check_zellij(program: &Path) -> Result<()> {
    let result = Command::new(program).arg("--version").output()?;
    let version = String::from_utf8(result.stdout)?;
    if !result.status.success() || version.trim() != "zellij 0.45.1-bkpr.3" {
        bail!(
            "this frontend requires Zellij 0.45.1-bkpr.3; detected {}",
            version.trim()
        );
    }
    Ok(())
}

pub(crate) fn zellij_socket_dir() -> Result<PathBuf> {
    let path = runtime_root()?.join("z");
    private_directory(&path)?;
    Ok(path)
}

pub(crate) fn runtime_root() -> Result<PathBuf> {
    let base = if let Some(dir) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("bkpr")
    } else {
        let state = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .context("HOME or XDG_STATE_HOME is required")?;
        state.join("bkpr/run")
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

pub(crate) fn startup_lock(runtime: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(runtime.join("startup.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(std::io::Error::last_os_error()).context("lock session startup");
    }
    Ok(file)
}

fn hash_identity(identity: &str) -> u64 {
    identity.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

pub(crate) fn validate_session(session: &str) -> Result<()> {
    if session.is_empty() || session.chars().any(char::is_control) {
        bail!("session names must not be empty or contain control characters");
    }
    Ok(())
}

// The sidebar accepts keyboard focus only through the explicit control binding.
const ZELLIJ_CONFIG: &str = r#"default_mode "normal"
pane_frames false
dangerously_enable_paste_buffer_read true
mouse_hover_effects true
focus_follows_mouse false
mouse_click_through true
session_serialization false
show_startup_tips false
show_release_notes false
theme "bkpr"
themes {
    bkpr {
        frame_selected {
            base 8
            emphasis_0 3
            emphasis_1 2
            emphasis_2 5
            emphasis_3 4
        }
    }
}
keybinds {
    locked {
        bind "Ctrl Space" { MessagePlugin { name "bkpr-controls"; payload "toggle"; }; }
    }
    normal {
        bind "Ctrl Space" { MessagePlugin { name "bkpr-controls"; payload "toggle"; }; }
        bind "Esc" { MessagePlugin { name "bkpr-controls"; payload "leave"; }; SwitchToMode "Locked"; }
        bind "Tab" { FocusNextPane; }
        bind "Shift Tab" { FocusPreviousPane; }
        bind "Ctrl d" { Detach; }
    }
}
"#;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_others_is_opt_in_and_has_a_short_option() {
        use clap::Parser;

        assert!(
            !crate::Cli::try_parse_from(["bkpr"])
                .unwrap()
                .launch
                .detach_others
        );
        for flag in ["-d", "--detach-others"] {
            let cli = crate::Cli::try_parse_from(["bkpr", flag]).unwrap();
            assert!(cli.launch.detach_others);
        }
    }

    fn record(panes: usize) -> RecoveryRecord {
        use bkpr::recovery_contract::{RecoveryPane, RecoveryTab, RecoveryWorkspace, SplitNode};
        let pane = |index: usize| RecoveryPane {
            cwd: Some("/work".into()),
            name: format!("p{index}"),
            renamed: false,
            harness: None,
            agent_id: None,
            focused: index == 0,
        };
        RecoveryRecord {
            version: bkpr::recovery_contract::RECOVERY_VERSION,
            session_name: "work".into(),
            session_id: "old".into(),
            sequence: 1,
            written_by: "test".into(),
            updated_at_millis: 0,
            workspaces: if panes == 0 {
                Vec::new()
            } else {
                vec![RecoveryWorkspace {
                    name: "w".into(),
                    tabs: vec![RecoveryTab {
                        name: "t".into(),
                        focused: true,
                        layout: SplitNode::Stack {
                            panes: (0..panes).map(pane).collect(),
                        },
                    }],
                }]
            },
        }
    }

    /// Runs `decide` and reports how often it asked.
    fn decide_with(
        intent: Intent,
        live: bool,
        record: Option<RecoveryRecord>,
        overwrite: bool,
        interactive: bool,
        answer: bool,
    ) -> (Result<StartMode>, usize) {
        let mut asked = 0;
        let mode = decide(
            "work",
            intent,
            live,
            record,
            overwrite,
            interactive,
            &mut || {
                asked += 1;
                Ok(answer)
            },
        );
        (mode, asked)
    }

    #[test]
    fn open_live_attaches() {
        let (mode, asked) = decide_with(Intent::Open, true, Some(record(2)), false, true, true);
        assert!(matches!(mode.unwrap(), StartMode::Attach));
        assert_eq!(asked, 0);
    }

    #[test]
    fn resume_live_refuses() {
        let (mode, asked) = decide_with(Intent::Resume, true, Some(record(2)), false, true, true);
        let error = format!("{:#}", mode.err().expect("live resume refuses"));
        assert!(error.contains("bkpr --session work"), "{error}");
        assert_eq!(asked, 0);
    }

    #[test]
    fn resume_without_usable_record_refuses() {
        for record in [None, Some(record(0))] {
            let (mode, asked) = decide_with(Intent::Resume, false, record, false, true, true);
            let error = format!("{:#}", mode.err().expect("nothing to resume"));
            assert!(error.contains("no recovery record"), "{error}");
            assert_eq!(asked, 0);
        }
    }

    #[test]
    fn resume_restores_without_asking() {
        let (mode, asked) =
            decide_with(Intent::Resume, false, Some(record(2)), false, false, false);
        assert!(matches!(mode.unwrap(), StartMode::Restore(record) if record.pane_count() == 2));
        assert_eq!(asked, 0);
    }

    #[test]
    fn open_over_record_prompts_and_recovers() {
        let (mode, asked) = decide_with(Intent::Open, false, Some(record(2)), false, true, true);
        assert!(matches!(mode.unwrap(), StartMode::Restore(record) if record.pane_count() == 2));
        assert_eq!(asked, 1);
    }

    #[test]
    fn open_over_record_prompts_and_overwrites() {
        let (mode, asked) = decide_with(Intent::Open, false, Some(record(2)), false, true, false);
        assert!(matches!(mode.unwrap(), StartMode::Fresh));
        assert_eq!(asked, 1);
    }

    #[test]
    fn open_over_record_noninteractive_refuses() {
        let (mode, asked) = decide_with(Intent::Open, false, Some(record(2)), false, false, true);
        let error = format!("{:#}", mode.err().expect("cannot ask"));
        assert!(error.contains("bkpr resume --session work"), "{error}");
        assert!(error.contains("--overwrite"), "{error}");
        assert_eq!(asked, 0);
    }

    #[test]
    fn open_over_record_with_overwrite_is_fresh() {
        let (mode, asked) = decide_with(Intent::Open, false, Some(record(2)), true, true, true);
        assert!(matches!(mode.unwrap(), StartMode::Fresh));
        assert_eq!(asked, 0);
    }

    #[test]
    fn open_without_record_is_fresh() {
        for record in [None, Some(record(0))] {
            let (mode, asked) = decide_with(Intent::Open, false, record, false, false, true);
            assert!(matches!(mode.unwrap(), StartMode::Fresh));
            assert_eq!(asked, 0);
        }
    }

    #[test]
    fn prompt_answers_parse_strictly() {
        for answer in ["r", "R", "recover", " Recover\n"] {
            assert_eq!(parse_answer(answer), Some(true), "{answer:?}");
        }
        for answer in ["o", "O", "overwrite", "overwrite\n"] {
            assert_eq!(parse_answer(answer), Some(false), "{answer:?}");
        }
        for answer in ["", "\n", "y", "yes", "re", "over"] {
            assert_eq!(parse_answer(answer), None, "{answer:?}");
        }
    }

    #[test]
    fn restore_layout_resumes_harnesses_and_seeds_sticky_names() {
        use crate::relay::{RecoveredAgent, RecoveryAgents};
        use bkpr::recovery_contract::{RecoveryPane, RecoveryTab, RecoveryWorkspace, SplitNode};
        let pane = |name: &str, renamed: bool, agent: Option<&str>| RecoveryPane {
            cwd: Some("/".into()),
            name: name.into(),
            renamed,
            harness: agent.map(|_| "claude".into()),
            agent_id: agent.map(str::to_owned),
            focused: false,
        };
        let mut record = record(0);
        record.workspaces = vec![RecoveryWorkspace {
            name: "w".into(),
            tabs: vec![RecoveryTab {
                name: "t".into(),
                focused: true,
                layout: SplitNode::Split {
                    direction: bkpr::recovery_contract::SplitDirection::Vertical,
                    children: vec![
                        SplitNode::Pane {
                            pane: pane("Build", true, None),
                        },
                        SplitNode::Stack {
                            panes: vec![
                                pane("shell", false, None),
                                pane("Agent", true, Some("a1")),
                            ],
                        },
                    ],
                },
            }],
        }];
        let configuration = BTreeMap::from([("role".to_owned(), "sidebar".to_owned())]);
        let agents = RecoveryAgents::from_entries([(
            "a1".to_owned(),
            RecoveredAgent {
                harness: "claude".into(),
                session_id: Some("native-1".into()),
                live: false,
                queue_endpoint: None,
            },
        )]);
        let restore = restore_layout(
            "file:/bkpr.wasm",
            &configuration,
            30,
            "new-session",
            &record,
            "/bin/bkpr",
            Ok(&agents),
        )
        .unwrap();
        assert_eq!(restore.expected_panes, 3);
        assert_eq!(
            restore.names,
            BTreeMap::from([
                ("w0t0p0".to_owned(), "Build".to_owned()),
                ("w0t0p2".to_owned(), "Agent".to_owned()),
            ])
        );
        assert!(
            restore
                .layout
                .contains("bkpr_workspace_id=\"new-session:workspace:bkpr:00000\"")
        );
        assert!(restore.layout.contains("command=\"/bin/bkpr\""));
        assert!(restore.layout.contains("\"restore-pane\" \"--key=w0t0p2\""));
        assert!(
            restore
                .layout
                .contains("\"--harness=claude\" \"--session-id=native-1\"")
        );
        assert!(!restore.layout.contains("restore_names"));
        let a1 = bkpr::recovery_contract::RestoreLink {
            harness: "claude".into(),
            agent_id: "a1".into(),
        };
        // A resumed harness keeps its link seeded until the pane reports otherwise.
        assert_eq!(
            restore.links,
            BTreeMap::from([("w0t0p2".to_owned(), a1.clone())])
        );
        assert!(
            restore
                .layout
                .contains("\"--link-harness=claude\" \"--link-agent-id=a1\"")
        );
        assert!(!restore.layout.contains("restore_links"));

        // Unreadable relay state never fails the resume: harness panes become noted shells.
        let restore = restore_layout(
            "file:/bkpr.wasm",
            &configuration,
            30,
            "new-session",
            &record,
            "/bin/bkpr",
            Err("permission denied"),
        )
        .unwrap();
        assert!(!restore.layout.contains("--harness="));
        assert!(
            restore
                .layout
                .contains("relay state was unreadable: permission denied")
        );
        assert_eq!(restore.expected_panes, 3);
        // ...and keep their association for a retry once relay state is readable.
        assert_eq!(restore.links, BTreeMap::from([("w0t0p2".to_owned(), a1)]));
    }

    /// Two recorded panes can name one native session (a retained link plus its resume
    /// elsewhere): only the first leaf resumes it; the later one is a noted shell.
    #[test]
    fn restore_layout_resumes_each_session_once() {
        use crate::relay::{RecoveredAgent, RecoveryAgents};
        use bkpr::recovery_contract::{RecoveryPane, RecoveryTab, RecoveryWorkspace, SplitNode};
        let pane = |agent: &str| SplitNode::Pane {
            pane: RecoveryPane {
                cwd: Some("/".into()),
                name: agent.into(),
                renamed: false,
                harness: Some("claude".into()),
                agent_id: Some(agent.into()),
                focused: false,
            },
        };
        let mut record = record(0);
        record.workspaces = vec![RecoveryWorkspace {
            name: "w".into(),
            tabs: vec![RecoveryTab {
                name: "t".into(),
                focused: true,
                layout: SplitNode::Split {
                    direction: bkpr::recovery_contract::SplitDirection::Vertical,
                    children: vec![pane("old"), pane("new")],
                },
            }],
        }];
        let dead = |session: &str| RecoveredAgent {
            harness: "claude".into(),
            session_id: Some(session.into()),
            live: false,
            queue_endpoint: None,
        };
        let agents = RecoveryAgents::from_entries([
            ("old".to_owned(), dead("shared")),
            ("new".to_owned(), dead("shared")),
        ]);
        let restore = restore_layout(
            "file:/bkpr.wasm",
            &BTreeMap::new(),
            30,
            "new-session",
            &record,
            "/bin/bkpr",
            Ok(&agents),
        )
        .unwrap();
        assert_eq!(restore.layout.matches("--session-id=shared").count(), 1);
        assert!(restore.layout.contains("\"--key=w0t0p0\""));
        let second = restore
            .layout
            .lines()
            .skip_while(|line| !line.contains("--key=w0t0p1"))
            .next()
            .unwrap();
        assert!(
            second.contains("--note=session is still open elsewhere"),
            "{second}"
        );
        assert!(!second.contains("--harness="), "{second}");
        // The duplicate is not a transient refusal: it keeps no link to retry.
        assert_eq!(restore.links.keys().collect::<Vec<_>>(), ["w0t0p0"]);
    }

    #[test]
    fn resume_requires_a_session_and_takes_launch_options() {
        use clap::Parser;

        let cli =
            crate::Cli::try_parse_from(["bkpr", "resume", "--session", "work", "-d"]).unwrap();
        let Some(crate::Commands::Resume { launch }) = cli.command else {
            panic!("expected resume");
        };
        assert_eq!(launch.session.as_deref(), Some("work"));
        assert!(launch.detach_others && !launch.overwrite);
        let cli = crate::Cli::try_parse_from(["bkpr", "resume"]).unwrap();
        let Some(crate::Commands::Resume { launch }) = cli.command else {
            panic!("expected resume");
        };
        let error = run_resume(launch).unwrap_err().to_string();
        assert_eq!(error, "bkpr resume requires --session <name>");
        let cli = crate::Cli::try_parse_from(["bkpr", "--session", "work", "--overwrite"]).unwrap();
        assert!(cli.launch.overwrite);
    }

    #[test]
    fn unresolvable_state_root_warns_and_starts_fresh() {
        let loaded = Err(anyhow::Error::new(crate::recovery::StateUnavailable));
        let (record, warning) = loaded_record("work", Intent::Open, false, loaded).unwrap();
        assert!(record.is_none());
        let warning = warning.expect("recovery is off");
        assert!(
            warning.starts_with("this session will not be resumable: "),
            "{warning}"
        );
        let (mode, asked) = decide_with(Intent::Open, false, record, false, true, true);
        assert!(matches!(mode.unwrap(), StartMode::Fresh));
        assert_eq!(asked, 0);
    }

    #[test]
    fn unreadable_record_requires_overwrite() {
        let unreadable = || Err(anyhow::anyhow!("malformed recovery record"));
        for intent in [Intent::Open, Intent::Resume] {
            let error = loaded_record("work", intent, false, unreadable()).unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains("bkpr --session work --overwrite"), "{error}");
            assert!(error.contains("malformed recovery record"), "{error}");
        }
        let (discarded, warning) = loaded_record("work", Intent::Open, true, unreadable()).unwrap();
        assert!(discarded.is_none() && warning.is_none());
        let (kept, warning) =
            loaded_record("work", Intent::Open, false, Ok(Some(record(1)))).unwrap();
        assert!(kept.is_some() && warning.is_none());
    }

    #[test]
    fn fresh_claims_fail_open_and_restores_fail_closed() {
        let mut calls = Vec::new();
        let mut failing = |keep: Option<&RecoveryRecord>| {
            calls.push(keep.map(RecoveryRecord::pane_count));
            Err(anyhow::anyhow!("read-only state"))
        };
        assert!(
            claim_record(&StartMode::Attach, &mut failing)
                .unwrap()
                .is_none()
        );
        let warning = claim_record(&StartMode::Fresh, &mut failing)
            .unwrap()
            .expect("fresh warns");
        assert_eq!(
            warning,
            "this session will not be resumable: read-only state"
        );
        let error = claim_record(&StartMode::Restore(record(2)), &mut failing).unwrap_err();
        assert!(format!("{error:#}").contains("read-only state"));
        // Attach never claims; fresh clears; restore keeps the record's workspaces.
        assert_eq!(calls, [None, Some(2)]);
        let mut succeeding = |_: Option<&RecoveryRecord>| Ok(());
        assert!(
            claim_record(&StartMode::Fresh, &mut succeeding)
                .unwrap()
                .is_none()
        );
        assert!(
            claim_record(&StartMode::Restore(record(1)), &mut succeeding)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resume_rejects_overwrite_and_detach_others() {
        use clap::Parser;

        for flag in ["--overwrite", "-d", "--detach-others"] {
            let cli =
                crate::Cli::try_parse_from(["bkpr", "resume", "--session", "work", flag]).unwrap();
            let Some(crate::Commands::Resume { launch }) = cli.command else {
                panic!("expected resume");
            };
            let error = run_resume(launch).unwrap_err().to_string();
            assert!(error.starts_with("bkpr resume"), "{flag}: {error}");
            assert!(
                error.contains(flag.trim_start_matches('-').split('-').next().unwrap()),
                "{flag}: {error}"
            );
        }
    }

    #[test]
    fn refusals_quote_session_names_for_copying() {
        assert_eq!(shell_word("work-2.x"), "work-2.x");
        assert_eq!(shell_word("my work"), "'my work'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
    }

    #[test]
    fn session_names_have_no_remote_backend_character_or_length_limits() {
        for session in ["work", "仕事 / workspace", ".", "..", &"x".repeat(128)] {
            assert!(validate_session(session).is_ok());
        }
        for session in ["", "work\n", "work\0"] {
            assert!(validate_session(session).is_err());
        }
    }
}
