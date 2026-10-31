//! TUI application and typed local daemon transport. No backend implementation
//! dependency is linked into a frontend.
mod app;
mod controller;
pub mod transport;
mod updates;
use anyhow::{Context, Result, bail};
pub use app::Options;
use bkpr_protocol::*;
use bkpr_tui_framework::{ClientEvent, ReplyDemux, decode_input, key_event, kitty_key};
use std::{
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use termwiz::input::{InputEvent, InputParser};

pub fn tui(socket: &Path, read_only: bool, clipboard_policy: &str) -> Result<()> {
    let mut options = Options::inherited()?;
    options.read_only = read_only;
    options.clipboard_policy = clipboard_policy.into();
    app::run(socket, options)
}
pub fn run(socket: &Path, options: Options) -> Result<()> {
    app::run(socket, options)
}
pub fn context(socket: &Path) -> Result<RegistrySnapshot> {
    let response: RegistryResult =
        serde_json::from_value(transport::checked(socket, Operation::Inspect)?)?;
    Ok(response.registry)
}

pub struct LocalPaths {
    pub socket: PathBuf,
    pub state_root: PathBuf,
}
impl LocalPaths {
    pub fn defaults() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/bkpr-{}", unsafe { libc::geteuid() })));
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        Ok(Self {
            socket: runtime.join("bkpr/bkprd.sock"),
            state_root: state.join("bkpr"),
        })
    }
}
fn private_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                bail!(
                    "{} must be a private owned directory (0700)",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                bail!("private daemon directory was replaced during creation");
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
/// Connect or start an independently session-owned daemon. Only connection
/// absence triggers startup; authentication/protocol failures are never hidden.
pub fn ensure_server(
    paths: &LocalPaths,
    binary: Option<&Path>,
    options: &Options,
    size: Size,
) -> Result<()> {
    match transport::checked(&paths.socket, Operation::Inspect) {
        Ok(_) => return Ok(()),
        Err(error) => {
            let unavailable = error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
            });
            if !unavailable {
                return Err(error);
            }
        }
    }
    private_directory(paths.socket.parent().context("socket needs parent")?)?;
    private_directory(&paths.state_root)?;
    let bkpr = std::env::current_exe().context("locating bkpr for managed harness launches")?;
    let server = if let Some(binary) = binary {
        binary.to_owned()
    } else {
        let sibling = bkpr.with_file_name("bkprd");
        if sibling.is_file() {
            sibling
        } else {
            PathBuf::from("bkprd")
        }
    };
    let log_path = paths.state_root.join("bkprd.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let mut command = Command::new(server);
    command
        .arg("--socket")
        .arg(&paths.socket)
        .arg("--state-root")
        .arg(&paths.state_root)
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
        if transport::checked(&paths.socket, Operation::Inspect).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            if transport::checked(&paths.socket, Operation::Inspect).is_ok() {
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
/// Physical decoder probe: ordered acceptance, never child processing or replay.
pub fn keyboard(socket: &Path, pane: &str, view: &str, bytes: &[u8]) -> Result<()> {
    let input = updates::InputPump::new(socket)?;
    let mut parser = InputParser::new();
    let mut demux = ReplyDemux::default();
    for event in decode_input(&mut parser, demux.feed(bytes), false) {
        let request = match event {
            ClientEvent::Legacy(InputEvent::Key(key)) => PaneRequest::Key {
                view_id: view.into(),
                event: key_event(key),
            },
            ClientEvent::Reply(reply) if reply.starts_with(b"key;") => PaneRequest::Key {
                view_id: view.into(),
                event: kitty_key(&reply[4..])?,
            },
            ClientEvent::Legacy(InputEvent::Paste(text)) => PaneRequest::Paste {
                view_id: view.into(),
                data: text.into_bytes(),
            },
            _ => bail!("keyboard probe accepts only keys and bracketed paste"),
        };
        if matches!(&request,PaneRequest::Key{event,..}if event.key=="BracketRight"&&event.modifiers&2!=0)
        {
            bail!("detach shortcut is not a keyboard injection");
        }
        input.send(Operation::Pane {
            pane_id: pane.into(),
            request,
        })?;
    }
    input.finish()
}
