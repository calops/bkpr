//! Antigravity capture uses only root-conversation MCP metadata. Launch never installs settings.
use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use bkpr_protocol::{ConversationSource, HarnessKind};
use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use super::agent;
use super::{Capture, passthrough, run_managed};
#[path = "agy_receiver.rs"]
mod native_receiver;

const MAX_FRAME: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
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

/// Run supported interactive native launches with scoped capture; preserve native utility modes and exit status.
pub(super) fn launch(executable: PathBuf, arguments: Vec<OsString>) -> Result<i32> {
    if !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
        || !interactive_arguments(&arguments)
    {
        return passthrough(&executable, arguments);
    }
    let installed = super::install::runtime(HarnessKind::Agy, Default::default())?;
    let capture = Capture::begin(HarnessKind::Agy)?;
    if installed.is_none() {
        let diagnostic = "Awaiting AGY integration: run bkpr install agy explicitly; launch does not change native settings";
        capture.error(diagnostic)?;
        eprintln!("bkpr: {diagnostic}");
    }
    let credentials = super::assets::private_directory("bkpr-agy-")?;
    let credential_file = credentials.path().join("capture.json");
    super::assets::write_credentials(&capture, &credential_file)?;
    let mut command = Command::new(executable);
    capture.apply(&mut command);
    command.env("BKPR_INTEGRATION_FILE", credential_file);
    command.args(arguments);
    run_managed(&mut command, &capture)
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

fn bootstrap(capture: &Capture) -> Result<String> {
    let program = capture
        .program
        .to_str()
        .context("AGY receiver path is not UTF-8")?;
    let command = format!("'{}' capture agy-receiver", program.replace('\'', "'\\''"));
    Ok(format!(
        "First call Bkpr whoami to bind this launch from authoritative root-conversation metadata. If native receiver readiness is false, bootstrap once for this root using AGY's native run_command tool with CommandLine={}, IsDaemon:true, under ordinary native approvals. Never type this command into a terminal. Do not start another receiver while one is ready. The native child alone retains its local language-server credentials and sends only opaque inbox pointers through SendAgentMessage, including during active turns. Binding, active subscription, native acceptance, reading, explicit processed acknowledgement and semantic status are separate facts.",
        serde_json::to_string(&command)?,
    ))
}

fn tools() -> Value {
    let mut tools = agent::tools();
    tools.as_array_mut().expect("common native tool array").push(json!({
        "name":"context",
        "description":"Return this authenticated AGY launch's live Bkpr context after binding authoritative root-conversation metadata.",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
    }));
    json!({"tools":tools})
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
    ensure!(
        bytes.len() < MAX_RESPONSE,
        "MCP response exceeds frame limit"
    );
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
    let pages = agent::output::Pages::default();
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
                    "protocolVersion":version,"capabilities":{"tools":{},"resources":{}},
                    "serverInfo":{"name":"bkpr","version":env!("CARGO_PKG_VERSION")},
                    "instructions":format!("{}\n{}", agent::INSTRUCTIONS, bootstrap(capture)?)
                }})
            }
            "ping" if initialized => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" if ready => json!({"jsonrpc":"2.0","id":id,"result":tools()}),
            "resources/list" if ready => {
                json!({"jsonrpc":"2.0","id":id,"result":agent::resources()})
            }
            "resources/templates/list" if ready => {
                json!({"jsonrpc":"2.0","id":id,"result":{"resourceTemplates":[]}})
            }
            "resources/read" if ready => {
                match agent::resource(message["params"]["uri"].as_str().unwrap_or("")) {
                    Some(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    None => rpc_error(id, -32002, "resource not found"),
                }
            }
            "tools/call" if ready => {
                let mut receiver_instructions = None;
                let mut origin = None;
                let mut dispatched = false;
                let result = (|| -> Result<Value> {
                    let root = root_metadata(&message)?;
                    capture.bind(ConversationSource::AgyMcp, root)?;
                    let name = message["params"]["name"]
                        .as_str()
                        .context("missing tool name")?;
                    let arguments = message["params"]
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    // Obtain readiness before any user mutation: a later diagnostic failure must
                    // never hide a durable send's successful result.
                    let identity = agent::call(capture, "whoami", json!({}))?;
                    origin = Some(agent::output::Owner::from_identity(&identity)?);
                    let instructions = bootstrap(capture)?;
                    let value = if name == "context" {
                        ensure!(
                            arguments.as_object().is_some_and(|value| value.is_empty()),
                            "context accepts no arguments"
                        );
                        live_context(capture)?
                    } else if name == "read_result" {
                        pages.read(
                            origin.as_ref().expect("authenticated tool origin"),
                            arguments,
                        )?
                    } else if name == "whoami"
                        && arguments.as_object().is_some_and(|value| value.is_empty())
                    {
                        identity.clone()
                    } else {
                        dispatched = true;
                        agent::call(capture, name, arguments)?
                    };
                    if identity["notification_ready"] != true {
                        receiver_instructions = Some(instructions);
                    }
                    let value = match receiver_instructions {
                        Some(instructions) => {
                            let mut value = value;
                            if let Some(object) = value.as_object_mut() {
                                object.insert("receiver_instructions".into(), json!(instructions));
                                value
                            } else {
                                json!({"result":value,"receiver_instructions":instructions})
                            }
                        }
                        None => value,
                    };
                    Ok(value)
                })();
                let result = if dispatched {
                    result
                } else {
                    result.map_err(agent::before_dispatch_error)
                };
                let result = agent::tool_result(&pages, result, origin.as_ref(), || {
                    agent::output::Owner::from_identity(&agent::call(capture, "whoami", json!({}))?)
                });
                json!({"jsonrpc":"2.0","id":id,"result":result})
            }
            "initialize" => rpc_error(id, -32600, "MCP connection is already initialized"),
            "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/read"
            | "resources/templates/list"
            | "ping" => rpc_error(id, -32002, "MCP connection is not initialized"),
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

pub(super) fn receiver(arguments: Vec<OsString>) -> Result<i32> {
    ensure!(arguments.is_empty(), "usage: bkpr capture agy-receiver");
    native_receiver::run()
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
pub(super) fn strict_json(bytes: &[u8]) -> Result<Value> {
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
