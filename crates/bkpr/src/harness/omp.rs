//! A private, launch-local Omp extension. No persistent settings or MCP servers.
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use bkpr_protocol::HarnessKind;
use tempfile::TempDir;

use super::{Capture, passthrough, run_managed, scrub};

const EXTENSION: &str = include_str!("../../../../integration/omp/index.ts");

fn interactive_arguments(arguments: &[OsString]) -> bool {
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
            "--print"
            | "-p"
            | "--mode"
            | "--help"
            | "-h"
            | "--version"
            | "-v"
            | "--export"
            | "--alias"
            | "--no-extensions"
            | "--trusted-extension" => return false,
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
                        .is_some_and(|next| !next.as_encoded_bytes().starts_with(b"-"))
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
            | "--no-skills"
            | "--no-rules"
            | "--no-context-files"
                if !inline => {}
            _ if option.starts_with('-') => return false,
            "launch" if first_positional => first_positional = false,
            _ if first_positional => {
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
        .context("create Omp capture package asset")?
        .write_all(contents)
        .context("write Omp capture package asset")
}

fn extension_package() -> Result<TempDir> {
    let directory = tempfile::Builder::new()
        .prefix("bkpr-omp-")
        .tempdir()
        .context("create launch-local Omp capture package")?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    write_private(&directory.path().join("package.json"),
        br#"{"name":"bkpr-native-capture","private":true,"type":"module","omp":{"extensions":["index.ts"]}}"#)?;
    write_private(&directory.path().join("index.ts"), EXTENSION.as_bytes())?;
    write_private(
        &directory.path().join("tools.json"),
        &serde_json::to_vec(&super::agent::tools())?,
    )?;
    Ok(directory)
}

pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return passthrough(&executable, arguments);
    }
    if !interactive_arguments(&arguments) {
        eprintln!(
            "bkpr: Omp invocation is unchanged; launch-local capture is not enabled for this argument mode"
        );
        return passthrough(&executable, arguments);
    }
    let package = extension_package()?;
    let capture = Capture::begin(HarnessKind::Omp)?;
    let mut command = Command::new(executable);
    scrub(&mut command);
    capture.apply(&mut command);
    command.arg("-e").arg(package.path()).args(arguments);
    // Keep the native loader's package alive through the child's entire life,
    // including error unwinding. run_managed owns launch revocation and signals.
    let result = run_managed(&mut command, &capture);
    drop(package);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn optouts_are_only_options_outside_consumed_values_and_literal_prompts() {
        assert!(!interactive_arguments(&args(&[
            "--no-extensions",
            "prompt"
        ])));
        assert!(!interactive_arguments(&args(&[
            "--trusted-extension=/tmp/ext.ts"
        ])));
        assert!(!interactive_arguments(&args(&[
            "--trusted-extension",
            "/tmp/ext.ts"
        ])));
        assert!(interactive_arguments(&args(&[
            "--system-prompt",
            "--no-extensions",
            "prompt"
        ])));
        assert!(interactive_arguments(&args(&["--", "--trusted-extension"])));
        assert!(interactive_arguments(&args(&["--no-skills", "prompt"])));
        assert!(!interactive_arguments(&args(&[
            "--future-option",
            "prompt"
        ])));
    }

    #[test]
    fn interactive_conversation_modes_are_distinct_from_rpc_print_and_subcommands() {
        for values in [
            args(&["--profile", "work", "--resume", "native-id"]),
            args(&["--continue"]),
            args(&["--session=native-id"]),
            args(&["--restore", "native-id"]),
            args(&["--fork", "native-id"]),
            args(&["launch", "models"]),
        ] {
            assert!(interactive_arguments(&values));
        }
        assert!(!interactive_arguments(&args(&["--mode", "rpc"])));
        assert!(!interactive_arguments(&args(&["--print", "prompt"])));
        assert!(!interactive_arguments(&args(&["models"])));
    }
}
