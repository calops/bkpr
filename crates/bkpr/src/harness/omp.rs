//! Launch-local Omp MCP, guidance and native binding/notification extension.
use std::ffi::OsString;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use bkpr_protocol::HarnessKind;
use tempfile::TempDir;

use super::{
    Capture,
    assets::{private_directory, write_credentials, write_private},
    passthrough, run_managed, scrub,
};

const EXTENSION: &str = include_str!("../../../../integration/omp/index.ts");

fn interactive_arguments(arguments: &[OsString]) -> Option<super::install::Invocation> {
    let mut index = 0;
    let mut first_positional = true;
    let mut invocation = super::install::Invocation::default();
    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            return Some(invocation);
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
            | "--trusted-extension" => return None,
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
                let value = if let Some((_, value)) = argument.split_once('=') {
                    std::ffi::OsStr::new(value)
                } else {
                    index += 1;
                    arguments.get(index)?.as_os_str()
                };
                match option {
                    "--profile" => invocation.profile = Some(value.to_owned()),
                    "--cwd" => invocation.cwd = Some(PathBuf::from(value)),
                    "--config" => invocation.config_files.push(PathBuf::from(value)),
                    _ => {}
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
            _ if option.starts_with('-') => return None,
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
                    return None;
                }
                first_positional = false;
            }
            _ => {}
        }
        index += 1;
    }
    Some(invocation)
}

fn extension_package(capture: &Capture, installed: bool) -> Result<TempDir> {
    let directory =
        private_directory("bkpr-omp-").context("create launch-local Omp capture package")?;
    let extensions = if installed {
        Vec::<PathBuf>::new()
    } else {
        vec![PathBuf::from("omp/index.ts")]
    };
    write_private(
        &directory.path().join("package.json"),
        &serde_json::to_vec(&serde_json::json!({
            "name":"bkpr-native-capture", "private":true, "type":"module",
            "omp":{"extensions":extensions}
        }))?,
    )?;
    write_credentials(capture, &directory.path().join("capture.json"))?;
    if !installed {
        fs::create_dir(directory.path().join("omp"))?;
        fs::create_dir(directory.path().join("shared"))?;
        write_private(&directory.path().join("omp/index.ts"), EXTENSION.as_bytes())?;
        write_private(
            &directory.path().join("shared/native.ts"),
            include_bytes!("../../../../integration/shared/native.ts"),
        )?;
        // Installed launches use the sole user MCP entry instead.
        let config = serde_json::json!({"mcpServers":{"bkpr":{
            "type":"stdio",
            "command":capture.program,
            "args":["capture","mcp","--harness","omp"],
            "env":{
                "BKPR_CAPTURE_LAUNCH_ID":capture.grant.launch_id,
                "BKPR_CAPTURE_TOKEN":capture.grant.token,
                "BKPR_CAPTURE_HARNESS":"omp",
                "BKPR_CAPTURE_PROGRAM":capture.program,
                "BKPR_SOCKET":capture.socket,
                "BKPR_PANE_ID":capture.grant.pane_id
            }
        }}});
        write_private(
            &directory.path().join("mcp.json"),
            &serde_json::to_vec(&config)?,
        )?;
    }
    // Native --no-skills/explicit skill selection governs package discovery too.
    let skill = directory.path().join("skills/bkpr-control");
    fs::create_dir_all(&skill)?;
    write_private(
        &skill.join("SKILL.md"),
        include_bytes!("../../../../integration/omp/SKILL.md"),
    )?;
    Ok(directory)
}

/// Run supported interactive native launches with scoped capture; preserve native utility modes and exit status.
pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return passthrough(&executable, arguments);
    }
    let Some(invocation) = interactive_arguments(&arguments) else {
        eprintln!(
            "bkpr: Omp invocation is unchanged; launch-local capture is not enabled for this argument mode"
        );
        return passthrough(&executable, arguments);
    };
    let installed = super::install::runtime(HarnessKind::Omp, invocation)?;
    let capture = Capture::begin(HarnessKind::Omp)?;
    let package = extension_package(&capture, installed.is_some())?;
    let mut command = Command::new(executable);
    scrub(&mut command);
    command.env("BKPR_INTEGRATION_FILE", package.path().join("capture.json"));
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
        assert!(interactive_arguments(&args(&["--no-extensions", "prompt"])).is_none());
        assert!(interactive_arguments(&args(&["--trusted-extension=/tmp/ext.ts"])).is_none());
        assert!(interactive_arguments(&args(&["--trusted-extension", "/tmp/ext.ts"])).is_none());
        assert!(
            interactive_arguments(&args(&["--system-prompt", "--no-extensions", "prompt"]))
                .is_some()
        );
        assert!(interactive_arguments(&args(&["--", "--trusted-extension"])).is_some());
        assert!(interactive_arguments(&args(&["--no-skills", "prompt"])).is_some());
        assert!(interactive_arguments(&args(&["--future-option", "prompt"])).is_none());
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
            assert!(interactive_arguments(&values).is_some());
        }
        assert!(interactive_arguments(&args(&["--mode", "rpc"])).is_none());
        assert!(interactive_arguments(&args(&["--print", "prompt"])).is_none());
        assert!(interactive_arguments(&args(&["models"])).is_none());
    }
}
