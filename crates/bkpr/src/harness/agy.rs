//! Antigravity capture uses only root-conversation MCP metadata. Launch never installs settings.
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use bkpr_protocol::{ConversationSource, HarnessKind};
use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use super::{Capture, passthrough, resolve, run_managed, scrub};

const MAX_FRAME: usize = 1024 * 1024;
const MAX_CONFIG: u64 = 16 * 1024 * 1024;
const ROOT_METADATA: &str = "antigravity.google/root_conversation_id";

// Option values are not commands; everything after -- is interactive prompt text.
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
            .map_or((argument, false), |(name, _)| (name, true));
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
                    if index == arguments.len() {
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

fn executable() -> Result<PathBuf> {
    resolve("agy").or_else(|_| resolve("antigravity"))
}

pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
        || !interactive_arguments(&arguments)
    {
        return passthrough(&executable, arguments);
    }
    let capture = Capture::begin(HarnessKind::Agy)?;
    let integration = config_path().and_then(|path| {
        let config = read_config(&path)?;
        integration_matches(&config, &capture.program)
    });
    if let Err(error) = integration {
        let diagnostic = format!(
            "Awaiting AGY integration: {error}. Run bkpr install agy explicitly; launch does not change AGY settings"
        );
        capture.error(&diagnostic)?;
        eprintln!("bkpr: {diagnostic}");
    }
    let mut command = Command::new(executable);
    scrub(&mut command);
    capture.apply(&mut command);
    command.args(arguments);
    run_managed(&mut command, &capture)
}

fn config_path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .context("AGY integration requires an absolute HOME")?;
    ensure!(
        !home.components().any(|part| part == Component::ParentDir),
        "HOME contains parent traversal"
    );
    Ok(home.join(".gemini/config/mcp_config.json"))
}

fn safe_path(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "refusing symlink integration path {}",
                    current.display()
                );
                if current != path {
                    ensure!(metadata.is_dir(), "integration ancestor is not a directory");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_config(path: &Path) -> Result<Value> {
    safe_path(path)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.len() <= MAX_CONFIG,
        "refusing unsafe or oversized AGY integration config"
    );
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG,
        "AGY integration config exceeds limit"
    );
    let config = strict_json(&bytes).context("malformed AGY MCP configuration")?;
    ensure!(
        config.is_object(),
        "AGY MCP configuration must be an object"
    );
    if let Some(servers) = config.get("mcpServers") {
        ensure!(servers.is_object(), "mcpServers must be an object");
    }
    Ok(config)
}

fn desired_entry(program: &Path) -> Value {
    json!({"command":program,"args":["capture","mcp","--harness","agy"],"disabled":false})
}

fn check_disabled(config: &Value) -> Result<()> {
    if let Some(disabled) = config.get("disabledServers") {
        let disabled = disabled
            .as_array()
            .context("disabledServers must be an array")?;
        ensure!(
            disabled.iter().all(Value::is_string),
            "disabledServers must contain names"
        );
        ensure!(
            !disabled.iter().any(|name| name == "bkpr"),
            "bkpr is explicitly disabled in AGY settings"
        );
    }
    Ok(())
}

fn integration_matches(config: &Value, program: &Path) -> Result<()> {
    check_disabled(config)?;
    let entry = config
        .get("mcpServers")
        .and_then(|servers| servers.get("bkpr"))
        .context("Bkpr MCP entry is missing")?;
    ensure!(
        entry == &desired_entry(program),
        "Bkpr MCP entry conflicts with this capture integration"
    );
    Ok(())
}

fn unrelated_config(mut config: Value) -> Value {
    if let Some(servers) = config.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove("bkpr");
        if servers.is_empty() {
            config
                .as_object_mut()
                .expect("validated config")
                .remove("mcpServers");
        }
    }
    config
}

/// Only the native AGY CLI writes its MCP registration. Existing conflicts fail before invoking it.
pub(super) fn install(arguments: Vec<OsString>) -> Result<i32> {
    let dry_run = arguments == [OsString::from("--dry-run")];
    ensure!(
        arguments.is_empty() || dry_run,
        "usage: bkpr install agy [--dry-run]"
    );
    let executable = executable()?;
    let program = env::current_exe().context("locate public Bkpr executable")?;
    program
        .to_str()
        .context("AGY MCP command path must be UTF-8")?;
    let path = config_path()?;
    let before = read_config(&path)?;
    check_disabled(&before)?;
    if before
        .get("mcpServers")
        .and_then(|servers| servers.get("bkpr"))
        .is_some()
    {
        integration_matches(&before, &program)?;
        println!(
            "Bkpr AGY MCP integration already configured in {}",
            path.display()
        );
        return Ok(0);
    }
    if dry_run {
        println!(
            "Would register only Bkpr MCP through AGY CLI in {}",
            path.display()
        );
        return Ok(0);
    }
    let mut command = Command::new(executable);
    scrub(&mut command);
    command
        .args(["mcp", "add", "bkpr", "--"])
        .arg(&program)
        .args(["capture", "mcp", "--harness", "agy"])
        .stdin(Stdio::null());
    let status = command
        .status()
        .context("register Bkpr MCP through AGY CLI")?;
    ensure!(
        status.success(),
        "AGY MCP registration failed with {status}"
    );
    let after = read_config(&path)?;
    integration_matches(&after, &program)
        .context("AGY CLI did not register the expected Bkpr entry")?;
    ensure!(
        unrelated_config(before) == unrelated_config(after),
        "AGY CLI unexpectedly changed unrelated MCP configuration; inspect {}",
        path.display()
    );
    println!(
        "Registered Bkpr AGY MCP integration in {}. Conversation capture awaits an authenticated root-metadata tool call; this is not OSC status or readiness.",
        path.display()
    );
    Ok(0)
}

fn root_metadata(message: &Value) -> Result<&str> {
    let root = message["params"]["_meta"][ROOT_METADATA].as_str().context(
        "AGY tools/call requires root_conversation_id metadata; no conversation is guessed",
    )?;
    ensure!(
        canonical_uuid(root),
        "AGY root_conversation_id must be a canonical lowercase UUID"
    );
    Ok(root)
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn live_context(capture: &Capture) -> Result<Value> {
    ensure!(
        capture.grant.kind == HarnessKind::Agy,
        "capture is not an AGY launch"
    );
    let context = capture.context()?;
    ensure!(
        context["active"] == true
            && context["harness"] == "agy"
            && context["launch_id"] == capture.grant.launch_id,
        "AGY capture is not the authenticated live launch"
    );
    Ok(context)
}

fn tools() -> Value {
    json!({"tools":[
        {"name":"whoami","description":"Bind this managed launch from authoritative AGY root-conversation metadata and return its actual authenticated Bkpr identity and live pane/workspace context. Does not provide peer messaging or imply semantic status.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"context","description":"Return this authenticated AGY launch's actual live Bkpr pane/workspace context, after binding the authoritative root metadata.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}
    ]})
}

fn tool_result(result: Result<Value>) -> Value {
    match result {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(error) => json!({"content":[{"type":"text","text":error.to_string()}],"isError":true}),
    }
}

fn read_frame(input: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            ensure!(frame.is_empty(), "truncated MCP frame");
            return Ok(None);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        ensure!(
            frame.len() + count <= MAX_FRAME,
            "MCP frame exceeds {MAX_FRAME} bytes"
        );
        frame.extend_from_slice(&available[..count]);
        input.consume(count);
        if newline.is_some() {
            return Ok(Some(frame));
        }
    }
}

fn write_frame(output: &mut impl Write, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() < MAX_FRAME, "MCP response exceeds frame limit");
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

// Short, bounded backend operations are processed serially: cancellation notifications have no
// outstanding asynchronous work to cancel and never produce responses or establish a binding.
fn serve(input: &mut impl BufRead, output: &mut impl Write, capture: &Capture) -> Result<()> {
    live_context(capture)?; // Authenticate source before advertising even the tool descriptions.
    let mut initialized = false;
    let mut ready = false;
    while let Some(frame) = read_frame(input)? {
        let message = match strict_json(&frame) {
            Ok(value) => value,
            Err(_) => {
                write_frame(output, &rpc_error(Value::Null, -32700, "Parse error"))?;
                continue;
            }
        };
        let id = message.get("id");
        let valid_id =
            id.is_none_or(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some());
        let method = message.get("method").and_then(Value::as_str);
        if !message.is_object()
            || message["jsonrpc"] != "2.0"
            || method.is_none()
            || !valid_id
            || message
                .get("params")
                .is_some_and(|params| !params.is_object())
        {
            write_frame(output, &rpc_error(Value::Null, -32600, "Invalid Request"))?;
            continue;
        }
        let method = method.expect("validated method");
        if id.is_none() {
            match method {
                "notifications/initialized" if initialized => {
                    ready = true;
                }
                "notifications/cancelled" | "notifications/roots/list_changed" => {}
                _ => {}
            }
            continue;
        }
        let id = id.expect("request id").clone();
        // Every request revalidates capability, process birth and lineage in the backend. A stale
        // MCP connection cannot keep serving a replaced or background launch's context.
        let authenticated = live_context(capture);
        if authenticated.is_err() {
            write_frame(
                output,
                &rpc_error(id, -32001, "AGY capture authorization is no longer live"),
            )?;
            return Ok(());
        }
        let response = match method {
            "initialize" if !initialized => {
                initialized = true;
                let version = message["params"]["protocolVersion"]
                    .as_str()
                    .filter(|version| {
                        matches!(
                            *version,
                            "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
                        )
                    })
                    .unwrap_or("2025-11-25");
                json!({"jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":version,"capabilities":{"tools":{}},
                    "serverInfo":{"name":"bkpr","version":env!("CARGO_PKG_VERSION")},
                    "instructions":"First call Bkpr whoami to bind this managed launch using authoritative AGY root-conversation tool metadata. Bkpr only supplies authenticated identity/context here; MCP connection, conversation binding and native OSC 7501 semantic status are separate facts. No peer messaging or receiver is advertised."
                }})
            }
            "ping" if initialized => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" if ready => json!({"jsonrpc":"2.0","id":id,"result":tools()}),
            "tools/call" if ready => {
                let result = (|| -> Result<Value> {
                    let root = root_metadata(&message)?;
                    capture.bind(ConversationSource::AgyMcp, root)?;
                    let name = message["params"]["name"]
                        .as_str()
                        .context("missing tool name")?;
                    ensure!(
                        matches!(name, "whoami" | "context"),
                        "unknown Bkpr context tool"
                    );
                    if let Some(arguments) = message["params"].get("arguments") {
                        ensure!(
                            arguments
                                .as_object()
                                .is_some_and(|arguments| arguments.is_empty()),
                            "context tools accept no arguments"
                        );
                    }
                    live_context(capture)
                })();
                json!({"jsonrpc":"2.0","id":id,"result":tool_result(result)})
            }
            "initialize" => rpc_error(id, -32600, "MCP connection is already initialized"),
            "tools/list" | "tools/call" | "ping" => {
                rpc_error(id, -32002, "MCP connection is not initialized")
            }
            _ => rpc_error(id, -32601, "Method not found"),
        };
        write_frame(output, &response)?;
    }
    Ok(())
}

pub(super) fn mcp() -> Result<i32> {
    let capture = Capture::from_environment()?;
    serve(&mut io::stdin().lock(), &mut io::stdout().lock(), &capture)?;
    Ok(0)
}

// serde_json normally accepts duplicate keys. Refuse ambiguous identity/configuration inputs.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<StrictValue, E> {
                serde_json::Number::from_f64(value)
                    .map(|value| StrictValue(Value::Number(value)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_string<E: de::Error>(
                self,
                value: String,
            ) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<StrictValue, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = access.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<StrictValue, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = access.next_entry::<String, StrictValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}
fn strict_json(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice::<StrictValue>(bytes)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_canonical_root_metadata_binds() {
        let root = "2b0ec54f-1f81-4c9c-821b-38d4fdc8d8af";
        let message = json!({"params":{"_meta":{"antigravity.google/root_conversation_id":root,"antigravity.google/conversation_id":"child"}}});
        assert_eq!(root_metadata(&message).unwrap(), root);
        for candidate in [
            root.to_uppercase(),
            root.replace('-', ""),
            format!("{root} "),
            "child".into(),
        ] {
            assert!(root_metadata(&json!({"params":{"_meta":{"antigravity.google/root_conversation_id":candidate}}})).is_err());
        }
        assert!(
            root_metadata(&json!({"params":{"_meta":{"antigravity.google/conversation_id":root}}}))
                .is_err()
        );
    }

    #[test]
    fn frames_are_bounded_complete_and_duplicate_free() {
        assert_eq!(
            read_frame(&mut &b"{\"jsonrpc\":\"2.0\"}\n"[..])
                .unwrap()
                .unwrap(),
            b"{\"jsonrpc\":\"2.0\"}\n"
        );
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        assert!(read_frame(&mut &vec![b' '; MAX_FRAME + 1][..]).is_err());
        assert!(strict_json(br#"{"params":{"x":1,"x":2}}"#).is_err());
        assert!(strict_json(br#"{"x":1}{}"#).is_err());
        assert!(read_frame(&mut &b""[..]).unwrap().is_none());
    }

    #[test]
    fn installer_refuses_conflicts_and_preserves_unrelated_values() {
        let program = Path::new("/fixture/bkpr");
        let before = json!({"mcpServers":{"other":{"command":"other","env":{"KEEP":"value"}}},"future":{"preserve":true}});
        let mut after = before.clone();
        after["mcpServers"]["bkpr"] = desired_entry(program);
        integration_matches(&after, program).unwrap();
        assert_eq!(unrelated_config(before), unrelated_config(after));
        for bad in [
            json!({"mcpServers":{"bkpr":{"command":"user-choice"}}}),
            json!({"mcpServers":{"bkpr":desired_entry(program)},"disabledServers":["bkpr"]}),
            json!({"mcpServers":{"bkpr":desired_entry(program)},"disabledServers":"malformed"}),
        ] {
            assert!(integration_matches(&bad, program).is_err());
        }
    }

    #[test]
    fn classification_preserves_option_values_and_resume() {
        let interactive = |args: &[&str]| {
            interactive_arguments(&args.iter().map(OsString::from).collect::<Vec<_>>())
        };
        assert!(interactive(&["--conversation", "root", "--continue"]));
        assert!(interactive(&["--model", "--print"]));
        assert!(interactive(&["--", "--print", "mcp"]));
        assert!(!interactive(&["mcp", "add", "x"]));
        assert!(!interactive(&["--print", "prompt"]));
        assert!(!interactive(&["--conversation"]));
    }
}
