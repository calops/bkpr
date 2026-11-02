use anyhow::{Context, Result, ensure};
use bkpr_core::{registry::Registry, transport};
use bkpr_protocol::{PaneSpawnSpec, Size};
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
    #[arg(long, default_value_t = 100)]
    cols: u16,
    #[arg(long, default_value_t = 30)]
    rows: u16,
    #[arg(long, default_value_t = 8)]
    cell_width: u32,
    #[arg(long, default_value_t = 16)]
    cell_height: u32,
    /// Start current argv in a new workspace when no execution is still running.
    /// Historical workspace layouts and pane records are never replayed or replaced.
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
    let result = (|| -> Result<()> {
        let listener = transport::bind(&cli.socket)?;
        let executable = match cli.bkpr {
            Some(executable) => executable,
            None => std::env::current_exe()?.with_file_name("bkpr"),
        };
        registry.configure_harnesses(&cli.socket, &executable)?;
        if !cli.argv.is_empty() {
            let isolated_environment = cli.home.is_some();
            let home = cli
                .home
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .context("HOME is unavailable; supply --home")?;
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
            registry.bootstrap(PaneSpawnSpec {
                argv: cli.argv,
                cwd,
                home,
                isolated_environment,
                title: None,
                size: Size {
                    cols: cli.cols,
                    rows: cli.rows,
                    cell_width: cli.cell_width,
                    cell_height: cli.cell_height,
                },
            })?;
        }
        listener.serve(registry.clone())
    })();
    // Even startup/bind failures must tear down only the children this daemon owns.
    let cleanup = registry.shutdown();
    result.and(cleanup)
}
