//! Launch-local native conversation hooks; Claude alone produces semantic status.
use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use bkpr_protocol::{ConversationSource, HarnessKind};
use serde_json::{Value, json};

use super::{
    Capture, assets, claude_inbox, collaboration, install, passthrough, run_managed, scrub,
};

const MAX_CALLBACK_BYTES: u64 = 1024 * 1024;

struct ArgumentPlan {
    settings: Option<(usize, bool)>,
    insert_at: usize,
    external_mcp_config: bool,
    native_approval_override: bool,
}

// Unknown options belong to Claude's parser. In particular, never reinterpret a
// flag-looking option value as a Bkpr opt-out or inject after the literal `--`.
fn argument_plan(arguments: &[OsString]) -> Option<ArgumentPlan> {
    let mut plan = ArgumentPlan {
        settings: None,
        insert_at: arguments.len(),
        external_mcp_config: false,
        native_approval_override: false,
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
                plan.external_mcp_config |= option == "--mcp-config";
                plan.native_approval_override |= option == "--add-dir";
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
                plan.native_approval_override |= matches!(
                    option,
                    "--permission-mode"
                        | "--permission-prompt-tool"
                        | "--plugin-dir"
                        | "--agent"
                        | "--agents"
                );
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
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions"
                if value.is_none() =>
            {
                plan.native_approval_override = true;
            }
            "--continue"
            | "-c"
            | "--fork-session"
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
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
    ] {
        hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("Claude {event} hooks must be an array"))?
            .push(json!({
                "hooks": [{"type": "command", "command": command, "timeout": 5}]
            }));
    }
    for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
        hooks.entry(event).or_insert_with(|| json!([])).as_array_mut()
            .with_context(|| format!("Claude {event} hooks must be an array"))?
            .push(json!({"hooks":[{"type":"command","command":format!("'{}' capture claude-wait", program.replace('\'', "'\\''")),"asyncRewake":true,"timeout":86400}]}));
    }
    Ok(())
}

fn add_collaboration_defaults(settings: &mut Value) {
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    let Some(permissions) = settings
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return;
    };
    let Some(allow) = permissions
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return;
    };
    for rule in collaboration::claude_rules().map(Value::String) {
        if !allow.contains(&rule) {
            allow.push(rule);
        }
    }
}

fn supplement(
    arguments: &mut Vec<OsString>,
    plan: &ArgumentPlan,
    settings: Value,
) -> Result<usize> {
    let settings = serde_json::to_string(&settings)?;
    if let Some((index, inline)) = plan.settings {
        arguments[index] = if inline {
            format!("--settings={settings}").into()
        } else {
            settings.into()
        };
        Ok(plan.insert_at)
    } else {
        arguments.splice(
            plan.insert_at..plan.insert_at,
            [OsString::from("--settings"), OsString::from(settings)],
        );
        Ok(plan.insert_at + 2)
    }
}

fn compose_launch(
    arguments: &mut Vec<OsString>,
    plan: ArgumentPlan,
    mut settings: Value,
    program: &Path,
    installation: Option<&Path>,
    resolve_mcp: impl FnOnce(&Value, &Value) -> install::ClaudeMcp,
) -> Result<()> {
    let registration = (!plan.external_mcp_config
        && (installation.is_none() || !plan.native_approval_override))
        .then(|| install::claude_registration(program, installation))
        .transpose()?;
    let mcp = registration
        .as_ref()
        .map(|registration| resolve_mcp(&settings, registration))
        .unwrap_or_default();
    let defaults = !plan.native_approval_override && mcp == install::ClaudeMcp::Approve;
    if installation.is_none() {
        compose(
            &mut settings,
            program
                .to_str()
                .context("Claude callback path is not UTF-8")?,
        )?;
    }
    if defaults {
        add_collaboration_defaults(&mut settings);
    }
    let insertion = if installation.is_none() || defaults {
        supplement(arguments, &plan, settings)?
    } else {
        plan.insert_at
    };
    if let Some(registration) = registration
        && (defaults || installation.is_none() && mcp != install::ClaudeMcp::Native)
    {
        // Native dynamic MCP config pins this exact registration for the run.
        // Do not use strict mode: native project permissions and other servers
        // must still load, and explicit --mcp-config is not ours to replace.
        let server = serde_json::to_string(&json!({"mcpServers":{"bkpr":registration}}))?;
        arguments.splice(
            insertion..insertion,
            [OsString::from("--mcp-config"), OsString::from(server)],
        );
    }
    Ok(())
}

/// Run supported interactive native launches with scoped capture; preserve native utility modes and exit status.
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
    let Some(settings) = settings(&arguments, &plan)? else {
        eprintln!(
            "bkpr: Claude settings restrictions are preserved; launch-local capture is not enabled"
        );
        return passthrough(&executable, arguments);
    };
    let runtime = install::claude_runtime()?;
    let installed = runtime.installation.as_ref();
    let capture = Capture::begin(HarnessKind::Claude)?;
    compose_launch(
        &mut arguments,
        plan,
        settings,
        &capture.program,
        installed.map(|install| install.manifest.as_path()),
        |settings, registration| runtime.mcp_policy(settings, registration),
    )?;
    let inbox = assets::private_directory("bkpr-claude-")?;
    let credentials = inbox.path().join("capture.json");
    if installed.is_some() {
        assets::write_credentials(&capture, &credentials)?;
    }
    let mut command = Command::new(executable);
    scrub(&mut command);
    capture.apply(&mut command);
    command.env("BKPR_CLAUDE_INBOX_DIR", inbox.path());
    if installed.is_some() {
        command.env("BKPR_INTEGRATION_FILE", credentials);
    }
    command.args(arguments);
    let result = run_managed(&mut command, &capture);
    drop(inbox);
    result
}

fn root_session(value: &Value) -> Result<Option<&str>> {
    // A named root --agent may set agent_type, so that field is not a child ID.
    if value.get("agent_id").is_some_and(|id| !id.is_null()) {
        return Ok(None);
    }
    if !matches!(
        value.get("hook_event_name").and_then(Value::as_str),
        Some(
            "SessionStart"
                | "UserPromptSubmit"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "Stop"
                | "StopFailure"
        )
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

fn publish_conversation(capture: &Capture, value: &Value, id: &str) -> Result<()> {
    if matches!(
        value["hook_event_name"].as_str(),
        Some("SessionStart" | "UserPromptSubmit")
    ) {
        capture.bind(ConversationSource::ClaudeHook, id)?;
        if value["hook_event_name"] == "UserPromptSubmit" {
            capture.prompt_submitted(ConversationSource::ClaudeHook, id)?;
        }
    }
    Ok(())
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
        publish_conversation(&capture, &value, id)?;
        claude_inbox::hook(&capture, &value, id, &mut std::io::stdout().lock())
    })();
    if let Err(error) = &result {
        let _ = capture.error(&format!("Claude conversation capture failed: {error:#}"));
    }
    result
}

pub(super) fn wait() -> Result<i32> {
    let capture = Capture::from_environment()?;
    if capture.grant.kind != HarnessKind::Claude {
        bail!("Claude waiter does not belong to Claude");
    }
    let result = (|| {
        let value = callback_input(std::io::stdin().lock())?;
        let Some(id) = root_session(&value)? else {
            return Ok(0);
        };
        claude_inbox::wait(&capture, &value, id)
    })();
    if let Err(error) = &result {
        let _ = capture.error(&format!("Claude inbox notification unavailable: {error:#}"));
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
        assert_eq!(merged["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
        for event in ["SessionStart", "UserPromptSubmit"] {
            assert_eq!(merged["hooks"][event][0], original["hooks"][event][0]);
        }
        let mut invalid = json!({"hooks": {"SessionStart": {"unexpected": true}}});
        assert!(compose(&mut invalid, "/bkpr").is_err());
    }

    #[test]
    fn collaboration_defaults_append_only_exact_native_names() {
        let mut config = json!({
            "permissions": {
                "defaultMode": "auto",
                "allow": ["Read", "mcp__bkpr__channel_post"],
                "ask": ["mcp__bkpr__channel_create"],
                "deny": ["mcp__bkpr__send_message"],
            },
        });
        let mut expected = config.clone();
        let allow = expected["permissions"]["allow"].as_array_mut().unwrap();
        allow.extend(
            collaboration::claude_rules()
                .filter(|rule| rule != "mcp__bkpr__channel_post")
                .map(Value::String),
        );
        add_collaboration_defaults(&mut config);
        assert_eq!(config, expected);
        add_collaboration_defaults(&mut config);
        assert_eq!(config, expected);
        let forbidden = [
            "control",
            "terminal_input",
            "channel_upload",
            "channel_fetch",
            "*",
        ];
        for tool in forbidden {
            assert!(
                !config["permissions"]["allow"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("mcp__bkpr__{tool}")))
            );
        }
    }

    #[test]
    fn grants_require_a_launch_pinned_registration_and_preserve_native_arguments() -> Result<()> {
        let program = Path::new("/absolute/bkpr");
        let manifest = Path::new("/owned/manifest.json");
        let literal = args(&[
            "--settings={\"permissions\":{\"ask\":[\"mcp__bkpr__channel_create\"]}}",
            "--",
            "--mcp-config",
        ]);
        for installation in [None, Some(manifest)] {
            let mut argv = literal.clone();
            let plan = argument_plan(&argv).unwrap();
            let native_settings = settings(&argv, &plan)?.unwrap();
            let registration = install::claude_registration(program, installation)?;
            compose_launch(
                &mut argv,
                plan,
                native_settings,
                program,
                installation,
                |_, mcp| {
                    assert_eq!(*mcp, registration);
                    install::ClaudeMcp::Approve
                },
            )?;
            let settings: Value = serde_json::from_str(
                argv[0]
                    .to_str()
                    .unwrap()
                    .strip_prefix("--settings=")
                    .unwrap(),
            )?;
            assert_eq!(
                settings["permissions"]["ask"],
                json!(["mcp__bkpr__channel_create"])
            );
            assert_eq!(
                settings["permissions"]["allow"],
                json!(collaboration::claude_rules().collect::<Vec<_>>())
            );
            assert_eq!(settings.get("hooks").is_some(), installation.is_none());
            assert_eq!(argv[1], "--mcp-config");
            let mcp: Value = serde_json::from_str(argv[2].to_str().unwrap())?;
            assert_eq!(mcp["mcpServers"]["bkpr"], registration);
            assert_eq!(&argv[3..], &literal[1..]);
        }
        for native in [
            args(&["--mcp-config=/user/mcp.json"]),
            args(&["--mcp-config", "/user/mcp.json"]),
            args(&["--permission-mode", "dontAsk"]),
            args(&["--agent=custom"]),
            args(&["--plugin-dir", "/user/plugin"]),
        ] {
            let mut argv = native.clone();
            let plan = argument_plan(&argv).unwrap();
            compose_launch(
                &mut argv,
                plan,
                json!({}),
                program,
                Some(manifest),
                |_, _| unreachable!(),
            )?;
            assert_eq!(argv, native);
        }
        let mut argv = args(&["--mcp-config", "/user/mcp.json"]);
        let plan = argument_plan(&argv).unwrap();
        compose_launch(
            &mut argv,
            plan,
            json!({}),
            program,
            None,
            |_, _| unreachable!(),
        )?;
        assert_eq!(&argv[..2], &args(&["--mcp-config", "/user/mcp.json"]));
        assert_eq!(argv[2], "--settings");
        let hooks_only: Value = serde_json::from_str(argv[3].to_str().unwrap())?;
        assert!(hooks_only.get("hooks").is_some());
        assert!(hooks_only.get("permissions").is_none());
        let mut argv = literal.clone();
        let plan = argument_plan(&argv).unwrap();
        let native_settings = settings(&argv, &plan)?.unwrap();
        compose_launch(
            &mut argv,
            plan,
            native_settings,
            program,
            Some(manifest),
            |_, _| install::ClaudeMcp::Native,
        )?;
        assert_eq!(argv, literal);
        Ok(())
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
            json!({"hook_event_name":"Notification", "session_id":"id"}),
            json!({"hook_event_name":"SessionStart", "session_id":"bad\u{1b}id"}),
            json!({"hook_event_name":"SessionStart", "session_id":"x".repeat(129)}),
            json!({"hook_event_name":"SessionStart", "session_id":""}),
        ] {
            assert!(root_session(&value).is_err());
        }
        assert!(callback_input(&b"{broken"[..]).is_err());
        assert!(callback_input(std::io::repeat(b' ').take(MAX_CALLBACK_BYTES + 1)).is_err());
    }
    #[test]
    fn native_prompt_callback_sends_only_bound_body_free_facts() {
        use bkpr_protocol::{CaptureEvent, CaptureGrant, Operation, Request, Response, wire};
        use std::{os::unix::net::UnixListener, sync::atomic::AtomicBool, thread};
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("capture.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let capture = Capture {
            grant: CaptureGrant {
                launch_id: "launch".into(),
                token: "x".repeat(64),
                pane_id: "pane".into(),
                kind: HarnessKind::Claude,
            },
            program: "/unused/bkpr".into(),
            socket,
            revoke_on_drop: false,
            ended: AtomicBool::new(false),
        };
        let server = thread::spawn(move || {
            let mut received = Vec::new();
            let mut codec = wire::Codec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let bytes =
                    wire::read_frame(&mut stream, wire::Kind::Request, wire::MAX_REQUEST).unwrap();
                let request: Request = wire::decode_request(&bytes).unwrap();
                received.push(request.operation);
                let response = Response {
                    version: bkpr_protocol::PROTOCOL_VERSION,
                    ok: true,
                    result: Some(json!({"accepted":true}).into()),
                    error: None,
                    failure: None,
                };
                let bytes = codec.encode_response(&response).unwrap();
                wire::write_frame(&mut stream, wire::Kind::Response, bytes).unwrap();
            }
            received
        });
        for event in ["SessionStart", "UserPromptSubmit", "PostToolUse"] {
            let value = json!({"hook_event_name":event, "session_id":"native",
                "prompt":"PRIVATE_PROMPT", "tool_input":{"text":"PRIVATE_TOOL"}});
            let id = root_session(&value).unwrap().unwrap();
            publish_conversation(&capture, &value, id).unwrap();
        }
        let received = server.join().unwrap();
        assert!(matches!(
            &received[0],
            Operation::HarnessEvent {
                event: CaptureEvent::Bind { .. },
                ..
            }
        ));
        assert!(matches!(
            &received[1],
            Operation::HarnessEvent {
                event: CaptureEvent::Bind { .. },
                ..
            }
        ));
        assert!(matches!(&received[2], Operation::HarnessEvent {
            event:CaptureEvent::PromptSubmitted{conversation_id,source:ConversationSource::ClaudeHook},..
        } if conversation_id == "native"));
        let encoded = serde_json::to_string(&received).unwrap();
        assert!(!encoded.contains("PRIVATE_PROMPT") && !encoded.contains("PRIVATE_TOOL"));
    }
}
