//! Native user-scoped Claude MCP and command-hook registration.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::HarnessKind;
use serde_json::{Value, json};

use super::types::{Asset, Bundle, Definition, Entry, Format, Paths, read_native, shell_command};

const CALLBACK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
];
const WAIT_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop"];

fn managed_path(name: &str) -> PathBuf {
    Path::new(if cfg!(target_os = "macos") {
        "/Library/Application Support/ClaudeCode"
    } else {
        "/etc/claude-code"
    })
    .join(name)
}

fn managed_settings(paths: &Paths) -> Result<PathBuf> {
    paths.env_path(
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
        managed_path("managed-settings.json"),
    )
}

fn native_paths(paths: &Paths) -> Result<(PathBuf, PathBuf)> {
    let override_directory = paths
        .environment
        .get(std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"))
        .filter(|value| !value.is_empty());
    let directory = if override_directory.is_some() {
        paths.env_path("CLAUDE_CONFIG_DIR", paths.home.join(".claude"))?
    } else {
        paths.home.join(".claude")
    };
    let suffix = if paths
        .environment
        .get(std::ffi::OsStr::new("CLAUDE_CODE_CUSTOM_OAUTH_URL"))
        .is_some_and(|value| !value.is_empty())
    {
        "-custom-oauth"
    } else {
        ""
    };
    let current = if override_directory.is_some() {
        directory.join(format!(".claude{suffix}.json"))
    } else {
        paths.home.join(format!(".claude{suffix}.json"))
    };
    // Claude 2.1.322 still selects an existing writable or populated legacy file.
    let legacy = directory.join(".config.json");
    let global = match fs::metadata(&legacy) {
        Ok(metadata)
            if metadata.is_file()
                && (metadata.len() != 0 || metadata.permissions().mode() & 0o222 != 0) =>
        {
            legacy
        }
        Ok(_) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => current,
        Err(error) => return Err(error).context("inspect Claude legacy configuration"),
    };
    Ok((directory.join("settings.json"), global))
}

fn read_settings(path: &Path) -> Result<Option<Value>> {
    let Some(bytes) = read_native(path)? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid Claude settings JSON at {}", path.display()))?;
    ensure!(value.is_object(), "Claude settings must be a JSON object");
    Ok(Some(value))
}

fn check_policy<'a>(settings: impl IntoIterator<Item = &'a Value>, command: &Value) -> Result<()> {
    let mut allowed = Vec::new();
    let mut has_allowlist = false;
    for value in settings {
        for key in ["disableAllHooks", "allowManagedHooksOnly"] {
            ensure!(
                value.get(key).and_then(Value::as_bool) != Some(true),
                "Claude {key} prevents installing native Bkpr hooks"
            );
        }
        if let Some(restriction) = value.get("strictPluginOnlyCustomization") {
            let blocked = restriction.as_bool() == Some(true)
                || restriction.as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| matches!(item.as_str(), Some("hooks" | "mcp")))
                });
            ensure!(!blocked, "Claude policy permits plugin customization only");
        }
        if value
            .get("deniedMcpServers")
            .and_then(Value::as_array)
            .is_some_and(|entries| entries.iter().any(|entry| matches_server(entry, command)))
        {
            bail!("Claude MCP policy denies the Bkpr server");
        }
        if let Some(entries) = value.get("allowedMcpServers") {
            has_allowlist = true;
            allowed.extend(
                entries
                    .as_array()
                    .context("Claude allowedMcpServers must be an array")?
                    .iter(),
            );
        }
        ensure!(
            value
                .get("allowManagedMcpServersOnly")
                .and_then(Value::as_bool)
                != Some(true),
            "Claude managed-only MCP policy requires administrator review before Bkpr installation"
        );
    }
    if has_allowlist {
        let commands_only = allowed
            .iter()
            .any(|entry| entry.get("serverCommand").is_some());
        ensure!(
            allowed.iter().any(|entry| {
                if commands_only {
                    entry.get("serverCommand") == Some(command)
                } else {
                    matches_server(entry, command)
                }
            }),
            "Claude MCP allowlist does not allow the installed Bkpr command"
        );
    }
    Ok(())
}

fn matches_server(entry: &Value, command: &Value) -> bool {
    entry.get("serverName").and_then(Value::as_str) == Some("bkpr")
        || entry.get("serverCommand") == Some(command)
}

/// Build the exact MCP command shared by durable installation and the launch pin.
pub(in crate::harness) fn registration(
    program: &Path,
    installation: Option<&Path>,
) -> Result<Value> {
    let program = program
        .to_str()
        .context("Claude MCP executable path is not UTF-8")?;
    let mut args = vec!["capture"];
    if let Some(installation) = installation {
        args.extend([
            "installed-mcp",
            "--harness",
            "claude",
            "--installation",
            installation
                .to_str()
                .context("Claude installation record path is not UTF-8")?,
        ]);
    } else {
        args.extend(["mcp", "--harness", "claude"]);
    }
    Ok(json!({"type":"stdio", "command":program, "args":args}))
}

fn command(registration: &Value) -> Value {
    let mut command = vec![registration["command"].clone()];
    command.extend(registration["args"].as_array().unwrap().iter().cloned());
    Value::Array(command)
}

fn has_bkpr_server(value: &Value) -> bool {
    value["mcpServers"].get("bkpr").is_some() || value["managedMcpServers"].get("bkpr").is_some()
}

fn approval_opt_out(settings: &Value) -> bool {
    settings.get("agent").is_some()
        || settings["allowManagedPermissionRulesOnly"].as_bool() == Some(true)
        || settings["permissions"]["denyInsteadOfPrompting"].as_bool() == Some(true)
        || matches!(
            settings["permissions"]["defaultMode"].as_str(),
            Some("dontAsk" | "plan")
        )
}

fn server_disabled(settings: &Value) -> bool {
    ["disabledMcpServers", "disabledMcpjsonServers"]
        .into_iter()
        .any(|key| {
            settings[key]
                .as_array()
                .is_some_and(|servers| servers.iter().any(|server| server.as_str() == Some("bkpr")))
        })
}

/// Whether a verified managed launch can safely add its own MCP registration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::harness) enum LaunchMcp {
    /// Keep native MCP source selection and approvals unchanged.
    #[default]
    Native,
    /// The Bkpr registration is safe, but native approval choices are explicit.
    Register,
    /// The exact registration can be pinned with launch-local tool defaults.
    Approve,
}

/// Determine whether a launch can pin Bkpr without adopting a competing
/// registration or opting out of native approval and MCP policy.
pub(super) fn launch_mcp(
    paths: &Paths,
    settings: &Value,
    registration: &Value,
) -> Result<LaunchMcp> {
    let native_command = command(registration);
    if managed_path("managed-mcp.json").try_exists()?
        || has_bkpr_server(settings)
        || server_disabled(settings)
        || native_command
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg.as_str().is_some_and(|arg| arg.contains("${")))
    {
        return Ok(LaunchMcp::Native);
    }
    let mut approve = !approval_opt_out(settings);
    let (settings_file, global_file) = native_paths(paths)?;
    if let Some(global) = read_settings(&global_file)? {
        if server_disabled(&global)
            || global["mcpServers"]
                .get("bkpr")
                .is_some_and(|candidate| candidate != registration)
            || global["projects"]
                .as_object()
                .is_some_and(|projects| projects.values().any(has_bkpr_server))
        {
            return Ok(LaunchMcp::Native);
        }
        approve &= !approval_opt_out(&global);
    }
    let mut policy = Vec::new();
    for file in
        [settings_file, managed_settings(paths)?]
            .into_iter()
            .chain(paths.cwd.ancestors().flat_map(|directory| {
                [
                    directory.join(".mcp.json"),
                    directory.join(".claude/settings.json"),
                    directory.join(".claude/settings.local.json"),
                ]
            }))
    {
        if let Some(value) = read_settings(&file)? {
            if has_bkpr_server(&value) || server_disabled(&value) {
                return Ok(LaunchMcp::Native);
            }
            approve &= !approval_opt_out(&value);
            policy.push(value);
        }
    }
    Ok(
        if check_policy(std::iter::once(settings).chain(&policy), &native_command).is_err() {
            LaunchMcp::Native
        } else if approve {
            LaunchMcp::Approve
        } else {
            LaunchMcp::Register
        },
    )
}

/// Describe credential-free native Claude registration for the current account.
pub(super) fn definition(paths: &Paths) -> Result<Definition> {
    let (settings_file, global_file) = native_paths(paths)?;
    let manifest = paths.manifest(HarnessKind::Claude, &[&settings_file, &global_file]);
    let mcp = registration(&paths.program, Some(&manifest))?;
    let native_command = command(&mcp);
    let installation = manifest
        .to_str()
        .context("Claude installation record path is not UTF-8")?;
    let mut settings = Vec::new();
    for file in [settings_file.clone(), managed_settings(paths)?] {
        if let Some(value) = read_settings(&file)? {
            settings.push(value);
        }
    }
    let mut blockers = Vec::new();
    if let Err(error) = check_policy(&settings, &native_command) {
        blockers.push(error.to_string());
    }
    if managed_path("managed-mcp.json").try_exists()? {
        blockers.push("Claude enterprise MCP configuration has exclusive server ownership".into());
    }
    let mut entries = vec![Entry::value(
        "mcp",
        global_file,
        Format::Json,
        &["mcpServers", "bkpr"],
        mcp.clone(),
    )];
    for (callback, events) in [
        ("claude-hook", CALLBACK_EVENTS),
        ("claude-wait", WAIT_EVENTS),
    ] {
        let command = shell_command(
            &paths.program,
            &[
                "capture",
                "installed-hook",
                "--harness",
                "claude",
                "--callback",
                callback,
                "--installation",
                installation,
            ],
        )?;
        for event in events {
            let mut hook = json!({"type":"command", "command":command, "timeout":5});
            if callback == "claude-wait" {
                hook["asyncRewake"] = json!(true);
                hook["timeout"] = json!(86400);
            }
            entries.push(Entry::array(
                &format!("{event}-{callback}"),
                settings_file.clone(),
                Format::Json,
                &["hooks", event],
                json!({"hooks":[hook]}),
                "installed-hook".into(),
            ));
        }
    }
    let bundle = Bundle::new(
        paths,
        HarnessKind::Claude,
        vec![Asset::text(
            "registration.json",
            serde_json::to_string(&json!({
                "mcp":mcp,
                "hooks":entries.iter().skip(1).map(|entry| {
                    json!({"keys":entry.keys,"value":entry.value})
                }).collect::<Vec<_>>()
            }))?,
        )],
    )?;
    Ok(Definition {
        kind: HarnessKind::Claude,
        bundle,
        entries,
        files: Vec::new(),
        removals: Vec::new(),
        blockers,
        notes: vec![
            "Start a new managed Claude session to load the configured MCP and hooks; existing sessions are unchanged.".into(),
            "Installation does not grant tool approval or workspace trust. Update or uninstall removes unchanged allow rules owned by older Bkpr installs.".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn paths(root: &Path) -> Paths {
        Paths {
            home: root.join("home"),
            data: root.join("data"),
            program: root.join("quoted ' executable/bkpr"),
            cwd: root.to_owned(),
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn redirected_native_account_keeps_legacy_global_configuration() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut paths = paths(root.path());
        paths
            .environment
            .insert("CLAUDE_CONFIG_DIR".into(), "native account".into());
        let directory = root.path().join("native account");
        fs::create_dir(&directory)?;
        let legacy = directory.join(".config.json");
        fs::write(
            &legacy,
            "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}",
        )?;
        assert_eq!(
            native_paths(&paths)?,
            (directory.join("settings.json"), legacy)
        );
        fs::remove_file(directory.join(".config.json"))?;
        assert_eq!(native_paths(&paths)?.1, directory.join(".claude.json"));
        Ok(())
    }

    #[test]
    fn installed_hook_commands_preserve_quoted_program_paths_and_native_stdin() -> Result<()> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let root = tempfile::tempdir()?;
        let mut paths = paths(root.path());
        paths.data = root.path().join("data ' account");
        let (settings_file, global_file) = native_paths(&paths)?;
        let manifest = paths.manifest(HarnessKind::Claude, &[&settings_file, &global_file]);
        fs::create_dir_all(paths.program.parent().unwrap())?;
        fs::write(
            &paths.program,
            "#!/bin/sh\n\
             test \"$#\" -eq 8 || exit 90\n\
             test \"$1\" = capture && test \"$2\" = installed-hook || exit 91\n\
             test \"$3\" = --harness && test \"$4\" = claude || exit 92\n\
             test \"$5\" = --callback || exit 93\n\
             test \"$7\" = --installation && test \"$8\" = \"$EXPECTED_INSTALLATION\" || exit 96\n\
             IFS= read -r payload\n\
             test \"$payload\" = '{\"session_id\":\"native quote '\\'' body\"}' || exit 94\n\
             case \"$6\" in claude-hook) exit 31;; claude-wait) exit 32;; *) exit 95;; esac\n",
        )?;
        fs::set_permissions(&paths.program, fs::Permissions::from_mode(0o700))?;
        let definition = definition(&paths)?;
        for entry in definition
            .entries
            .iter()
            .filter(|entry| entry.keys.first().is_some_and(|key| key == "hooks"))
        {
            let command = entry.value["hooks"][0]["command"].as_str().unwrap();
            let mut child = Command::new("/bin/sh")
                .args(["-c", command])
                .env("EXPECTED_INSTALLATION", &manifest)
                .stdin(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"{\"session_id\":\"native quote ' body\"}\n")?;
            let code = if entry.id.ends_with("claude-wait") {
                32
            } else {
                31
            };
            assert_eq!(child.wait()?.code(), Some(code));
        }
        Ok(())
    }

    #[test]
    fn native_command_allowlists_do_not_fall_back_to_permissive_name_matching() {
        let command = json!(["/bkpr", "capture", "installed-mcp", "--harness", "claude"]);
        let denied = json!({"allowedMcpServers":[
            {"serverName":"bkpr"}, {"serverCommand":["/different"]}
        ]});
        assert!(check_policy(&[denied], &command).is_err());
        let allowed = json!({"allowedMcpServers":[{"serverCommand":command}]});
        assert!(
            check_policy(
                &[allowed, json!({"deniedMcpServers":[{"serverName":"bkpr"}]})],
                &command
            )
            .is_err()
        );
        assert!(check_policy(&[json!({"disableAllHooks":true})], &command).is_err());
    }

    #[test]
    fn managed_launch_preserves_project_policy_and_competing_mcp_registrations() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut paths = paths(root.path());
        let project = root.path().join("project");
        paths.cwd = project.join("child");
        fs::create_dir_all(&paths.cwd)?;
        fs::create_dir(&paths.home)?;
        let definition = definition(&paths)?;
        let entry = &definition.entries[0];
        let installed = &entry.value;
        let write_json = |path: &Path, value: &Value| -> Result<()> {
            fs::write(path, serde_json::to_vec(value)?)?;
            Ok(())
        };
        let account = json!({"mcpServers":{"bkpr":installed}});
        write_json(&entry.file, &account)?;
        let settings = json!({"permissions":{
            "ask":["mcp__bkpr__send_message"],
            "deny":["mcp__bkpr__channel_create"]
        }});
        let project_mcp = project.join(".mcp.json");
        write_json(
            &project_mcp,
            &json!({"mcpServers":{"other":{"command":"foreign"}}}),
        )?;
        assert_eq!(
            launch_mcp(&paths, &settings, installed)?,
            LaunchMcp::Approve
        );
        let project_settings = project.join(".claude/settings.local.json");
        fs::create_dir(project_settings.parent().unwrap())?;
        write_json(
            &project_settings,
            &json!({"permissions":{"defaultMode":"dontAsk"}}),
        )?;
        assert_eq!(
            launch_mcp(&paths, &settings, installed)?,
            LaunchMcp::Register
        );
        write_json(
            &project_settings,
            &json!({"allowedMcpServers":[{"serverCommand":["/different"]}]}),
        )?;
        assert_eq!(launch_mcp(&paths, &settings, installed)?, LaunchMcp::Native);
        fs::remove_file(project_settings)?;
        let competing = json!({"mcpServers":{"bkpr":{"command":"foreign"}}});
        write_json(&project_mcp, &competing)?;
        assert_eq!(launch_mcp(&paths, &settings, installed)?, LaunchMcp::Native);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&project_mcp)?)?,
            competing
        );
        fs::remove_file(project_mcp)?;
        let mut project_account = account.clone();
        project_account["projects"][project.to_str().unwrap()] = competing;
        write_json(&entry.file, &project_account)?;
        assert_eq!(launch_mcp(&paths, &settings, installed)?, LaunchMcp::Native);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&entry.file)?)?,
            project_account
        );
        Ok(())
    }
}
