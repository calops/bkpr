mod agent;
mod agent_relay;
mod ci;
mod claude;
mod codex;
mod context;
mod launcher;
mod omp;
mod project;
#[cfg(target_os = "linux")]
mod project_cache;
mod project_watch;
mod relay;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use bkpr::MAX_MESSAGE_BYTES;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "bkpr",
    version,
    disable_help_subcommand = true,
    override_usage = "bkpr [OPTIONS] [COMMAND]",
    about = "Persistent local Zellij workspaces with an agent-aware sidebar",
    after_help = "Examples:\n  bkpr                         Open or attach to the default local session\n  bkpr --session work          Open or attach to a named local session\n  bkpr --sidebar-width 44      Set the sidebar width\n\nSessions use native Zellij terminals.\nAutomatic Git branch, diff and CI reporting is available on Linux."
)]
struct Cli {
    #[command(flatten)]
    launch: launcher::Options,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Show this pane's current native session and workspace context.
    Context {
        /// Emit compact JSON.
        #[arg(long)]
        json: bool,
    },
    /// Publish the native owner's authenticated foreground context projection.
    #[command(hide = true)]
    ContextPublish {
        #[arg(long)]
        json: String,
    },
    /// Inspect communication bridge readiness and retained messages.
    Bridge {
        #[command(subcommand)]
        command: BridgeCommands,
    },
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
    /// Wait for a broker message from a launch-local Claude hook.
    #[command(hide = true)]
    ClaudeWait,
    /// Launch OMP with launch-local messaging.
    #[command(hide = true)]
    OmpLaunch {
        #[arg(long)]
        wrapper_dir: PathBuf,
        #[arg(last = true)]
        arguments: Vec<std::ffi::OsString>,
    },
    /// Launch Codex with launch-local messaging.
    #[command(hide = true)]
    CodexLaunch {
        #[arg(long)]
        wrapper_dir: PathBuf,
        #[arg(last = true)]
        arguments: Vec<std::ffi::OsString>,
    },
    #[command(hide = true)]
    CodexHook,
    #[command(hide = true)]
    CodexStop {
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Serve the authenticated launch-local MCP connection over stdio.
    #[command(hide = true)]
    RelayMcp,
    /// Register the launch-local OMP extension's future-turn receiver.
    #[command(hide = true)]
    RelayReceiver {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        disable: bool,
    },
    /// Own the private native durable message broker.
    #[command(hide = true)]
    RelayDaemon {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Own a managed session's SSH-agent relay until its last attachment ends.
    #[command(hide = true)]
    AgentRelayDaemon {
        #[arg(long)]
        runtime: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BridgeCommands {
    /// Report bridge readiness without starting or changing agent sessions.
    Status {
        /// Emit compact JSON.
        #[arg(long)]
        json: bool,
    },
    /// Read a retained message explicitly as the local operator.
    Show { message_id: String },
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
        eprintln!("bkpr: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => launcher::run(cli.launch),
        Some(Commands::Context { json }) => {
            let value = context::current()?;
            let mut stdout = io::stdout().lock();
            if json {
                serde_json::to_writer(&mut stdout, &value)?;
            } else {
                serde_json::to_writer_pretty(&mut stdout, &value)?;
            }
            writeln!(stdout)?;
            stdout.flush()?;
            Ok(())
        }
        Some(Commands::ContextPublish { json }) => context::publish(&json),
        Some(Commands::Bridge { command }) => {
            let (value, compact) = match command {
                BridgeCommands::Status { json } => (relay::status()?, json),
                BridgeCommands::Show { message_id } => {
                    (relay::inspect_message(&message_id)?, false)
                }
            };
            let mut stdout = io::stdout().lock();
            if compact {
                serde_json::to_writer(&mut stdout, &value)?;
            } else {
                serde_json::to_writer_pretty(&mut stdout, &value)?;
            }
            writeln!(stdout)?;
            stdout.flush()?;
            Ok(())
        }
        Some(Commands::ClaudeLaunch {
            wrapper_dir,
            arguments,
        }) => claude::launch(&wrapper_dir, arguments),
        Some(Commands::ClaudeTitle) => claude::prompt_title(),
        Some(Commands::ClaudeWait) => claude::wait_message(),
        Some(Commands::OmpLaunch {
            wrapper_dir,
            arguments,
        }) => omp::launch(&wrapper_dir, arguments),
        Some(Commands::CodexLaunch {
            wrapper_dir,
            arguments,
        }) => codex::launch(&wrapper_dir, arguments),
        Some(Commands::CodexHook) => codex::hook(),
        Some(Commands::CodexStop { manifest }) => codex::stop(&manifest),
        Some(Commands::RelayMcp) => relay::mcp(),
        Some(Commands::RelayReceiver { pid, disable }) => {
            relay::request(
                "receiver",
                serde_json::json!({
                    "kind": "omp_mcp",
                    "pid": pid,
                    "activity": "unknown",
                    "enabled": !disable,
                    "diagnostic": null,
                }),
            )?;
            Ok(())
        }
        Some(Commands::RelayDaemon { socket }) => relay::daemon(&socket),
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
