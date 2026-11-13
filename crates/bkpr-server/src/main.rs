//! Daemon startup, service wiring and owned shutdown over `bkpr-core`.

use anyhow::{Context, Result, ensure};
use bkpr_core::{diagnostics, registry::Registry, transport};
use bkpr_protocol::{PaneSpawnSpec, Size, SpawnDefaults};
use clap::Parser;
use std::path::PathBuf;

/// Own panes independently of connected clients. Only metadata/layout is persisted.
#[derive(Debug, Parser)]
#[command(name = "bkprd", version)]
struct Cli {
    /// Exact logical session name, independent of paths and this daemon's birth.
    #[arg(short = 's', long, default_value = bkpr_protocol::DEFAULT_SESSION_NAME)]
    session: String,
    /// Explicit private Unix socket. A live listener is never replaced.
    #[arg(long)]
    socket: PathBuf,
    /// Explicit metadata root; exclusive daemon ownership, no terminal contents.
    #[arg(long)]
    state_root: PathBuf,
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
    ensure!(cli.socket.is_absolute(), "socket must be absolute");
    let registry = Registry::open_named(&cli.state_root, &cli.session)?;
    // Daemon-local opt-in, not a process-global logger installed by the library.
    // Diagnostics are never a prerequisite for durable control or pane startup.
    let diagnostic_guard =
        match diagnostics::install(&cli.state_root, registry.diagnostic_session_id()) {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("bkprd: private diagnostics disabled: {error}");
                None
            }
        };
    diagnostics::record("daemon.start", None, "success");
    let result = (|| -> Result<()> {
        let initial_requested = !cli.argv.is_empty();
        let startup = (|| -> Result<Option<PaneSpawnSpec>> {
            let isolated_environment = cli.home.is_some();
            let home = cli
                .home
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .context("HOME is unavailable; supply --home")?;
            let cwd = match cli.cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir()?,
            };
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
                cwd,
                home,
                isolated_environment,
            };
            let initial = initial_requested.then(|| PaneSpawnSpec {
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
            });
            registry.configure_spawn_defaults(defaults)?;
            Ok(initial)
        })();
        let initial = if initial_requested {
            startup?
        } else {
            // Headless startup stays usable without complete, valid daemon defaults.
            // Never substitute programs/paths or infer them from saved panes.
            startup.unwrap_or(None)
        };
        let listener = transport::bind(&cli.socket)?;
        let executable = match cli.bkpr {
            Some(executable) => executable,
            None => std::env::current_exe()?.with_file_name("bkpr"),
        };
        registry.configure_harnesses(&cli.socket, &executable)?;
        registry.configure_agent_forwarding(&cli.socket)?;
        diagnostics::record("daemon.listen", None, "success");
        listener.serve_bootstrap(registry.clone(), initial)
    })();
    // Even startup/bind failures must tear down only the children this daemon owns.
    let cleanup = registry.shutdown();
    diagnostics::record(
        "daemon.stop",
        None,
        if result.is_ok() && cleanup.is_ok() {
            "success"
        } else {
            "failed"
        },
    );
    if let Some(guard) = diagnostic_guard {
        let report = guard.finish();
        if report.dropped != 0 || report.io_failures != 0 || report.pending != 0 || !report.stopped
        {
            eprintln!(
                "bkprd: best-effort diagnostics: written={} dropped={} io_failures={} pending={} stopped={}",
                report.written, report.dropped, report.io_failures, report.pending, report.stopped,
            );
        }
    }
    result.and(cleanup)
}
