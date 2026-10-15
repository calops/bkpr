mod agent;
mod agent_relay;
mod ci;
mod claude;
mod launcher;
mod project;
mod project_watch;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use shephrd::MAX_MESSAGE_BYTES;

#[derive(Debug, Parser)]
#[command(
    name = "shephrd",
    version,
    disable_help_subcommand = true,
    override_usage = "shephrd [OPTIONS]",
    about = "Persistent local Zellij workspaces with an agent-aware sidebar",
    after_help = "Examples:\n  shephrd                         Open or attach to the default local session\n  shephrd --session work          Open or attach to a named local session\n  shephrd --sidebar-width 44      Set the sidebar width\n\nSessions use native Zellij terminals.\nAutomatic Git branch, diff and CI reporting is available on Linux."
)]
struct Cli {
    #[command(flatten)]
    launch: launcher::Options,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect currently live foreground agent processes.
    #[command(hide = true)]
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Discover a local directory's project name.
    #[command(hide = true)]
    Project {
        #[arg(long)]
        cwd: PathBuf,
    },
    /// Synchronize the native session's event-driven Git subscriptions.
    #[command(hide = true)]
    ProjectWatch {
        #[arg(long)]
        json: String,
    },
    /// Own native filesystem subscriptions until the Zellij server exits.
    #[command(hide = true)]
    ProjectWatchDaemon {
        #[arg(long)]
        json: String,
        #[arg(long)]
        listener_fd: i32,
    },
    /// Launch the real Claude with session-local title callbacks.
    #[command(hide = true)]
    ClaudeLaunch {
        #[arg(long)]
        wrapper_dir: PathBuf,
        #[arg(last = true)]
        arguments: Vec<std::ffi::OsString>,
    },
    /// Receive a Claude UserPromptSubmit hook payload.
    #[command(hide = true)]
    ClaudeTitle,
    /// Own a managed session's SSH-agent relay until its last attachment ends.
    #[command(hide = true)]
    AgentRelayDaemon {
        #[arg(long)]
        runtime: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommands {
    Inspect {
        #[arg(long)]
        json: String,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("shephrd: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => launcher::run(cli.launch),
        Some(Commands::ClaudeLaunch {
            wrapper_dir,
            arguments,
        }) => claude::launch(&wrapper_dir, arguments),
        Some(Commands::ClaudeTitle) => claude::prompt_title(),
        Some(Commands::AgentRelayDaemon { runtime }) => agent_relay::daemon(&runtime),
        Some(Commands::Agent { command }) => match command {
            AgentCommands::Inspect { json } => {
                let response = serde_json::to_vec(&agent::inspect(&json)?)?;
                if response.len() > MAX_MESSAGE_BYTES {
                    bail!("agent inspect response exceeds {MAX_MESSAGE_BYTES} bytes");
                }
                let mut stdout = io::stdout().lock();
                stdout.write_all(&response)?;
                writeln!(stdout)?;
                stdout.flush()?;
                Ok(())
            }
        },
        Some(Commands::Project { cwd }) => {
            let project = project::discover(&cwd)?;
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, &project)?;
            writeln!(stdout)?;
            stdout.flush()?;
            Ok(())
        }
        Some(Commands::ProjectWatch { json }) => project_watch::sync(&json),
        Some(Commands::ProjectWatchDaemon { json, listener_fd }) => {
            project_watch::serve(&json, listener_fd)
        }
    }
}
