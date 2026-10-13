mod agent;
mod backend;
mod ci;
mod claude;
mod herdr;
mod launcher;
mod project;
mod project_watch;
mod transport;

use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use shephrd::{Action, Effect, MAX_MESSAGE_BYTES, Request};

#[derive(Debug, Parser)]
#[command(
    name = "shephrd",
    version,
    disable_help_subcommand = true,
    override_usage = "shephrd [OPTIONS]",
    about = "Persistent terminal workspaces with an agent-aware sidebar",
    after_help = "Examples:\n  shephrd                         Open or attach to the default workspace session\n  shephrd --session work          Open or attach to a named session\n  shephrd --remote user@host      Connect to a remote Herdr session over SSH\n  shephrd --machine laptop        Connect using a saved Herdr machine"
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
    /// Discover a local directory's project name without starting a backend.
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
    /// Run the remote Herdr companion (normally started by the launcher).
    #[command(hide = true)]
    Backend {
        #[arg(long)]
        config: PathBuf,
    },
    /// Issue a versioned semantic request to a running remote companion.
    #[command(hide = true)]
    Request {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        json: String,
    },
    /// Attach a real native terminal to a persistent Herdr pane.
    #[command(hide = true)]
    Attach {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        pane: String,
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
        Some(Commands::Backend { config }) => {
            let bytes =
                std::fs::read(&config).with_context(|| format!("read {}", config.display()))?;
            let config: backend::Config =
                serde_json::from_slice(&bytes).context("invalid backend configuration")?;
            backend::run(config)
        }
        Some(Commands::Request { socket, json }) => {
            if json.len() > MAX_MESSAGE_BYTES {
                bail!("request exceeds {MAX_MESSAGE_BYTES} bytes");
            }
            let request: Request = serde_json::from_str(&json).context("invalid request JSON")?;
            let response = transport::request(&socket, &request)?;
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, &response)?;
            writeln!(stdout)?;
            stdout.flush()?;
            if let Some(error) = response.error {
                bail!("{error}");
            }
            Ok(())
        }
        Some(Commands::Attach { socket, pane }) => {
            let response = transport::request(
                &socket,
                &Request::new(1, Action::ResolveAttachment { pane_id: pane }),
            )?;
            if let Some(error) = response.error {
                bail!("{error}");
            }
            let command = response
                .effects
                .into_iter()
                .find_map(|effect| match effect {
                    Effect::Attachment { command } => Some(command),
                    _ => None,
                })
                .context("companion did not return an attachment command")?;
            let mut child = Command::new(&command.executable);
            child.args(&command.args);
            if let Some(cwd) = command.cwd {
                child.current_dir(cwd);
            }
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("HERDR_") {
                    child.env_remove(key);
                }
            }
            child.envs(&command.env);
            Err(child.exec()).with_context(|| format!("execute attachment {}", command.executable))
        }
    }
}
