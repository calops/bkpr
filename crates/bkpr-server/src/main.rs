//! Daemon startup, service wiring and owned shutdown over `bkpr-core`.

use anyhow::{Context, Result, ensure};
use bkpr_core::{
    daemon::{HostConfig, HostDaemon},
    diagnostics,
};
use bkpr_protocol::{PaneSpawnSpec, Size, SpawnDefaults};
use clap::Parser;
use std::path::PathBuf;

/// Own panes independently of connected clients. Only metadata/layout is persisted.
#[derive(Debug, Parser)]
#[command(name = "bkprd", version)]
struct Cli {
    /// Optional initial session; argv alone selects the default session.
    #[arg(short = 's', long)]
    session: Option<String>,
    /// Private host control socket. A live listener is never replaced.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Private host metadata root; named sessions retain independent roots.
    #[arg(long)]
    state_root: Option<PathBuf>,
    /// Bkpr utility executable used by private automatic harness wrappers.
    #[arg(long)]
    bkpr: Option<PathBuf>,
    /// An explicit fixture home scrubs inherited credentials/environment.
    /// Omit for the real user's normal environment and profiles.
    #[arg(long)]
    home: Option<PathBuf>,
    /// Initial pane working directory (defaults to invocation cwd).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Initial terminal width in character cells.
    #[arg(long, default_value_t = 100)]
    cols: u16,
    /// Initial terminal height in character cells.
    #[arg(long, default_value_t = 30)]
    rows: u16,
    /// Initial character-cell width in pixels.
    #[arg(long, default_value_t = 8)]
    cell_width: u32,
    /// Initial character-cell height in pixels.
    #[arg(long, default_value_t = 16)]
    cell_height: u32,
    /// Restore saved owned panes/layout using this current shell command.
    /// Without argv, startup executes nothing; daemon SHELL only configures future panes.
    #[arg(last = true)]
    argv: Vec<String>,
}
fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("bkprd: {error:#}");
        std::process::exit(1);
    }
}
fn run(cli: Cli) -> Result<()> {
    let socket = cli
        .socket
        .map(Ok)
        .unwrap_or_else(HostConfig::default_socket)?;
    let state_root = cli
        .state_root
        .map(Ok)
        .unwrap_or_else(HostConfig::default_state_root)?;
    ensure!(socket.is_absolute(), "socket must be absolute");
    let selected = cli
        .session
        .or_else(|| (!cli.argv.is_empty()).then(|| bkpr_protocol::DEFAULT_SESSION_NAME.into()));
    let initial_requested = !cli.argv.is_empty();
    let startup = if selected.is_some() {
        let defaults = (|| -> Result<SpawnDefaults> {
            let home = cli
                .home
                .clone()
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .context("HOME is unavailable; supply --home")?;
            let argv = if initial_requested {
                cli.argv
            } else {
                vec![
                    std::env::var("SHELL")
                        .ok()
                        .filter(|shell| !shell.is_empty())
                        .context("SHELL is unavailable for future panes")?,
                ]
            };
            let defaults = SpawnDefaults {
                argv,
                cwd: match cli.cwd {
                    Some(cwd) => cwd,
                    None => std::env::current_dir()?,
                },
                home,
                isolated_environment: cli.home.is_some(),
            };
            bkpr_core::registry::validate_spawn_defaults(&defaults)?;
            Ok(defaults)
        })();
        let defaults = if initial_requested {
            Some(defaults?)
        } else {
            defaults.ok()
        };
        let initial = initial_requested.then(|| {
            let defaults = defaults.as_ref().expect("explicit startup defaults");
            PaneSpawnSpec {
                argv: defaults.argv.clone(),
                cwd: defaults.cwd.clone(),
                home: defaults.home.clone(),
                isolated_environment: defaults.isolated_environment,
                title: None,
                size: Size {
                    cols: cli.cols,
                    rows: cli.rows,
                    cell_width: cli.cell_width,
                    cell_height: cli.cell_height,
                },
            }
        });
        Some((defaults, initial))
    } else {
        None
    };
    let executable = match cli.bkpr {
        Some(executable) => executable,
        None => std::env::current_exe()?.with_file_name("bkpr"),
    };
    let host = HostDaemon::bind(HostConfig {
        socket,
        state_root: state_root.clone(),
        bkpr: executable,
    })?;
    let diagnostic_guard = match diagnostics::install(&state_root, &host.info().daemon_id) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("bkprd: private diagnostics disabled: {error}");
            None
        }
    };
    diagnostics::record("daemon.start", None, "success");
    let result = match selected.zip(startup) {
        Some((name, (defaults, initial))) => host.serve_initial(name, defaults, initial),
        None => host.serve(),
    };
    diagnostics::record(
        "daemon.stop",
        None,
        if result.is_ok() { "success" } else { "failed" },
    );
    if let Some(guard) = diagnostic_guard {
        let report = guard.finish();
        if report.dropped != 0 || report.io_failures != 0 || report.pending != 0 || !report.stopped
        {
            eprintln!(
                "bkprd: best-effort diagnostics: written={} dropped={} io_failures={} pending={} stopped={}",
                report.written, report.dropped, report.io_failures, report.pending, report.stopped
            );
        }
    }
    result
}
