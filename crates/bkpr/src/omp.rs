//! Launch-local OMP extension and MCP registration; never writes OMP settings.
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tempfile::TempDir;

use crate::relay::{self, RelayLaunch};

const EXTENSION: &str = include_str!("../../../integration/omp/index.ts");

pub fn wrapper(program: &Path, directory: &Path) -> Result<String> {
    let program = program
        .to_str()
        .context("OMP wrapper program path is not UTF-8")?;
    let directory = directory
        .to_str()
        .context("OMP wrapper directory is not UTF-8")?;
    Ok(format!(
        "#!/bin/sh\nexec {} omp-launch --wrapper-dir {} -- \"$@\"\n",
        crate::claude::shell_quote(program),
        crate::claude::shell_quote(directory),
    ))
}

fn check_arguments(arguments: &[OsString]) -> Result<()> {
    for argument in arguments {
        if argument == "--" {
            break;
        }
        if let Some(argument) = argument.to_str()
            && (argument == "--trusted-extension" || argument.starts_with("--trusted-extension="))
        {
            bail!(
                "Bkpr OMP communication requires a launch-local extension package (-e), \
                 which OMP cannot combine with --trusted-extension"
            );
        }
    }
    Ok(())
}

fn skill_arguments(arguments: &[OsString]) -> bool {
    let mut index = 0;
    let mut first_positional = true;
    while index < arguments.len() {
        let Some(argument) = arguments[index].to_str() else {
            return false;
        };
        if argument == "--" {
            return true;
        }
        let (option, inline) = argument
            .split_once('=')
            .map_or((argument, false), |(option, _)| (option, true));
        match option {
            "--print" | "-p" | "--mode" | "--help" | "-h" | "--version" | "-v" | "--export"
            | "--alias" => return false,
            "--cwd"
            | "--add-dir"
            | "--profile"
            | "--config"
            | "--session-dir"
            | "--fork"
            | "--model"
            | "--smol"
            | "--slow"
            | "--plan"
            | "--models"
            | "--provider"
            | "--api-key"
            | "--provider-session-id"
            | "--prompt-cache-key"
            | "--service-tier"
            | "--thinking"
            | "--prewalk-into"
            | "--plan-yolo-into"
            | "--goal"
            | "--tools"
            | "--approval-mode"
            | "--max-time"
            | "--extension"
            | "-e"
            | "--hook"
            | "--plugin-dir"
            | "--skills"
            | "--system-prompt"
            | "--system-prompt-template"
            | "--append-system-prompt" => {
                if !inline {
                    index += 1;
                    if index >= arguments.len() {
                        return false;
                    }
                }
            }
            "--resume" | "-r" | "--session" | "--restore" => {
                if !inline
                    && arguments
                        .get(index + 1)
                        .is_some_and(|next| !next.to_string_lossy().starts_with('-'))
                {
                    index += 1;
                }
            }
            "--continue"
            | "-c"
            | "--allow-home"
            | "--no-session"
            | "--from-claude"
            | "--from-codex"
            | "--no-title"
            | "--hide-thinking"
            | "--print-thoughts"
            | "--external-thinking"
            | "--prewalk"
            | "--no-prewalk"
            | "--plan-yolo"
            | "--no-tools"
            | "--no-lsp"
            | "--no-pty"
            | "--auto-approve"
            | "--yolo"
            | "--advisor"
            | "--no-extensions"
            | "--no-skills"
            | "--no-rules"
                if !inline => {}
            _ if option.starts_with('-') => return false,
            "launch" if first_positional => first_positional = false,
            _ if first_positional => {
                // A single known prompt position is safe; routing commands must
                // not acquire a skill just because they run in a terminal.
                if matches!(
                    argument,
                    "acp"
                        | "auth-broker"
                        | "auth-gateway"
                        | "agents"
                        | "bench"
                        | "browser-relay"
                        | "cleanse"
                        | "collab"
                        | "clip"
                        | "commit"
                        | "completions"
                        | "compress"
                        | "config"
                        | "dry-balance"
                        | "find"
                        | "gc"
                        | "grep"
                        | "gallery"
                        | "git"
                        | "grievances"
                        | "if-bench"
                        | "images"
                        | "img"
                        | "install"
                        | "join"
                        | "login"
                        | "models"
                        | "plugin"
                        | "plugins"
                        | "play"
                        | "predict"
                        | "ps"
                        | "say"
                        | "share"
                        | "setup"
                        | "shell"
                        | "read"
                        | "render"
                        | "skill"
                        | "skills"
                        | "ssh"
                        | "stats"
                        | "stream"
                        | "update"
                        | "usage"
                        | "tiny-models"
                        | "token"
                        | "toks"
                        | "ttsr"
                        | "worktree"
                        | "wt"
                        | "search"
                        | "q"
                        | "web-search"
                        | "marketplace"
                        | "uninstall"
                        | "extensions"
                ) {
                    return false;
                }
                first_positional = false;
            }
            _ => {}
        }
        index += 1;
    }
    true
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create OMP launch file {}", path.display()))?
        .write_all(contents)
        .with_context(|| format!("write OMP launch file {}", path.display()))
}

fn extension_package(
    runtime: &Path,
    program: &Path,
    cwd: &Path,
    relay: &RelayLaunch,
    include_skill: bool,
) -> Result<TempDir> {
    // Keep credentials in the session's private runtime, never in OMP config.
    let directory = tempfile::Builder::new()
        .prefix("omp-")
        .tempdir_in(runtime)
        .context("create launch-local OMP extension package")?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let server_name = format!("bkpr-{}", relay.agent_id);
    let manifest = json!({
        "name": server_name,
        "private": true,
        "type": "module",
        "omp": {"extensions": ["index.ts"]},
    });
    let config = json!({
        "mcpServers": {
            (server_name): {
                "type": "stdio",
                "command": program,
                "args": ["relay-mcp"],
                "cwd": cwd,
                "env": {
                    "BKPR_RELAY_AGENT_ID": relay.agent_id,
                    "BKPR_RELAY_TOKEN": relay.token,
                    "BKPR_RELAY_SOCKET": relay.socket,
                    "BKPR_RELAY_HARNESS": relay.harness,
                },
            },
        },
    });
    write_private(
        &directory.path().join("package.json"),
        &serde_json::to_vec(&manifest)?,
    )?;
    write_private(&directory.path().join("index.ts"), EXTENSION.as_bytes())?;
    write_private(
        &directory.path().join("mcp.json"),
        &serde_json::to_vec(&config)?,
    )?;
    if include_skill {
        let skill = directory.path().join("skills/bkpr-control");
        fs::create_dir_all(skill.join("references"))?;
        write_private(&skill.join("SKILL.md"), relay::CONTROL_SKILL.as_bytes())?;
        write_private(
            &skill.join("references/control.md"),
            relay::CONTROL_REFERENCE.as_bytes(),
        )?;
    }
    Ok(directory)
}

pub fn launch(directory: Option<&Path>, arguments: Vec<OsString>) -> Result<()> {
    check_arguments(&arguments)?;
    let executable = crate::harness::resolve("omp", directory)?;
    let cwd = env::current_dir().context("locate OMP working directory")?;
    let program = env::current_exe().context("locate Bkpr MCP executable")?;
    let relay = relay::prepare("omp", &cwd)?;
    let runtime = match directory {
        Some(directory) => directory
            .parent()
            .context("locate session runtime")?
            .to_path_buf(),
        None => crate::launcher::runtime_root()?,
    };
    let include_skill =
        io::stdin().is_terminal() && io::stdout().is_terminal() && skill_arguments(&arguments);
    let package = extension_package(&runtime, &program, &cwd, &relay, include_skill)?;
    let mut command = Command::new(executable);
    relay.apply(&mut command);
    command
        .env("BKPR_OMP_SERVER", format!("bkpr-{}", relay.agent_id))
        .env("BKPR_OMP_PROGRAM", &program)
        .arg("-e")
        .arg(package.path())
        .args(arguments);
    // Both processes share the foreground terminal group. OMP owns Ctrl-C;
    // the waiting wrapper must not exit while OMP handles an interrupted turn.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    // Keep the registration's process alive while OMP runs, and remove its
    // credential-bearing package on normal exit instead of leaking temp files.
    let status = command.status().context("execute OMP");
    drop(package);
    let status = status?;
    if !status.success() {
        std::process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn trusted_extension_conflict_does_not_consume_prompt() {
        assert!(check_arguments(&["--trusted-extension=/tmp/ext.ts".into()]).is_err());
        assert!(check_arguments(&["--trusted-extension".into(), "/tmp/ext.ts".into()]).is_err());
        assert!(check_arguments(&["--".into(), "--trusted-extension".into()]).is_ok());
        assert!(check_arguments(&["--no-extensions".into(), "a prompt".into()]).is_ok());
    }

    #[test]
    fn skill_loading_respects_argument_values_and_interactive_routing() {
        assert!(skill_arguments(&[]));
        assert!(skill_arguments(&[
            "--model".into(),
            "--print".into(),
            "prompt".into()
        ]));
        assert!(skill_arguments(&[
            "--".into(),
            "--mode".into(),
            "models".into()
        ]));
        assert!(skill_arguments(&["launch".into(), "models".into()]));
        assert!(!skill_arguments(&[
            "--model".into(),
            "opus".into(),
            "models".into()
        ]));
        assert!(!skill_arguments(&["--mode=text".into(), "prompt".into()]));
        assert!(!skill_arguments(&["-p".into(), "prompt".into()]));
        assert!(!skill_arguments(&[
            "--export".into(),
            "conversation".into()
        ]));
    }

    #[test]
    fn temporary_credentials_are_private_and_removed_after_launch() {
        let runtime = tempfile::tempdir().unwrap();
        let relay = RelayLaunch {
            agent_id: "agent-test".into(),
            token: "secret-test".into(),
            socket: PathBuf::from("/run/bkpr-relay/socket"),
            harness: "omp".into(),
        };
        let package = extension_package(
            runtime.path(),
            Path::new("/opt/bkpr"),
            Path::new("/work"),
            &relay,
            false,
        )
        .unwrap();
        let package_path = package.path().to_path_buf();
        assert_eq!(
            fs::metadata(&package_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["index.ts", "mcp.json", "package.json"] {
            assert_eq!(
                fs::metadata(package_path.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(package);
        assert!(!package_path.exists());
    }
}
