use anyhow::{Context, Result, ensure};
use bkpr_core::{registry::Registry, transport};
use bkpr_protocol::{Operation, PaneSpawnSpec, Request, Size};
use clap::Parser;
use std::path::PathBuf;

/// Own panes independently of connected clients. Only metadata/layout is persisted.
#[derive(Debug, Parser)]
#[command(name = "bkprd", version)]
struct Cli {
    /// Explicit private Unix socket. A live listener is never replaced.
    #[arg(long)]
    socket: PathBuf,
    /// Explicit metadata root; exclusive daemon ownership, no terminal contents.
    #[arg(long)]
    state_root: PathBuf,
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
    /// Bootstrap a first workspace/pane only on a new metadata root.
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
    let registry = Registry::open(&cli.state_root)?;
    let result = (|| -> Result<()> {
        if registry.is_fresh() && !cli.argv.is_empty() {
            let isolated_environment = cli.home.is_some();
            let home = cli
                .home
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .context("HOME is unavailable; supply --home")?;
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
            let created = registry.request(Request::new(Operation::WorkspaceCreate {
                name: "Main".into(),
            }))?;
            let workspace_id = created["workspace_id"]
                .as_str()
                .context("missing created workspace id")?
                .to_owned();
            registry.request(Request::new(Operation::PaneSpawn {
                workspace_id: Some(workspace_id),
                expected_revision: Some(1),
                spec: PaneSpawnSpec {
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
                },
            }))?;
        }
        transport::serve(&cli.socket, registry.clone())
    })();
    // Even startup/bind failures must tear down only the children this daemon owns.
    let cleanup = registry.shutdown();
    result.and(cleanup)
}
