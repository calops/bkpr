use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

#[derive(Debug, Args)]
pub struct Options {
    /// Open or attach to a named session.
    #[arg(long, help_heading = "Session")]
    pub session: Option<String>,
    #[arg(long, env = "SHEPHRD_ZELLIJ", default_value = "zellij", hide = true)]
    pub zellij: PathBuf,
    /// Frontend WASM artifact; bundled automatically by the product package.
    #[arg(long, env = "SHEPHRD_PLUGIN", hide = true)]
    pub plugin: Option<PathBuf>,
    /// Initial sidebar target in columns (16–120); resizable, rounded to integer percent.
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
    run_native(&options, &program, &zellij, &plugin)
}

fn run_native(options: &Options, program: &Path, zellij: &Path, plugin: &Path) -> Result<()> {
    let session = options.session.as_deref().unwrap_or("shephrd");
    validate_session(session)?;
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
        let sidebar_percent = shephrd::native_sidebar_percent(
            usize::from(options.sidebar_width),
            terminal_columns()?,
        )
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
                sidebar_percent,
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
        let inherited_wrapper = env::var_os("SHEPHRD_HARNESS_WRAPPER_DIR").map(PathBuf::from);
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
            .env("SHEPHRD_HARNESS_WRAPPER_DIR", bin);
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
mouse_hover_effects true
focus_follows_mouse false
mouse_click_through true
session_serialization false
show_startup_tips false
show_release_notes false
theme "shephrd"
themes {
    shephrd {
        frame_selected {
            base 108 112 134
            background 24 24 37
            emphasis_0 250 179 135
            emphasis_1 166 227 161
            emphasis_2 203 166 247
            emphasis_3 137 180 250
        }
    }
}
keybinds {
    locked {
        bind "Ctrl Space" { MessagePlugin { name "shephrd-controls"; payload "toggle"; }; }
        bind "Alt Shift Right" { NewPane "Right"; }
        bind "Alt Shift Down" { NewPane "Down"; }
    }
    normal {
        bind "Ctrl Space" { MessagePlugin { name "shephrd-controls"; payload "toggle"; }; }
        bind "Alt Shift Right" { NewPane "Right"; }
        bind "Alt Shift Down" { NewPane "Down"; }
        bind "Esc" { MessagePlugin { name "shephrd-controls"; payload "leave"; }; SwitchToMode "Locked"; }
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
    fn session_names_have_no_remote_backend_character_or_length_limits() {
        for session in ["work", "仕事 / workspace", ".", "..", &"x".repeat(128)] {
            assert!(validate_session(session).is_ok());
        }
        for session in ["", "work\n", "work\0"] {
            assert!(validate_session(session).is_err());
        }
    }
}
