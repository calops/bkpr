mod backend;
mod herdr;
mod launcher;
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
    about = "Workspace-first Herdr composition in Zellij"
)]
struct Cli {
    #[command(flatten)]
    launch: launcher::Options,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the native companion (normally started by the launcher).
    #[command(hide = true)]
    Backend {
        #[arg(long)]
        config: PathBuf,
    },
    /// Issue a versioned semantic request to a running companion.
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
