//! Launch-local Antigravity identity; never writes AGY settings or installs assets.
use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

pub fn wrapper(program: &Path, directory: &Path) -> Result<String> {
    let program = program
        .to_str()
        .context("AGY wrapper program path is not UTF-8")?;
    let directory = directory
        .to_str()
        .context("AGY wrapper directory is not UTF-8")?;
    Ok(format!(
        "#!/bin/sh\nexec {} agy-launch --wrapper-dir {} -- \"$@\"\n",
        crate::claude::shell_quote(program),
        crate::claude::shell_quote(directory),
    ))
}

// Only recognized interactive routing may register an agent. In particular, option
// values are not commands or switches, and arguments after `--` are prompt text.
pub(crate) fn interactive_arguments(arguments: &[OsString]) -> bool {
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
            "--help"
            | "-h"
            | "--version"
            | "-v"
            | "--print"
            | "-p"
            | "--prompt"
            | "--input-format"
            | "--output-format"
            | "--json-schema"
            | "--print-timeout"
            | "--disable-slash-commands" => return false,
            "--mode"
            | "--model"
            | "--effort"
            | "--agent"
            | "--add-dir"
            | "--conversation"
            | "--prompt-interactive"
            | "-i"
            | "--project"
            | "--log-file" => {
                if !inline {
                    index += 1;
                    if index >= arguments.len() {
                        return false;
                    }
                }
            }
            "--continue"
            | "-c"
            | "--sandbox"
            | "--dangerously-skip-permissions"
            | "--remote-control"
            | "--new-project"
                if !inline => {}
            _ if option.starts_with('-') => return false,
            _ if first_positional => {
                if matches!(
                    argument,
                    "agent"
                        | "agents"
                        | "agentapi"
                        | "changelog"
                        | "install"
                        | "mcp"
                        | "mic-serve"
                        | "models"
                        | "plugin"
                        | "plugins"
                        | "remote-control"
                        | "update"
                        | "help"
                        | "version"
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

fn clear_identity(command: &mut Command) {
    for (name, _) in env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"BKPR_AGY_")
            || name.as_encoded_bytes().starts_with(b"BKPR_RELAY_")
        {
            command.env_remove(name);
        }
    }
}

pub fn launch(directory: Option<&Path>, arguments: Vec<OsString>) -> Result<()> {
    let executable = crate::harness::resolve("agy", directory)
        .or_else(|_| crate::harness::resolve("antigravity", directory))?;
    let mut command = Command::new(&executable);
    clear_identity(&mut command);
    // A nested AGY must obtain its own native command capability, never its parent's.
    command
        .env_remove("ANTIGRAVITY_LS_ADDRESS")
        .env_remove("ANTIGRAVITY_CSRF_TOKEN")
        .env_remove("ANTIGRAVITY_CONVERSATION_ID");
    let installed = crate::install::installed_launch("agy", &arguments)?;
    if !installed
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && interactive_arguments(&arguments)
    {
        let cwd = env::current_dir().context("locate AGY working directory")?;
        let relay = crate::relay::prepare("agy", &cwd)?;
        relay.apply(&mut command);
    }
    // No temporary assets need a waiting owner: exec retains the registered PID
    // for precisely the lifetime of AGY, including signals and its exit status.
    Err(command.args(arguments).exec()).context("execute AGY")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interactive(arguments: &[&str]) -> bool {
        interactive_arguments(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn interactive_routes_and_exact_conversation_resume() {
        assert!(interactive(&[]));
        assert!(interactive(&["--conversation", "captured-root-id"]));
        assert!(interactive(&[
            "--conversation=captured-root-id",
            "--continue"
        ]));
        assert!(interactive(&[
            "-i",
            "initial prompt",
            "--model",
            "model-id"
        ]));
        assert!(interactive(&["--model", "--print"]));
        assert!(interactive(&["--", "--print", "mcp"]));
        assert!(!interactive(&["--conversation"]));
        assert!(!interactive(&["--unknown-option"]));
    }

    #[test]
    fn administrative_and_noninteractive_routes_never_register() {
        for command in [
            "agent",
            "agents",
            "agentapi",
            "changelog",
            "install",
            "mcp",
            "mic-serve",
            "models",
            "plugin",
            "plugins",
            "remote-control",
            "update",
            "help",
            "version",
        ] {
            assert!(!interactive(&[command]), "{command}");
            assert!(!interactive(&["--model", "model-id", command]), "{command}");
        }
        for option in [
            "--help",
            "-h",
            "--version",
            "-v",
            "--print",
            "-p",
            "--prompt",
            "--input-format=stream-json",
            "--input-format",
            "--output-format=json",
        ] {
            assert!(!interactive(&[option]), "{option}");
        }
        assert!(!interactive(&[
            "--prompt-interactive",
            "initial prompt",
            "--print"
        ]));
    }
}
