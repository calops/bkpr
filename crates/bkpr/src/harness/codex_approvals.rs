//! Exact Bkpr tool defaults, resolved against Codex's own effective config.
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use socket2::{Domain, SockAddr, Socket, Type};
use tungstenite::{Message, WebSocket, client::client_with_config, protocol::WebSocketConfig};

use crate::harness::collaboration::TOOLS;

const TIMEOUT: Duration = Duration::from_secs(1);
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Build the temporary or installed relay registration without embedding credentials.
pub(in crate::harness) fn registration(program: &Path, manifest: Option<&Path>) -> Result<Value> {
    let mut args = vec!["capture"];
    if let Some(manifest) = manifest {
        args.extend([
            "installed-mcp",
            "--harness",
            "codex",
            "--installation",
            manifest
                .to_str()
                .context("Codex installation record path is not UTF-8")?,
        ]);
    } else {
        args.extend(["mcp", "--harness", "codex"]);
    }
    Ok(json!({
        "command": program.to_str().context("Codex MCP executable path is not UTF-8")?,
        "args": args,
        "env_vars": ["BKPR_AGENT_RELAY_SOCKET", "BKPR_AGENT_RELAY_TOKEN"]
    }))
}

fn server(config: &Value) -> &Value {
    &config["mcp_servers"]["bkpr"]
}

fn tool_mode<'a>(server: &'a Value, tool: &str) -> Option<&'a str> {
    server["tools"][tool]["approval_mode"].as_str()
}

fn server_mode(server: &Value) -> Option<&str> {
    server["default_tools_approval_mode"].as_str()
}

fn permits_defaults(policy: &Value) -> bool {
    policy.is_null()
        || matches!(policy.as_str(), Some("on-request" | "on-failure"))
        || policy["granular"]["mcp_elicitations"].as_bool() == Some(true)
}

fn user_reviewer(reviewer: &Value) -> bool {
    reviewer.is_null() || reviewer.as_str() == Some("user")
}

fn global_opt_out(config: &Value) -> bool {
    !permits_defaults(&config["approval_policy"]) || !user_reviewer(&config["approvals_reviewer"])
}

fn tool_exposed(server: &Value, tool: &str) -> bool {
    let lists = |key: &str| server[key].as_array();
    let lists_tool = |names: &[Value]| names.iter().any(|name| name.as_str() == Some(tool));
    server["enabled"].as_bool() != Some(false)
        && lists("enabled_tools").is_none_or(|names| lists_tool(names))
        && !lists("disabled_tools").is_some_and(|names| lists_tool(names))
}

/// Encode only the registration's transport settings for the native CLI.
pub(super) fn registration_overrides(registration: &Value) -> Result<[String; 3]> {
    let option = |key: &str| -> Result<String> {
        let value = toml::Value::try_from(&registration[key])?;
        Ok(format!("mcp_servers.bkpr.{key}={value}"))
    };
    // Codex replaces a parent table set twice in the same CLI layer. Set only
    // the private transport leaves so native flags keep their other settings.
    Ok([option("command")?, option("args")?, option("env_vars")?])
}

/// Resolve optional approvals for fresh threads on the launch's private app-server.
pub(super) struct ThreadDefaults<'a> {
    /// Existing native app-server endpoint, not the TUI-facing proxy.
    pub socket: &'a Path,
    /// Base directory against which native relative thread paths are resolved.
    pub cwd: &'a Path,
    /// Exact Bkpr transport identity required before any defaults are offered.
    pub registration: &'a Value,
}

impl ThreadDefaults<'_> {
    /// Return a replacement request payload, or pass the original through unchanged.
    pub fn rewrite(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.rewrite_with(bytes, |cwd| inspect(self.socket, cwd))
    }

    fn rewrite_with(
        &self,
        bytes: &[u8],
        read_config: impl FnOnce(&Path) -> Result<Value>,
    ) -> Option<Vec<u8>> {
        if bytes.len() > 64 * 1024 {
            return None;
        }
        let mut message: Value = serde_json::from_slice(bytes).ok()?;
        if message.get("method")?.as_str() != Some("thread/start")
            || !message
                .get("id")
                .is_some_and(|id| id.is_string() || id.is_i64() || id.is_u64())
        {
            return None;
        }
        let params = message.get_mut("params")?.as_object_mut()?;
        if params.get("threadSource").and_then(Value::as_str) != Some("user")
            || params
                .get("environments")
                .is_some_and(|value| !value.is_null())
            || caller_controls_mcp(params.get("config"))?
            || params
                .get("approvalPolicy")
                .is_some_and(|policy| !permits_defaults(policy))
            || params
                .get("approvalsReviewer")
                .is_some_and(|reviewer| !user_reviewer(reviewer))
        {
            return None;
        }
        let requested_cwd;
        let cwd = match params.get("cwd") {
            None | Some(Value::Null) => self.cwd,
            Some(cwd) => {
                requested_cwd = self.cwd.join(cwd.as_str()?);
                &requested_cwd
            }
        };
        let loaded = read_config(cwd).ok()?;
        let settings = overrides(&loaded, self.registration).ok()?;
        if settings.is_empty() {
            return None;
        }
        let config = params.entry("config").or_insert(Value::Null);
        if config.is_null() {
            *config = Value::Object(settings);
        } else {
            config.as_object_mut()?.extend(settings);
        }
        serde_json::to_vec(&message).ok()
    }
}

fn caller_controls_mcp(config: Option<&Value>) -> Option<bool> {
    Some(match config {
        None | Some(Value::Null) => false,
        Some(config) => config.as_object()?.keys().any(|key| {
            matches!(
                key.split('.').next(),
                Some("mcp_servers" | "approval_policy" | "approvals_reviewer")
            )
        }),
    })
}

struct DeadlineStream {
    socket: UnixStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Codex config read timed out",
            ))
        } else {
            Ok(remaining)
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(output)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

fn inspect(socket_path: &Path, cwd: &Path) -> Result<Value> {
    let cwd = cwd.to_str().context("Codex config cwd is not UTF-8")?;
    let deadline = Instant::now() + TIMEOUT;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    // A full Unix listener backlog must not stall native startup.
    socket.set_nonblocking(true)?;
    socket.connect(&SockAddr::unix(socket_path)?)?;
    socket.set_nonblocking(false)?;
    let stream = DeadlineStream {
        socket: socket.into(),
        deadline,
    };
    let limits = WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (mut client, _) = client_with_config("ws://localhost/", stream, Some(limits))
        .map_err(|_| anyhow::anyhow!("unable to query the private Codex app-server"))?;
    request(
        &mut client,
        json!({"id":1,"method":"initialize","params":{
            "clientInfo":{"name":"bkpr","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}
        }}),
    )?;
    response(&mut client, 1)?;
    request(&mut client, json!({"method":"initialized"}))?;
    request(
        &mut client,
        json!({"id":2,"method":"config/read","params":{"cwd":cwd,"includeLayers":true}}),
    )?;
    response(&mut client, 2)
}

fn request(client: &mut WebSocket<DeadlineStream>, message: Value) -> Result<()> {
    client.send(Message::Text(message.to_string().into()))?;
    Ok(())
}

fn response(client: &mut WebSocket<DeadlineStream>, id: u64) -> Result<Value> {
    loop {
        let message = client.read()?;
        ensure!(!message.is_close(), "Codex closed the config connection");
        let Message::Text(text) = message else {
            continue;
        };
        let mut message: Value = serde_json::from_str(text.as_str())
            .map_err(|_| anyhow::anyhow!("Codex returned invalid app-server JSON"))?;
        if message["id"].as_u64() == Some(id) {
            let result = message["result"].take();
            ensure!(
                result.is_object(),
                "Codex could not resolve its effective config"
            );
            return Ok(result);
        }
    }
}

fn overrides(loaded: &Value, registration: &Value) -> Result<Map<String, Value>> {
    let config = &loaded["config"];
    let bkpr = server(config);
    let layers = loaded["layers"]
        .as_array()
        .context("Codex did not return config layers")?;
    let exact_registration = registration
        .as_object()
        .unwrap()
        .iter()
        .all(|(key, value)| bkpr.get(key) == Some(value));
    if !exact_registration || global_opt_out(config) || server_mode(bkpr).is_some() {
        return Ok(Map::new());
    }
    let mut result = Map::new();
    for &tool in TOOLS {
        if !tool_exposed(bkpr, tool) || tool_mode(bkpr, tool).is_some() {
            continue;
        }
        // Onboarding can activate a previously untrusted project config.
        // Do not pin an approval across that transition when it has an opinion.
        let latent_restriction = layers.iter().any(|layer| {
            let server = server(&layer["config"]);
            !layer["disabledReason"].is_null()
                && (tool_mode(server, tool)
                    .or(server_mode(server))
                    .is_some_and(|mode| mode != "approve")
                    || global_opt_out(&layer["config"]))
        });
        if !latent_restriction {
            result.insert(
                format!("mcp_servers.bkpr.tools.{tool}.approval_mode"),
                Value::String("approve".into()),
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(registration: Value) -> Value {
        json!({"config":{"mcp_servers":{"bkpr":registration}},"origins":{},"layers":[]})
    }

    fn approval(tool: &str, mode: &str) -> (String, Value) {
        (
            format!("mcp_servers.bkpr.tools.{tool}.approval_mode"),
            json!(mode),
        )
    }

    #[test]
    fn launch_registers_only_transport_and_defaults_exact_tools() -> Result<()> {
        let registration = registration(Path::new("/private/Bkpr with \"quotes\""), None)?;
        let options = registration_overrides(&registration)?;
        let parsed: toml::Value = toml::from_str(&options.join("\n"))?;
        assert_eq!(
            serde_json::to_value(parsed)?,
            json!({"mcp_servers":{"bkpr":registration}})
        );
        let loaded = config(registration.clone());
        assert_eq!(
            overrides(&loaded, &registration)?,
            TOOLS
                .iter()
                .map(|tool| approval(tool, "approve"))
                .collect::<Map<_, _>>()
        );
        Ok(())
    }

    #[test]
    fn fresh_thread_rewrite_preserves_caller_fields_and_omits_unproven_defaults() -> Result<()> {
        let registration = registration(Path::new("/private/bkpr"), None)?;
        let defaults = ThreadDefaults {
            socket: Path::new("/unused/socket"),
            cwd: Path::new("/private/workspace"),
            registration: &registration,
        };
        let request = json!({"id":"origin/7","method":"thread/start","params":{
            "cwd":"subdir","threadSource":"user","approvalPolicy":"on-request",
            "config":{"model_reasoning_effort":"low"},"developerInstructions":"caller-owned"
        }});
        let wire = serde_json::to_vec(&request)?;
        let rewritten = defaults
            .rewrite_with(&wire, |cwd| {
                assert_eq!(cwd, Path::new("/private/workspace/subdir"));
                Ok(config(registration.clone()))
            })
            .context("fresh thread did not acquire its optional native settings")?;
        let mut rewritten: Value = serde_json::from_slice(&rewritten)?;
        let config = rewritten["params"]["config"].as_object_mut().unwrap();
        for &tool in TOOLS {
            let (key, value) = approval(tool, "approve");
            assert_eq!(config.remove(&key), Some(value));
        }
        assert_eq!(rewritten, request);
        assert!(
            defaults
                .rewrite_with(&wire, |_| anyhow::bail!("read failed"))
                .is_none()
        );
        for method in ["thread/resume", "thread/fork"] {
            let mut selection = request.clone();
            selection["method"] = json!(method);
            assert!(
                defaults
                    .rewrite_with(&serde_json::to_vec(&selection)?, |_| unreachable!())
                    .is_none()
            );
        }
        for source in [None, Some(Value::Null), Some(json!("thread_title"))] {
            let mut non_user = request.clone();
            let params = non_user["params"].as_object_mut().unwrap();
            params.remove("threadSource");
            if let Some(source) = source {
                params.insert("threadSource".into(), source);
            }
            assert!(
                defaults
                    .rewrite_with(&serde_json::to_vec(&non_user)?, |_| unreachable!())
                    .is_none()
            );
        }
        for key in [
            "mcp_servers",
            "mcp_servers.bkpr.tools.channel_ack",
            "approval_policy",
            "approvals_reviewer",
        ] {
            let mut explicit = request.clone();
            explicit["params"]["config"][key] = Value::Null;
            assert!(
                defaults
                    .rewrite_with(&serde_json::to_vec(&explicit)?, |_| unreachable!())
                    .is_none()
            );
        }
        let mut review = request;
        review["params"]["approvalsReviewer"] = json!("auto-review");
        assert!(
            defaults
                .rewrite_with(&serde_json::to_vec(&review)?, |_| unreachable!())
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn explicit_modes_filters_disables_and_global_denial_survive() -> Result<()> {
        let registration = registration(Path::new("/private/bkpr"), None)?;
        let mut loaded = config(registration.clone());
        let bkpr = &mut loaded["config"]["mcp_servers"]["bkpr"];
        bkpr["enabled_tools"] = json!(["channel_ack", "channel_post", "channel_read", "control"]);
        bkpr["disabled_tools"] = json!(["channel_read"]);
        bkpr["tools"] = json!({"channel_ack":{"approval_mode":"prompt"}});
        assert_eq!(
            overrides(&loaded, &registration)?,
            Map::from_iter([approval("channel_post", "approve")])
        );
        let mut disabled = loaded.clone();
        disabled["config"]["mcp_servers"]["bkpr"]["enabled"] = json!(false);
        assert!(overrides(&disabled, &registration)?.is_empty());
        let mut fallback = loaded.clone();
        fallback["config"]["mcp_servers"]["bkpr"]["default_tools_approval_mode"] = json!("writes");
        assert!(overrides(&fallback, &registration)?.is_empty());
        loaded["config"]["approval_policy"] = json!({"granular":{"mcp_elicitations":false}});
        assert!(overrides(&loaded, &registration)?.is_empty());
        Ok(())
    }

    #[test]
    fn latent_project_restriction_does_not_get_pinned() -> Result<()> {
        let registration = registration(Path::new("/private/bkpr"), None)?;
        let mut loaded = config(registration.clone());
        loaded["layers"] = json!([{
            "name":{"type":"project"},
            "disabledReason":"untrusted project",
            "config":{"mcp_servers":{"bkpr":{"tools":{"channel_ack":{"approval_mode":"prompt"}}}}}
        }]);
        let settings = overrides(&loaded, &registration)?;
        assert!(!settings.contains_key("mcp_servers.bkpr.tools.channel_ack.approval_mode"));
        assert_eq!(settings.len() + 1, TOOLS.len());
        loaded["config"]["mcp_servers"]["bkpr"]["command"] = json!("/foreign/server");
        assert!(overrides(&loaded, &registration)?.is_empty());
        Ok(())
    }
}
