//! Launch-local native conversation hooks; Claude alone produces semantic status.
use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use bkpr_protocol::{ConversationSource, HarnessKind};
use serde_json::{Value, json};

use super::{Capture, passthrough, run_managed, scrub};

const MAX_CALLBACK_BYTES: u64 = 1024 * 1024;

struct ArgumentPlan {
    settings: Option<(usize, bool)>,
    insert_at: usize,
}

// Unknown options belong to Claude's parser. In particular, never reinterpret a
// flag-looking option value as a Bkpr opt-out or inject after the literal `--`.
fn argument_plan(arguments: &[OsString]) -> Option<ArgumentPlan> {
    let mut plan = ArgumentPlan {
        settings: None,
        insert_at: arguments.len(),
    };
    let mut index = 0;
    let mut first_positional = true;
    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if argument == "--" {
            plan.insert_at = index;
            break;
        }
        let (option, value) = argument
            .split_once('=')
            .map_or((argument, None), |(option, value)| (option, Some(value)));
        match option {
            "--bare"
            | "--print"
            | "-p"
            | "--help"
            | "-h"
            | "--version"
            | "-v"
            | "--strict-mcp-config"
            | "--setting-sources" => return None,
            "--mcp-config"
            | "--dangerously-load-development-channels"
            | "--channels"
            | "--add-dir"
            | "--allowedTools"
            | "--allowed-tools"
            | "--disallowedTools"
            | "--disallowed-tools"
            | "--tools"
            | "--betas" => {
                let mut end = index + 1;
                while end < arguments.len() && !arguments[end].as_encoded_bytes().starts_with(b"-")
                {
                    end += 1;
                }
                if value.is_none() && end == index + 1 {
                    return None;
                }
                index = end - 1;
            }
            "--model"
            | "--name"
            | "-n"
            | "--settings"
            | "--permission-mode"
            | "--session-id"
            | "--effort"
            | "--plugin-dir"
            | "--agent"
            | "--agents"
            | "--append-system-prompt"
            | "--system-prompt"
            | "--permission-prompt-tool" => {
                let value_index = if value.is_some() { index } else { index + 1 };
                arguments.get(value_index)?;
                if option == "--settings"
                    && plan
                        .settings
                        .replace((value_index, value.is_some()))
                        .is_some()
                {
                    // Multiple settings sources have native precedence semantics.
                    return None;
                }
                if value.is_none() {
                    index += 1;
                }
            }
            "--resume" | "-r" => {
                if value.is_none()
                    && arguments
                        .get(index + 1)
                        .is_some_and(|next| !next.as_encoded_bytes().starts_with(b"-"))
                {
                    index += 1;
                }
            }
            "--continue"
            | "-c"
            | "--fork-session"
            | "--dangerously-skip-permissions"
            | "--allow-dangerously-skip-permissions"
            | "--ide"
            | "--chrome"
            | "--no-chrome"
            | "--disable-slash-commands"
            | "--verbose"
                if value.is_none() => {}
            _ if option.starts_with('-') => return None,
            _ => {
                if first_positional
                    && matches!(
                        argument,
                        "agents"
                            | "attach"
                            | "auth"
                            | "auto-mode"
                            | "doctor"
                            | "gateway"
                            | "import"
                            | "install"
                            | "logs"
                            | "mcp"
                            | "plugin"
                            | "plugins"
                            | "purge"
                            | "respawn"
                            | "rm"
                            | "setup-token"
                            | "stop"
                            | "kill"
                            | "ultrareview"
                            | "update"
                            | "upgrade"
                    )
                {
                    return None;
                }
                first_positional = false;
            }
        }
        index += 1;
    }
    Some(plan)
}

fn settings(arguments: &[OsString], plan: &ArgumentPlan) -> Result<Option<Value>> {
    let Some((index, inline)) = plan.settings else {
        return Ok(Some(json!({})));
    };
    let value = arguments[index]
        .to_str()
        .context("Claude settings argument is not UTF-8")?;
    let value = if inline {
        &value["--settings=".len()..]
    } else {
        value
    };
    if !value.trim_start().starts_with('{') {
        // File-relative permission rules must remain anchored to their file.
        return Ok(None);
    }
    let settings: Value = serde_json::from_str(value).context("parse inline Claude settings")?;
    if settings.get("disableAllHooks").and_then(Value::as_bool) == Some(true)
        || settings
            .get("allowManagedHooksOnly")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return Ok(None);
    }
    Ok(Some(settings))
}

fn compose(settings: &mut Value, program: &str) -> Result<()> {
    let object = settings
        .as_object_mut()
        .context("inline Claude settings must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude hooks must be an object")?;
    let command = format!("'{}' capture claude-hook", program.replace('\'', "'\\''"));
    for event in ["SessionStart", "UserPromptSubmit"] {
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("Claude {event} hooks must be an array"))?
            .push(json!({
                "hooks": [{"type": "command", "command": command, "timeout": 5}]
            }));
    }
    Ok(())
}

fn supplement(arguments: &mut Vec<OsString>, plan: ArgumentPlan, settings: Value) -> Result<()> {
    let settings = serde_json::to_string(&settings)?;
    if let Some((index, inline)) = plan.settings {
        arguments[index] = if inline {
            format!("--settings={settings}").into()
        } else {
            settings.into()
        };
    } else {
        arguments.splice(
            plan.insert_at..plan.insert_at,
            [OsString::from("--settings"), OsString::from(settings)],
        );
    }
    Ok(())
}

pub(super) fn launch(executable: PathBuf, mut arguments: Vec<OsString>) -> Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return passthrough(&executable, arguments);
    }
    let Some(plan) = argument_plan(&arguments) else {
        eprintln!(
            "bkpr: Claude invocation is unchanged; launch-local capture is not enabled for this argument mode"
        );
        return passthrough(&executable, arguments);
    };
    let Some(mut settings) = settings(&arguments, &plan)? else {
        eprintln!(
            "bkpr: Claude settings restrictions are preserved; launch-local capture is not enabled"
        );
        return passthrough(&executable, arguments);
    };
    // Prepare everything fallible before creating a backend launch grant.
    let program = std::env::current_exe().context("locate Claude capture callback")?;
    compose(
        &mut settings,
        program
            .to_str()
            .context("Claude callback path is not UTF-8")?,
    )?;
    supplement(&mut arguments, plan, settings)?;
    let capture = Capture::begin(HarnessKind::Claude)?;
    let mut command = Command::new(executable);
    scrub(&mut command);
    capture.apply(&mut command);
    command.args(arguments);
    run_managed(&mut command, &capture)
}

fn root_session(value: &Value) -> Result<Option<&str>> {
    // A named root --agent may set agent_type, so that field is not a child ID.
    if value.get("agent_id").is_some_and(|id| !id.is_null()) {
        return Ok(None);
    }
    if !matches!(
        value.get("hook_event_name").and_then(Value::as_str),
        Some("SessionStart" | "UserPromptSubmit")
    ) {
        bail!("unexpected Claude capture hook event");
    }
    let id = value
        .get("session_id")
        .and_then(Value::as_str)
        .context("Claude root hook has no native session_id")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("Claude root hook has an invalid native session_id");
    }
    Ok(Some(id))
}

fn callback_input(input: impl Read) -> Result<Value> {
    let mut bytes = Vec::new();
    input
        .take(MAX_CALLBACK_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read Claude capture hook")?;
    if bytes.len() as u64 > MAX_CALLBACK_BYTES {
        bail!("Claude capture hook exceeds {MAX_CALLBACK_BYTES} bytes");
    }
    serde_json::from_slice(&bytes).context("parse Claude capture hook JSON")
}

pub(super) fn hook() -> Result<()> {
    // Claude may discard stderr for a failed SessionStart hook. Publish an
    // authenticated diagnostic as well, without inventing a conversation/status.
    let capture = Capture::from_environment()?;
    if capture.grant.kind != HarnessKind::Claude {
        bail!("Claude hook does not belong to a Claude launch");
    }
    let result = (|| {
        let value = callback_input(std::io::stdin().lock())?;
        let Some(id) = root_session(&value)? else {
            return Ok(());
        };
        capture.bind(ConversationSource::ClaudeHook, id)
    })();
    if let Err(error) = &result {
        let _ = capture.error(&format!("Claude conversation capture failed: {error:#}"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn user_settings_and_hooks_survive_capture_augmentation() {
        let original = json!({
            "permissions": {"deny": ["Read(./secret)"]},
            "env": {"CUSTOM": "kept"},
            "hooks": {
                "SessionStart": [{"matcher": "resume", "hooks": [{"type": "command", "command": "user-start"}]}],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "user-prompt"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "user-stop"}]}]
            }
        });
        let mut merged = original.clone();
        compose(&mut merged, "/tmp/with ' quote/bkpr").unwrap();
        assert_eq!(merged["permissions"], original["permissions"]);
        assert_eq!(merged["env"], original["env"]);
        assert_eq!(merged["hooks"]["Stop"], original["hooks"]["Stop"]);
        for event in ["SessionStart", "UserPromptSubmit"] {
            assert_eq!(merged["hooks"][event][0], original["hooks"][event][0]);
        }
        let mut invalid = json!({"hooks": {"SessionStart": {"unexpected": true}}});
        assert!(compose(&mut invalid, "/bkpr").is_err());
    }

    #[test]
    fn settings_sources_and_explicit_hook_optouts_are_not_reinterpreted() {
        for values in [
            args(&["--settings", "./local/settings.json"]),
            args(&["--settings={\"disableAllHooks\":true}"]),
            args(&["--settings", "{\"allowManagedHooksOnly\":true}"]),
        ] {
            let plan = argument_plan(&values).unwrap();
            assert!(settings(&values, &plan).unwrap().is_none());
        }
        assert!(argument_plan(&args(&["--settings", "{}", "--settings={}"])).is_none());
        assert!(argument_plan(&args(&["--setting-sources=user"])).is_none());
        assert!(argument_plan(&args(&["--strict-mcp-config"])).is_none());
    }

    #[test]
    fn consumed_values_and_literal_prompts_do_not_select_passthrough() {
        assert!(
            argument_plan(&args(&[
                "--resume",
                "native-id",
                "--fork-session",
                "--model",
                "--print",
                "--",
                "--bare",
            ]))
            .is_some()
        );
        assert!(argument_plan(&args(&["--future-option", "--resume"])).is_none());
        assert!(argument_plan(&args(&["--print", "prompt"])).is_none());
        assert!(argument_plan(&args(&["auth", "login"])).is_none());
        assert!(argument_plan(&args(&["--continue"])).is_some());
        assert!(argument_plan(&args(&["--resume=native-id", "--fork-session"])).is_some());
    }

    #[test]
    fn root_native_ids_exclude_children_and_unrelated_hook_events() {
        assert_eq!(root_session(&json!({"hook_event_name":"SessionStart", "session_id":"new-id", "agent_type":"reviewer"})).unwrap(), Some("new-id"));
        assert_eq!(root_session(&json!({"hook_event_name":"UserPromptSubmit", "session_id":"fork-id", "agent_id":null})).unwrap(), Some("fork-id"));
        assert_eq!(
            root_session(&json!({"session_id":"parent-id", "agent_id":"child"})).unwrap(),
            None
        );
        for value in [
            json!({"hook_event_name":"Stop", "session_id":"id"}),
            json!({"hook_event_name":"SessionStart", "session_id":"bad\u{1b}id"}),
            json!({"hook_event_name":"SessionStart", "session_id":"x".repeat(129)}),
            json!({"hook_event_name":"SessionStart", "session_id":""}),
        ] {
            assert!(root_session(&value).is_err());
        }
        assert!(callback_input(&b"{broken"[..]).is_err());
        assert!(callback_input(std::io::repeat(b' ').take(MAX_CALLBACK_BYTES + 1)).is_err());
    }
}
