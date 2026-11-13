//! Launch-local Pi extension, shared MCP bootstrap and native communication guidance.
use super::{Capture, agent, assets, passthrough, run_managed, scrub};
use anyhow::Result;
use bkpr_protocol::HarnessKind;
use std::{ffi::OsString, fs, io::IsTerminal, path::PathBuf, process::Command};

fn interactive_arguments(arguments: &[OsString]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let Some(argument) = argument.to_str() else {
            return false;
        };
        if argument == "--" {
            return true;
        }
        match argument {
            "--help" | "-h" | "--version" | "-v" | "--print" | "-p" | "--export"
            | "--list-models" | "--no-session" | "--no-mcp" | "--no-extensions" | "-ne" => {
                return false;
            }
            "--mode" => {
                index += 1;
                let mode = arguments.get(index).and_then(|value| value.to_str());
                if mode != Some("text") {
                    return false;
                }
            }
            "--provider"
            | "--model"
            | "--api-key"
            | "--thinking"
            | "--models"
            | "--session"
            | "--session-id"
            | "--session-dir"
            | "--fork"
            | "--name"
            | "-n"
            | "--tools"
            | "-t"
            | "--exclude-tools"
            | "-xt"
            | "--extension"
            | "-e"
            | "--skill"
            | "--prompt-template"
            | "--theme"
            | "--use-theme"
            | "--system-prompt"
            | "--append-system-prompt"
            | "--tui-mode" => {
                index += 1;
                if index >= arguments.len() {
                    return false;
                }
            }
            "--continue"
            | "-c"
            | "--resume"
            | "-r"
            | "--no-builtin-tools"
            | "-nbt"
            | "--no-tools"
            | "-nt"
            | "--no-skills"
            | "-ns"
            | "--no-prompt-templates"
            | "-np"
            | "--no-themes"
            | "--no-context-files"
            | "-nc"
            | "--verbose"
            | "--approve"
            | "-a"
            | "--no-approve"
            | "-na"
            | "--offline" => {}
            value if !value.starts_with('-') => {
                if index == 0
                    && matches!(
                        value,
                        "install"
                            | "remove"
                            | "uninstall"
                            | "update"
                            | "list"
                            | "config"
                            | "auth"
                            | "mcp"
                    )
                {
                    return false;
                }
            }
            _ => return false,
        }
        index += 1;
    }
    true
}

/// Launch an interactive Pi TUI with a private native extension and shared MCP.
/// Run supported interactive native launches with scoped capture; preserve native utility modes and exit status.
pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !interactive_arguments(&arguments)
    {
        return passthrough(&executable, arguments);
    }
    let installed = super::install::runtime(HarnessKind::Pi, Default::default())?;
    let capture = Capture::begin(HarnessKind::Pi)?;
    let package = assets::private_directory("bkpr-pi-")?;
    let directory = package.path();
    let extension = if installed.is_some() {
        None
    } else {
        fs::create_dir(directory.join("pi"))?;
        fs::create_dir(directory.join("shared"))?;
        let extension = directory.join("pi/index.ts");
        assets::write_private(
            &extension,
            include_bytes!("../../../../integration/pi/index.ts"),
        )?;
        assets::write_private(
            &directory.join("shared/native.ts"),
            include_bytes!("../../../../integration/shared/native.ts"),
        )?;
        Some(extension)
    };
    assets::write_credentials(&capture, &directory.join("capture.json"))?;
    let guide = directory.join("bkpr-instructions.md");
    assets::write_private(&guide, agent::INSTRUCTIONS.as_bytes())?;
    let mut command = Command::new(executable);
    scrub(&mut command);
    command.env("BKPR_INTEGRATION_FILE", directory.join("capture.json"));
    if let Some(extension) = extension {
        command.arg("--extension").arg(extension);
    }
    command
        .arg("--append-system-prompt")
        .arg(guide)
        .args(arguments);
    let result = run_managed(&mut command, &capture);
    drop(package);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interactive(arguments: &[&str]) -> bool {
        interactive_arguments(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn utility_protocol_and_disabled_integration_modes_remain_native() {
        for arguments in [
            vec!["--help"],
            vec!["--version"],
            vec!["--print", "prompt"],
            vec!["--mode=json", "prompt"],
            vec!["--mode", "rpc"],
            vec!["--no-session"],
            vec!["--no-mcp"],
            vec!["-ne", "-e", "custom.ts"],
            vec!["--export", "session.jsonl"],
            vec!["mcp", "list"],
            vec!["auth", "login", "provider"],
            vec!["install", "npm:package"],
            vec!["--model"],
            vec!["--unknown-extension-option"],
        ] {
            assert!(!interactive(&arguments), "{arguments:?}");
        }
    }

    #[test]
    fn native_option_values_and_prompt_terminators_do_not_select_another_mode() {
        for arguments in [
            vec!["--", "--print"],
            vec!["--model", "--print"],
            vec!["--append-system-prompt", "--no-mcp"],
            vec!["--mode", "text", "prompt"],
            vec!["--session", "exact-native-id"],
            vec!["--session-id", "custom-session"],
            vec!["--resume"],
            vec!["--fork", "exact-native-id"],
            vec!["--offline", "--no-builtin-tools", "prompt"],
        ] {
            assert!(interactive(&arguments), "{arguments:?}");
        }
    }
}
