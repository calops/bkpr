mod agent;
mod agent_relay;
mod ci;
mod claude;
mod codex;
mod context;
mod harness;
mod launcher;
mod omp;
mod project;
#[cfg(target_os = "linux")]
mod project_cache;
mod project_watch;
mod recovery;
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
    /// Run a supported harness with launch-local Bkpr integration.
    Run {
        #[arg(value_enum)]
        harness: harness::Harness,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<std::ffi::OsString>,
    },
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
    // LEGACY_COMPAT(next-major): existing sessions still invoke codex-hook.
    #[command(hide = true)]
    CodexHook,
    #[command(hide = true)]
    CodexStop {
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Apply a recovery update from the native-owner plugin.
    #[command(hide = true)]
    SessionRecord {
        #[arg(long)]
        json: String,
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
    /// Bind the launch's agent to its current native harness session ID.
    #[command(hide = true)]
    RelayBindSession {
        #[arg(long)]
        session_id: String,
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
        // The owner plugin treats 3 as "recovery not claimed": stay silent.
        if error.downcast_ref::<recovery::Unclaimed>().is_some() {
            std::process::exit(3);
        }
        eprintln!("bkpr: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => launcher::run(cli.launch),
        Some(Commands::Run { harness, arguments }) => harness.launch(arguments),
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
        }) => claude::launch(Some(&wrapper_dir), arguments),
        Some(Commands::ClaudeTitle) => claude::prompt_title(),
        Some(Commands::ClaudeWait) => claude::wait_message(),
        Some(Commands::OmpLaunch {
            wrapper_dir,
            arguments,
        }) => omp::launch(Some(&wrapper_dir), arguments),
        Some(Commands::CodexLaunch {
            wrapper_dir,
            arguments,
        }) => codex::launch(Some(&wrapper_dir), arguments),
        // LEGACY_COMPAT(next-major): remove with the old SessionStart adapter.
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
        Some(Commands::RelayBindSession { session_id }) => relay::bind_session(&session_id),
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
        Some(Commands::SessionRecord { json }) => recovery::record(&json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn explicit_run_preserves_argument_boundaries() {
        let cli = Cli::try_parse_from([
            "bkpr",
            "run",
            "claude",
            "--resume",
            "two words",
            "--",
            "--literal",
        ])
        .unwrap();
        let Some(Commands::Run { harness, arguments }) = cli.command else {
            panic!("expected explicit harness run");
        };
        assert_eq!(harness, harness::Harness::Claude);
        assert_eq!(
            arguments,
            ["--resume", "two words", "--", "--literal"].map(OsString::from)
        );
    }

    #[test]
    fn explicit_run_consumes_only_the_initial_argument_delimiter() {
        let cli = Cli::try_parse_from(["bkpr", "run", "goose", "--", "--flag", "two words", "--"])
            .unwrap();
        let Some(Commands::Run { arguments, .. }) = cli.command else {
            panic!("expected explicit harness run");
        };
        assert_eq!(arguments, ["--flag", "two words", "--"].map(OsString::from));
    }

    #[test]
    fn explicit_run_rejects_unknown_and_missing_harnesses() {
        let Err(error) = Cli::try_parse_from(["bkpr", "run", "sh", "-c", "exit 0"]) else {
            panic!("unknown harness was accepted");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        assert!(Cli::try_parse_from(["bkpr", "run"]).is_err());
    }

    #[test]
    fn explicit_run_accepts_recognized_aliases() {
        for (name, expected) in [
            ("claude-code", harness::Harness::Claude),
            ("antigravity", harness::Harness::Antigravity),
            ("cursor-agent", harness::Harness::Cursor),
            ("copilot-cli", harness::Harness::Copilot),
            ("github-copilot", harness::Harness::Copilot),
            (".cline", harness::Harness::Cline),
        ] {
            let cli = Cli::try_parse_from(["bkpr", "run", name]).unwrap();
            let Some(Commands::Run { harness, arguments }) = cli.command else {
                panic!("expected explicit harness run");
            };
            assert_eq!(harness, expected);
            assert!(arguments.is_empty());
        }
    }

    #[test]
    fn explicit_run_preserves_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let argument = OsString::from_vec(vec![b'a', 0xff, b'b']);
        let cli = Cli::try_parse_from([
            OsString::from("bkpr"),
            OsString::from("run"),
            OsString::from("goose"),
            argument.clone(),
        ])
        .unwrap();
        let Some(Commands::Run { arguments, .. }) = cli.command else {
            panic!("expected explicit harness run");
        };
        assert_eq!(arguments, [argument]);
    }
}
