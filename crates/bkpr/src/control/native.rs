use super::Caller;
use crate::context::LiveSession;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST: AtomicU64 = AtomicU64::new(1);
// Bound Bkpr-owned pipe ingress; native CLI actions use their existing argv contract.
const MAX_BRIDGE_REQUEST_BYTES: usize = 16 * 1024;

/// A failed pipe/write/read must never leave an unreaped ingress process.
struct Ingress(std::process::Child);
impl Drop for Ingress {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) struct Native {
    pub session: LiveSession,
    pub source: Option<String>,
    zellij: Option<std::path::PathBuf>,
}
impl Native {
    pub fn resolve(selector: Option<&str>, caller: &Caller, zellij: Option<&Path>) -> Result<Self> {
        let current = match caller {
            Caller::Agent { agent_id, context } => {
                let live = crate::context::agent_contexts()?;
                let current = live
                    .get(agent_id)
                    .context("authenticated agent no longer has a live native context")?;
                if current != context {
                    bail!(
                        "authenticated agent context changed; request fresh context before retrying"
                    );
                }
                Some(current.clone())
            }
            Caller::Operator => {
                if std::env::var_os("BKPR_SESSION_ID").is_some() {
                    match crate::context::current() {
                        Ok(context) => Some(context),
                        Err(_) if selector.is_some() => None, // explicit same-user targeting does not borrow stale pane authority
                        Err(error) => return Err(error),
                    }
                } else {
                    None
                }
            }
        };
        let selector = selector
            .or_else(|| current.as_ref().map(|context| context.session_id.as_str()))
            .context(
                "outside a managed pane, select a live Bkpr session explicitly with --session",
            )?;
        let session = crate::context::resolve_session(selector)?
            .context("no live authenticated Bkpr session matches the selector (legacy sessions have no control capability)")?;
        let source = current
            .filter(|context| context.session_id == session.session_id)
            .map(|context| context.pane_id);
        Ok(Self {
            session,
            source,
            zellij: zellij.map(Path::to_path_buf),
        })
    }
    /// SDK-only operations use authorized Bkpr replicas; client operations name
    /// one exact normal-sidebar replica, while session-wide mutations use the owner.
    pub fn call(&self, method: &str, arguments: Value) -> Result<Value> {
        if !matches!(
            method,
            "pane.set_state"
                | "workspace.rename"
                | "workspace.close"
                | "client.focus"
                | "client.mode"
                | "client.detach"
        ) {
            bail!("no baseline owner bridge implementation for {method}");
        }
        let clients = self.clients()?;
        let (client, receiver) = if method.starts_with("client.") {
            let id = arguments
                .get("client_id")
                .and_then(Value::as_u64)
                .and_then(|id| u16::try_from(id).ok())
                .context("client_id must fit the native u16 identity")?;
            let selected = clients
                .iter()
                .find(|entry| entry["client_id"].as_u64() == Some(u64::from(id)))
                .context("selected client is not attached to the target session")?;
            let plugin_url = crate::launcher::control_plugin_url(&self.session.runtime_name)?;
            (
                id,
                sidebar_receiver(&self.list("list-panes")?, selected, &plugin_url)?,
            )
        } else {
            let (request_id, output) = self
                .bridge_output("owner.discover", json!({}), 0, None)
                .context("read-only owner discovery failed; no mutation was dispatched")?;
            owner_receiver(&output, &request_id, &clients)?
        };
        if method.starts_with("client.") {
            // Permission prompting is a read-only prerequisite. The selected
            // sidebar never receives the mutation until this request completes.
            let (ready_id, output) = self
                .bridge_output("client.ready", json!({}), client, Some(receiver))
                .context("read-only client readiness failed; no mutation was dispatched")?;
            let ready = decode(&output, &ready_id)
                .context("invalid client readiness response; no mutation was dispatched")?;
            if ready["outcome"] != "completed" || ready["result"]["ready"] != true {
                bail!(
                    "selected client sidebar is not ready; no mutation was dispatched: {}",
                    ready["error"]
                );
            }
        }
        let (request_id, output) = self.bridge_output(method, arguments, client, Some(receiver))?;
        decode(&output, &request_id)
    }
    fn bridge_output(
        &self,
        method: &str,
        arguments: Value,
        client: u16,
        receiver: Option<u32>,
    ) -> Result<(String, Vec<u8>)> {
        let request_id = format!(
            "{}-{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        let mut request = json!({"version":1,"request_id":request_id,"session_id":self.session.session_id,"client_id":client,"method":method,"arguments":arguments});
        if let Some(receiver) = receiver {
            request["receiver_plugin_id"] = json!(receiver);
        }
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() > MAX_BRIDGE_REQUEST_BYTES {
            bail!("owner bridge request exceeds 16384 bytes");
        }
        let mut command = self.command()?;
        command
            .args(["action", "pipe", "--name", "bkpr-control", "--"])
            .arg(std::str::from_utf8(&bytes).expect("JSON is UTF-8"));
        let output = run(command, Some(bkpr::MAX_MESSAGE_BYTES), true)
            .with_context(|| format!("Bkpr bridge {method} request {request_id}"))?;
        if output.stdout.is_empty() {
            bail!(
                "no matching Bkpr control bridge response ({}, client {client}); matching plugin may be absent or not permitted; completion unknown, do not replay: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok((request_id, output.stdout))
    }
    fn command(&self) -> Result<std::process::Command> {
        crate::launcher::control_command(&self.session.runtime_name, self.zellij.as_deref())
    }
    fn output(
        &self,
        argv: &[String],
        source: Option<&str>,
        limit: Option<usize>,
    ) -> Result<std::process::Output> {
        let mut command = self.command()?;
        command.arg("action").args(argv);
        // ZELLIJ_PANE_ID is a native targeting hint, never a focus operation.
        command.env_remove("ZELLIJ_PANE_ID");
        if let Some(source) = source {
            command.env(
                "ZELLIJ_PANE_ID",
                source
                    .strip_prefix("terminal_")
                    .context("invalid source pane")?,
            );
        }
        run(command, limit, false)
    }
    pub fn action(&self, argv: Vec<String>) -> Result<Value> {
        self.action_from_optional(None, argv)
    }
    pub fn action_from(&self, source: &str, argv: Vec<String>) -> Result<Value> {
        self.action_from_optional(Some(source), argv)
    }
    fn action_from_optional(&self, source: Option<&str>, argv: Vec<String>) -> Result<Value> {
        let action = argv.first().context("native action is empty")?.as_str();
        let output = self.output(&argv, source, Some(bkpr::MAX_MESSAGE_BYTES))?;
        let stdout =
            String::from_utf8(output.stdout).context("native action output is not UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut receipt = json!({"outcome":if output.status.success() {"dispatched"} else {"failed"},"result":{"stdout":stdout,"stderr":stderr,"exit_status":output.status.code()}});
        if !output.status.success() {
            receipt["error"] = json!({"message":"native CLI action failed; no automatic replay"});
            return Ok(receipt);
        }
        if matches!(action, "dump-layout" | "save-session") {
            receipt["outcome"] = json!("completed");
        }
        if matches!(action, "new-pane" | "new-tab") {
            let id = stdout.trim();
            let verified = if action == "new-pane" {
                let pane = canonical_pane(id)?;
                receipt["result"]["pane_id"] = json!(pane);
                self.list("list-panes").map(|panes| {
                    panes
                        .iter()
                        .any(|entry| native_pane_id(entry).as_deref() == Some(pane))
                })
            } else {
                let tab = id.parse::<u64>().context(
                    "native new-tab returned no exact tab ID; completion unknown, do not replay",
                )?;
                receipt["result"]["tab_id"] = json!(tab);
                self.list("list-tabs").map(|tabs| {
                    tabs.iter()
                        .any(|entry| entry["tab_id"].as_u64() == Some(tab))
                })
            };
            match verified {
                Ok(true) => receipt["outcome"] = json!("completed"),
                Ok(false) => {
                    receipt["outcome"] = json!("unknown");
                    receipt["error"] = json!({"message":"returned creation ID was not observed in native topology; do not replay automatically"});
                }
                Err(error) => {
                    receipt["outcome"] = json!("unknown");
                    receipt["error"] = json!({"message":format!("exact creation ID returned, but observation failed: {error}; do not replay automatically")});
                }
            }
        }
        Ok(receipt)
    }
    fn list(&self, action: &str) -> Result<Vec<Value>> {
        let output = self.output(
            &[action.into(), "--all".into(), "--json".into()],
            None,
            Some(bkpr::MAX_MESSAGE_BYTES),
        )?;
        if !output.status.success() {
            bail!(
                "native {action} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("decode baseline {action} JSON"))
    }
    pub fn clients(&self) -> Result<Vec<Value>> {
        let output = self.output(
            &["list-clients".into()],
            None,
            Some(bkpr::MAX_MESSAGE_BYTES),
        )?;
        if !output.status.success() {
            bail!(
                "native list-clients failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        parse_clients(
            std::str::from_utf8(&output.stdout).context("native client list is not UTF-8")?,
        )
    }
    pub fn stop(&self) -> Result<Value> {
        let command = crate::launcher::control_stop_command(
            &self.session.runtime_name,
            self.zellij.as_deref(),
        )?;
        let output = run(command, Some(bkpr::MAX_MESSAGE_BYTES), false)?;
        Ok(
            json!({"outcome":if output.status.success() {"dispatched"} else {"failed"},"result":{"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"exit_status":output.status.code()}}),
        )
    }
    pub fn snapshot(&self) -> Result<Value> {
        let panes = self.list("list-panes")?;
        let tabs = self.list("list-tabs")?;
        // Lists and publication are separate observations, not an atomic transaction.
        // Never carry a binding through a contradictory native membership observation.
        let session = crate::context::resolve_session(&self.session.session_id)?
            .context("authenticated Bkpr session disappeared during snapshot")?;
        if session.server != self.session.server {
            bail!("native server identity changed during snapshot");
        }
        combine_snapshot(&session, panes, tabs, self.clients()?)
    }
    pub fn read(
        &self,
        pane: &str,
        full: bool,
        ansi: bool,
        tail: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Value> {
        let mut argv = vec!["dump-screen".into(), "--pane-id".into(), pane.into()];
        if full || tail.is_some() || offset.is_some() {
            argv.push("--full".into());
        }
        if ansi {
            argv.push("--ansi".into());
        }
        // Native retained scrollback is the bound. A raw CLI read must never
        // silently truncate it; MCP applies its explicit envelope limit later.
        let output = self.output(&argv, None, None)?;
        let text = String::from_utf8(output.stdout).context("native dump is not UTF-8")?;
        if !output.status.success() {
            return Ok(
                json!({"outcome":"failed","result":{"stdout":text,"exit_status":output.status.code()},"error":{"message":String::from_utf8_lossy(&output.stderr)}}),
            );
        }
        let (selected, total, skipped, count) = select_rows(&text, tail, offset.unwrap_or(0));
        Ok(
            json!({"outcome":"completed","result":{"stdout":selected,"pane_id":pane,"full":full || tail.is_some() || offset.is_some(),"ansi":ansi,"total_rows":total,"offset":skipped,"rows":count,"truncated":false}}),
        )
    }
    /// Stream raw CLI reads without constructing a JSON/string copy. Bottom
    /// ranges retain only their necessary suffix; offset-only reads delay the
    /// newest rows and emit older rows as soon as they become eligible.
    pub fn read_raw(
        &self,
        pane: &str,
        full: bool,
        ansi: bool,
        tail: Option<usize>,
        offset: Option<usize>,
        output: &mut impl std::io::Write,
    ) -> Result<()> {
        let mut command = self.command()?;
        command
            .args(["action", "dump-screen", "--pane-id", pane])
            .env_remove("ZELLIJ_PANE_ID");
        if full || tail.is_some() || offset.is_some() {
            command.arg("--full");
        }
        if ansi {
            command.arg("--ansi");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Ingress(command.spawn().context("start native raw pane read")?);
        let mut stderr = child.0.stderr.take().context("open raw pane read stderr")?;
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            bytes
        });
        let stdout = child.0.stdout.take().context("open raw pane read stdout")?;
        stream_rows(stdout, output, tail, offset.unwrap_or(0))?;
        let status = child.0.wait().context("wait for raw pane read")?;
        let stderr = errors.join().unwrap_or_default();
        if !status.success() {
            bail!(
                "native raw pane read failed ({status}); output may be partial: {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        output.flush().context("flush raw pane read")
    }
    pub fn pane(&self, requested: Option<&str>) -> Result<String> {
        let pane = requested
            .or(self.source.as_deref())
            .context("select an explicit --pane outside the target managed pane")?;
        let id = pane
            .strip_prefix("terminal_")
            .context("pane must be a canonical terminal_N identity")?;
        if id.is_empty()
            || !id.bytes().all(|byte| byte.is_ascii_digit())
            || (id.len() > 1 && id.starts_with('0'))
        {
            bail!("pane must be a canonical terminal_N identity");
        }
        id.parse::<u32>()
            .context("terminal pane identity is out of range")?;
        // The native server validates existence at acceptance. An asynchronous
        // sidebar/context publication must not veto an exact creation receipt.
        Ok(pane.to_owned())
    }
    pub fn workspace(&self, requested: Option<&str>) -> Result<String> {
        if requested.is_none() && self.source.is_none() {
            bail!("outside the target managed pane, select --workspace explicitly");
        }
        self.workspace_in(requested, &self.snapshot()?)
    }
    pub fn workspace_in(&self, requested: Option<&str>, snapshot: &Value) -> Result<String> {
        let tabs = snapshot["tabs"]
            .as_array()
            .context("native snapshot lacks Bkpr tab bindings")?;
        if let Some(workspace) = requested {
            if let Some(entry) = tabs
                .iter()
                .find(|entry| entry["workspace_id"].as_str() == Some(workspace))
            {
                return Ok(entry["workspace_id"]
                    .as_str()
                    .expect("matched string")
                    .to_owned());
            }
            let mut id = None;
            for entry in tabs
                .iter()
                .filter(|entry| entry["workspace_name"].as_str() == Some(workspace))
            {
                let candidate = entry["workspace_id"]
                    .as_str()
                    .context("native workspace binding lacks identity")?;
                if id.is_some_and(|id| id != candidate) {
                    bail!("ambiguous workspace name; use workspace_id");
                }
                id = Some(candidate);
            }
            return id
                .map(str::to_owned)
                .context("workspace does not exist in the live session");
        }
        let pane = self
            .source
            .as_deref()
            .context("outside the target managed pane, select --workspace explicitly")?;
        snapshot["panes"]
            .as_array()
            .and_then(|panes| {
                panes
                    .iter()
                    .find(|entry| entry["pane_id"].as_str() == Some(pane))
            })
            .and_then(|entry| entry["workspace_id"].as_str())
            .map(str::to_owned)
            .context("caller pane no longer has a native workspace binding")
    }
    pub fn tab(
        &self,
        requested: Option<u64>,
        workspace: Option<&str>,
        snapshot: &Value,
    ) -> Result<u64> {
        let tabs = snapshot["tabs"]
            .as_array()
            .context("native snapshot lacks Bkpr tab bindings")?;
        if let Some(tab) = requested {
            let binding = tabs
                .iter()
                .find(|entry| entry["tab_id"].as_u64() == Some(tab))
                .context("tab does not exist in the live session")?;
            if let Some(workspace) = workspace
                && binding["workspace_id"].as_str()
                    != Some(self.workspace_in(Some(workspace), snapshot)?.as_str())
            {
                bail!("tab and workspace selectors disagree");
            }
            return Ok(tab);
        }
        if workspace.is_none()
            && let Some(source) = self.source.as_deref()
        {
            let tab = snapshot["panes"]
                .as_array()
                .and_then(|panes| {
                    panes
                        .iter()
                        .find(|entry| entry["pane_id"].as_str() == Some(source))
                })
                .and_then(|entry| entry["tab_id"].as_u64())
                .context("caller pane has no live native tab binding")?;
            if !tabs
                .iter()
                .any(|entry| entry["tab_id"].as_u64() == Some(tab))
            {
                bail!("caller pane's native tab disappeared");
            }
            return Ok(tab);
        }
        let workspace = self.workspace_in(workspace, snapshot)?;
        let mut matches = tabs
            .iter()
            .filter(|entry| entry["workspace_id"].as_str() == Some(workspace.as_str()));
        let tab = matches
            .next()
            .and_then(|entry| entry["tab_id"].as_u64())
            .context("workspace has no native tab")?;
        if matches.next().is_some() {
            bail!("workspace contains multiple tabs; select --tab explicitly");
        }
        Ok(tab)
    }
}
fn run(
    mut command: std::process::Command,
    limit: Option<usize>,
    bridge: bool,
) -> Result<std::process::Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Ingress(command.spawn().context("start baseline native CLI")?);
    let mut stderr = child.0.stderr.take().context("open native stderr")?;
    let error_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut bytes);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        bytes
    });
    let mut stdout = child.0.stdout.take().context("open native stdout")?;
    let mut bytes = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    if bridge {
        use std::os::fd::AsRawFd;
        let mut buffer = [0u8; 8192];
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let mut fd = libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe {
                libc::poll(
                    &mut fd,
                    1,
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error).context("poll owner bridge; completion unknown, do not replay");
            }
            if ready == 0 || remaining.is_zero() {
                let _ = child.0.kill();
                let _ = child.0.wait();
                let _ = error_reader.join();
                bail!(
                    "Bkpr owner bridge did not finish within 10 seconds; completion unknown, do not replay"
                );
            }
            let count = stdout
                .read(&mut buffer)
                .context("read owner bridge; completion unknown, do not replay")?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if limit.is_some_and(|limit| bytes.len() > limit) {
                break;
            }
        }
    } else {
        stdout
            .by_ref()
            .take(limit.map_or(u64::MAX, |limit| limit as u64 + 1))
            .read_to_end(&mut bytes)
            .context("read native output; completion unknown, do not replay")?;
    }
    if limit.is_some_and(|limit| bytes.len() > limit) {
        let _ = child.0.kill();
        let _ = child.0.wait();
        let _ = error_reader.join();
        bail!("native output exceeds response limit; completion unknown, do not replay");
    }
    let status = if bridge {
        loop {
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.0.kill();
                let _ = child.0.wait();
                let _ = error_reader.join();
                bail!(
                    "owner bridge process did not finish within 10 seconds; completion unknown, do not replay"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    } else {
        child.0.wait()?
    };
    Ok(std::process::Output {
        status,
        stdout: bytes,
        stderr: error_reader.join().unwrap_or_default(),
    })
}

fn canonical_pane(pane: &str) -> Result<&str> {
    let id = pane.strip_prefix("terminal_").context(
        "native creation did not return an exact terminal_N ID; completion unknown, do not replay",
    )?;
    if id.is_empty()
        || !id.bytes().all(|byte| byte.is_ascii_digit())
        || (id.len() > 1 && id.starts_with('0'))
    {
        bail!("invalid exact native pane ID; completion unknown, do not replay");
    }
    id.parse::<u32>()
        .context("native pane ID is out of range")?;
    Ok(pane)
}
fn native_pane_id(entry: &Value) -> Option<String> {
    Some(format!(
        "{}_{}",
        if entry["is_plugin"].as_bool()? {
            "plugin"
        } else {
            "terminal"
        },
        entry["id"].as_u64()?
    ))
}
fn owner_receiver(bytes: &[u8], request_id: &str, clients: &[Value]) -> Result<(u16, u32)> {
    let text = std::str::from_utf8(bytes)
        .context("owner discovery output is not UTF-8; no mutation was dispatched")?;
    let mut ready = std::collections::BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let receipt = decode(line.as_bytes(), request_id)
            .context("uncorrelated owner discovery output; no mutation was dispatched")?;
        if receipt["outcome"] != "completed" {
            bail!("owner discovery did not complete; no mutation was dispatched");
        }
        let value = result(&receipt)?;
        let client = value["client_id"]
            .as_u64()
            .and_then(|id| u16::try_from(id).ok())
            .context("owner discovery lacks a valid replica client identity")?;
        let plugin = value["owner_plugin_id"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .context("owner discovery lacks an exact native plugin identity")?;
        let attached = clients
            .iter()
            .any(|entry| entry["client_id"].as_u64() == Some(u64::from(client)));
        // Global SDK mutations are not client-view operations. A ready owner
        // replica can remain usable after its original attachment departs.
        ready.insert((!attached, client, plugin));
    }
    ready
        .first()
        .map(|(_, client, plugin)| (*client, *plugin))
        .context("no ready canonical Bkpr owner replied; no mutation was dispatched")
}

fn sidebar_receiver(panes: &[Value], client: &Value, plugin_url: &str) -> Result<u32> {
    let current = client["pane_id"]
        .as_str()
        .context("native client lacks its exact pane ID")?;
    let tab = panes
        .iter()
        .find(|pane| native_pane_id(pane).as_deref() == Some(current))
        .and_then(|pane| pane["tab_id"].as_u64())
        .context("selected client's native tab disappeared before sidebar selection")?;
    let mut candidates = panes.iter().filter(|pane| {
        pane["tab_id"].as_u64() == Some(tab)
            && pane["is_plugin"] == true
            && pane["is_suppressed"] == false
            && pane["is_floating"] == false
            && pane["plugin_url"].as_str() == Some(plugin_url)
    });
    let receiver = candidates
        .next()
        .and_then(|pane| pane["id"].as_u64())
        .and_then(|id| u32::try_from(id).ok())
        .context("selected client's tab has no matching Bkpr sidebar; no request was dispatched")?;
    if candidates.next().is_some() {
        bail!(
            "selected client's tab has ambiguous Bkpr sidebar candidates; no request was dispatched"
        );
    }
    Ok(receiver)
}

fn parse_clients(text: &str) -> Result<Vec<Value>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND") {
        bail!("unrecognized baseline list-clients header");
    }
    let mut clients = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let line = line.trim_start();
        let (id, rest) = line
            .split_once(char::is_whitespace)
            .context("invalid native client row")?;
        let rest = rest.trim_start();
        let (pane, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let id: u16 = id.parse().context("invalid native client ID")?;
        let (kind, number) = pane
            .split_once('_')
            .context("invalid native client pane ID")?;
        if !matches!(kind, "terminal" | "plugin") || number.parse::<u32>().is_err() {
            bail!("invalid native client pane ID");
        }
        if clients
            .iter()
            .any(|client: &Value| client["client_id"].as_u64() == Some(u64::from(id)))
        {
            bail!("duplicate native client ID");
        }
        clients.push(json!({"client_id":id,"pane_id":pane,"running_command":command.trim()}));
    }
    Ok(clients)
}
fn combine_snapshot(
    session: &LiveSession,
    mut panes: Vec<Value>,
    tabs: Vec<Value>,
    clients: Vec<Value>,
) -> Result<Value> {
    let mut pane_bindings = Vec::new();
    for pane in &mut panes {
        let id = native_pane_id(pane).context("native pane list lacks canonical identity")?;
        pane["pane_id"] = json!(id);
        if pane["is_plugin"] == true {
            continue;
        }
        let mut binding = json!({"pane_id":id,"tab_id":pane["tab_id"]});
        if let Some(published) = session
            .panes
            .iter()
            .find(|published| published.pane_id == id)
        {
            binding["workspace_id"] = json!(published.workspace_id);
            binding["pane_name"] = json!(published.pane_name);
            binding["agent_id"] = json!(published.agent_id);
            if let Some(workspace) = session
                .workspaces
                .iter()
                .find(|workspace| workspace.id == published.workspace_id)
            {
                binding["workspace_name"] = json!(workspace.name);
            }
        }
        pane_bindings.push(binding);
    }
    let mut tab_bindings = Vec::new();
    let mut conflicting_tabs = std::collections::BTreeSet::new();
    for tab in &tabs {
        let id = tab["tab_id"]
            .as_u64()
            .context("native tab list lacks stable tab_id")?;
        let mut binding = json!({"tab_id":id,"name":tab["name"]});
        let workspaces: std::collections::BTreeSet<&str> = pane_bindings
            .iter()
            .filter(|pane| pane["tab_id"].as_u64() == Some(id))
            .filter_map(|pane| pane["workspace_id"].as_str())
            .collect();
        if workspaces.len() == 1 {
            let workspace = workspaces.first().expect("one workspace");
            binding["workspace_id"] = json!(workspace);
            if let Some(published) = session
                .workspaces
                .iter()
                .find(|published| published.id == *workspace)
            {
                binding["workspace_name"] = json!(published.name);
            }
        } else if workspaces.len() > 1 {
            conflicting_tabs.insert(id);
            binding["binding_diagnostic"] =
                json!("conflicting authenticated pane bindings; publication may be catching up");
        }
        tab_bindings.push(binding);
    }
    for pane in &mut pane_bindings {
        let tab = pane["tab_id"].as_u64();
        if tab.is_none_or(|id| {
            conflicting_tabs.contains(&id)
                || !tabs.iter().any(|tab| tab["tab_id"].as_u64() == Some(id))
        }) {
            pane.as_object_mut()
                .expect("pane binding")
                .retain(|key, _| key == "pane_id" || key == "tab_id");
            pane["binding_diagnostic"] = json!(
                "native membership and authenticated publication disagree; no workspace inferred"
            );
        }
    }
    Ok(
        json!({"session_id":session.session_id,"session_name":session.session_name,"session_key":session.runtime_name,"tabs":tab_bindings,"panes":pane_bindings,"native":{"panes":panes,"tabs":tabs,"clients":clients},"consistency":"separate native and authenticated publication observations; missing bindings are not guessed"}),
    )
}

fn stream_rows(
    mut input: impl Read,
    output: &mut impl std::io::Write,
    tail: Option<usize>,
    offset: usize,
) -> Result<()> {
    use std::io::BufRead;
    if tail.is_none() && offset == 0 {
        let mut buffer = [0u8; 8192];
        loop {
            let count = input.read(&mut buffer).context("read native pane stream")?;
            if count == 0 {
                return Ok(());
            }
            output
                .write_all(&buffer[..count])
                .context("write raw pane stream")?;
            output.flush().context("flush raw pane stream")?;
        }
    }
    if tail == Some(0) {
        std::io::copy(&mut input, &mut std::io::sink()).context("drain zero-row pane read")?;
        return Ok(());
    }
    let mut input = std::io::BufReader::new(input);
    let mut retained = std::collections::VecDeque::new();
    let maximum = tail.map_or(offset, |tail| tail.saturating_add(offset));
    let mut row = Vec::new();
    loop {
        if input
            .read_until(b'\n', &mut row)
            .context("read native pane row")?
            == 0
        {
            break;
        }
        retained.push_back(std::mem::take(&mut row));
        if retained.len() > maximum {
            row = retained.pop_front().expect("retained row");
            if tail.is_none() {
                output.write_all(&row).context("write offset pane row")?;
                output.flush().context("flush offset pane row")?;
            }
            row.clear();
        }
    }
    if tail.is_some() {
        for row in retained.iter().take(retained.len().saturating_sub(offset)) {
            output
                .write_all(row)
                .context("write bottom-relative pane row")?;
        }
    }
    output.flush().context("flush bottom-relative pane read")
}

/// Slice native dump rows, retaining selected LF, CR and ANSI bytes. A final LF
/// terminates a row; it is not an additional empty row.
fn select_rows(text: &str, tail: Option<usize>, offset: usize) -> (&str, usize, usize, usize) {
    let total = text.split_inclusive('\n').count();
    let skipped = offset.min(total);
    let end = total - skipped;
    let start = end.saturating_sub(tail.unwrap_or(end));
    if start == 0 && end == total {
        return (text, total, skipped, total);
    }
    let mut rows = text.split_inclusive('\n');
    let begin_byte: usize = rows.by_ref().take(start).map(str::len).sum();
    let byte_len: usize = rows.take(end - start).map(str::len).sum();
    (
        &text[begin_byte..begin_byte + byte_len],
        total,
        skipped,
        end - start,
    )
}
pub(super) fn result(receipt: &Value) -> Result<&Value> {
    if receipt["outcome"] == "unknown" {
        bail!(
            "native completion unknown; do not replay automatically: {}",
            receipt["error"]
        );
    }
    if receipt["outcome"] == "failed" {
        bail!("native control failed: {}", receipt["error"]);
    }
    receipt
        .get("result")
        .filter(|value| !value.is_null())
        .context("native completion has no result")
}
fn decode(bytes: &[u8], request_id: &str) -> Result<Value> {
    let response: Value = serde_json::from_slice(bytes)
        .context("invalid native response; completion unknown, do not replay")?;
    if response["request_id"].as_str() != Some(request_id) {
        bail!("uncorrelated native response; completion unknown, do not replay");
    }
    match response["outcome"].as_str() {
        Some("completed" | "dispatched" | "unknown" | "failed") => Ok(response),
        _ => bail!("invalid native outcome; completion unknown, do not replay"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_context_rejects_stale_membership_and_foreign_targets_do_not_borrow_source() {
        use bkpr::context_contract::{ContextPane, ContextWorkspace, PaneContext};
        let session = crate::context::test_support::Session::new("control-authority").unwrap();
        let _guard = session.guard();
        let workspaces = vec![
            ContextWorkspace {
                id: "workspace-a".into(),
                name: "A".into(),
            },
            ContextWorkspace {
                id: "workspace-b".into(),
                name: "B".into(),
            },
        ];
        let pane = ContextPane {
            pane_id: "terminal_1".into(),
            workspace_id: "workspace-a".into(),
            pane_name: "shell".into(),
            agent_id: Some("agent-a".into()),
        };
        session
            .publish(1, workspaces.clone(), vec![pane.clone()])
            .unwrap();
        let context = PaneContext {
            session_id: session.session_id().into(),
            session_name: "control-authority".into(),
            workspace_id: "workspace-a".into(),
            workspace_name: "A".into(),
            pane_id: "terminal_1".into(),
            pane_name: "shell".into(),
        };
        let caller = Caller::Agent {
            agent_id: "agent-a".into(),
            context,
        };
        let resolved = Native::resolve(None, &caller, None).unwrap();
        assert_eq!(resolved.source.as_deref(), Some("terminal_1"));
        assert!(Native::resolve(Some("another-session"), &caller, None).is_err());
        let snapshot = json!({"tabs":[{"tab_id":1,"workspace_id":"workspace-a","workspace_name":"A"},
            {"tab_id":2,"workspace_id":"workspace-a","workspace_name":"A"}],
            "panes":[{"pane_id":"terminal_1","tab_id":1,"workspace_id":"workspace-a"}]});
        assert_eq!(resolved.tab(None, None, &snapshot).unwrap(), 1);
        assert!(resolved.tab(None, Some("workspace-a"), &snapshot).is_err());
        assert_eq!(resolved.tab(Some(1), None, &snapshot).unwrap(), 1);
        assert!(
            resolved
                .tab(Some(1), Some("workspace-b"), &snapshot)
                .is_err()
        );
        let native_only = json!({"tabs":[{"tab_id":9,"workspace_id":"workspace-native","workspace_name":"Native"}],
            "panes":[{"pane_id":"terminal_1","tab_id":9,"workspace_id":"workspace-native"}]});
        assert_eq!(
            resolved.workspace_in(None, &native_only).unwrap(),
            "workspace-native"
        );
        assert_eq!(
            resolved.workspace_in(Some("Native"), &native_only).unwrap(),
            "workspace-native"
        );
        assert_eq!(resolved.tab(None, None, &native_only).unwrap(), 9);
        assert_eq!(resolved.pane(Some("terminal_999")).unwrap(), "terminal_999");
        assert!(resolved.pane(Some("plugin_1")).is_err());
        assert!(resolved.pane(Some("terminal_01")).is_err());
        let foreign =
            crate::context::test_support::Session::new_in(session.root(), "foreign-control")
                .unwrap();
        foreign
            .publish(
                1,
                vec![ContextWorkspace {
                    id: "foreign-workspace".into(),
                    name: "Foreign".into(),
                }],
                vec![ContextPane {
                    pane_id: "terminal_2".into(),
                    workspace_id: "foreign-workspace".into(),
                    pane_name: "foreign shell".into(),
                    agent_id: None,
                }],
            )
            .unwrap();
        let foreign_host = Native::resolve(Some(foreign.session_id()), &caller, None).unwrap();
        assert!(foreign_host.source.is_none());
        assert!(foreign_host.pane(None).is_err());
        assert!(foreign_host.workspace(None).is_err());
        assert_eq!(foreign_host.pane(Some("terminal_2")).unwrap(), "terminal_2");
        let current = super::super::dispatch(
            json!({"operation":"session.list","arguments":{}}),
            caller.clone(),
            false,
        )
        .unwrap();
        assert_eq!(current["sessions"].as_array().unwrap().len(), 1);
        let all = super::super::dispatch(
            json!({"operation":"session.list","arguments":{"all":true}}),
            caller.clone(),
            false,
        )
        .unwrap();
        assert_eq!(all["sessions"].as_array().unwrap().len(), 2);
        let mut moved = pane;
        moved.workspace_id = "workspace-b".into();
        session.publish(2, workspaces, vec![moved]).unwrap();
        assert!(Native::resolve(None, &caller, None).is_err());
        let serialized = serde_json::to_value(crate::context::live_sessions().unwrap()).unwrap();
        assert!(serialized[0].get("token").is_none());
        assert!(serialized[0].get("owner_token").is_none());
    }
    #[test]
    fn owner_envelopes_require_correlation_and_preserve_dispatch() {
        assert!(
            decode(
                br#"{"request_id":"other","outcome":"dispatched","result":{}}"#,
                "ours"
            )
            .is_err()
        );
        let dispatched = decode(br#"{"request_id":"ours","outcome":"dispatched","result":{"receipt":"SDK dispatch"},"error":null}"#, "ours").unwrap();
        assert_eq!(dispatched["outcome"], "dispatched");
        let failed = decode(
            br#"{"request_id":"ours","outcome":"failed","result":null,"error":"pane missing"}"#,
            "ours",
        )
        .unwrap();
        assert!(result(&failed).is_err());
    }

    #[test]
    fn read_consumer_preserves_large_full_dump_and_bottom_relative_boundaries() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let dump = directory.path().join("dump");
        let executable = directory.path().join("native-fixture");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nexec cat '{}'\n", dump.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let host = Native {
            session: LiveSession {
                session_id: "a".repeat(48),
                session_name: "read-consumer".into(),
                runtime_name: "read-consumer".into(),
                server: std::process::id() as i32,
                workspaces: vec![],
                panes: vec![],
            },
            source: None,
            zellij: Some(executable),
        };
        let large = format!("{}\n雪\n", "x".repeat(bkpr::MAX_MESSAGE_BYTES + 64));
        std::fs::write(&dump, &large).unwrap();
        let response = host.read("terminal_1", true, false, None, None).unwrap();
        assert_eq!(response["result"]["stdout"].as_str(), Some(large.as_str()));
        assert_eq!(response["result"]["truncated"], false);
        assert!(super::super::bound_read_response(&response, bkpr::MAX_MESSAGE_BYTES).is_err());
        let ranged = host
            .read("terminal_1", false, false, Some(1), Some(0))
            .unwrap();
        assert_eq!(ranged["result"]["stdout"], "雪\n");
        assert!(super::super::bound_read_response(&ranged, bkpr::MAX_MESSAGE_BYTES).is_ok());

        let text = "a\n\n雪\r\n\u{1b}[31mred\u{1b}[0m\nlast";
        std::fs::write(&dump, text).unwrap();
        for (tail, offset, expected, rows) in [
            (None, None, text, 5),
            (Some(2), Some(1), "雪\r\n\u{1b}[31mred\u{1b}[0m\n", 2),
            (Some(1), None, "last", 1),
            (Some(0), None, "", 0),
            (Some(99), Some(99), "", 0),
            (None, Some(3), "a\n\n", 2),
        ] {
            let response = host.read("terminal_1", false, true, tail, offset).unwrap();
            assert_eq!(response["result"]["stdout"], expected);
            assert_eq!(response["result"]["rows"], rows);
            assert_eq!(response["result"]["total_rows"], 5);
            let mut raw = Vec::new();
            host.read_raw("terminal_1", false, true, tail, offset, &mut raw)
                .unwrap();
            assert_eq!(raw, expected.as_bytes());
        }
        for text in ["", "\n", "a\n", "a\n\n", "雪"] {
            assert_eq!(select_rows(text, None, 0).0, text);
            assert_eq!(select_rows(text, Some(0), 0).0, "");
            assert_eq!(select_rows(text, Some(usize::MAX), usize::MAX).0, "");
        }
    }

    #[test]
    fn raw_read_delivers_stdout_before_native_eof() {
        use std::os::unix::fs::PermissionsExt;
        struct Observed {
            bytes: Vec<u8>,
            first: Option<std::sync::mpsc::Sender<Vec<u8>>>,
        }
        impl std::io::Write for Observed {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                if let Some(first) = self.first.take() {
                    let _ = first.send(bytes.to_vec());
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let release = directory.path().join("release");
        let executable = directory.path().join("native-fixture");
        std::fs::write(&executable, format!("#!/bin/sh\nprintf 'before\\n'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf 'after\\n'\n", release.display())).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let host = Native {
            session: LiveSession {
                session_id: "a".repeat(48),
                session_name: "stream-consumer".into(),
                runtime_name: "stream-consumer".into(),
                server: std::process::id() as i32,
                workspaces: vec![],
                panes: vec![],
            },
            source: None,
            zellij: Some(executable),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut output = Observed {
                bytes: vec![],
                first: Some(sender),
            };
            host.read_raw("terminal_1", true, false, None, None, &mut output)
                .map(|()| output.bytes)
        });
        // Release the fixture even if delivery times out, so a failing regression
        // cannot leak a blocked child or hang the test suite.
        let before_eof = receiver.recv_timeout(std::time::Duration::from_secs(3));
        std::fs::write(&release, b"").unwrap();
        let all = worker.join().unwrap().unwrap();
        assert_eq!(before_eof.unwrap(), b"before\n");
        assert_eq!(all, b"before\nafter\n");
    }

    #[test]
    fn parses_baseline_clients_without_losing_command_spaces() {
        let clients = parse_clients("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1         terminal_4     /bin/sh -l     \n12        plugin_2       builtin status\n").unwrap();
        assert_eq!(
            clients[0],
            json!({"client_id":1,"pane_id":"terminal_4","running_command":"/bin/sh -l"})
        );
        assert_eq!(clients[1]["client_id"], 12);
        assert!(
            parse_clients(
                "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1 terminal_1 sh\n1 terminal_2 sh"
            )
            .is_err()
        );
    }

    #[test]
    fn owner_discovery_pins_a_ready_replica_preferring_attached_without_guessing() {
        let a = json!({"request_id":"ours","outcome":"completed","result":{"owner_plugin_id":0,"client_id":1}}).to_string();
        let b = json!({"request_id":"ours","outcome":"completed","result":{"owner_plugin_id":0,"client_id":2}}).to_string();
        let output = format!("{a}\n{b}\n");
        assert_eq!(
            owner_receiver(output.as_bytes(), "ours", &[json!({"client_id":2})]).unwrap(),
            (2, 0)
        );
        assert_eq!(owner_receiver(a.as_bytes(), "ours", &[]).unwrap(), (1, 0));
        assert!(owner_receiver(a.as_bytes(), "another", &[]).is_err());
        assert!(owner_receiver(b"\n", "ours", &[]).is_err());
    }

    #[test]
    fn client_receiver_uses_exact_tab_and_authenticated_plugin_url_without_fallback() {
        let client = json!({"client_id":2,"pane_id":"terminal_9"});
        let terminal = json!({"id":9,"is_plugin":false,"tab_id":3});
        let sidebar = json!({"id":7,"is_plugin":true,"tab_id":3,"is_floating":false,"is_selectable":false,"is_suppressed":false,"plugin_url":"file:/private/bkpr.wasm"});
        let owner = json!({"id":0,"is_plugin":true,"tab_id":3,"is_floating":true,"is_selectable":true,"is_suppressed":true,"plugin_url":"file:/private/bkpr.wasm"});
        let foreign = json!({"id":8,"is_plugin":true,"tab_id":4,"is_floating":false,"is_selectable":false,"is_suppressed":false,"plugin_url":"file:/private/bkpr.wasm"});
        assert_eq!(
            sidebar_receiver(
                &[terminal.clone(), sidebar.clone(), owner, foreign],
                &client,
                "file:/private/bkpr.wasm"
            )
            .unwrap(),
            7
        );
        // A sidebar awaiting normal permission approval is deliberately
        // selectable; client.ready must be able to address that exact replica.
        let mut ungranted = sidebar.clone();
        ungranted["is_selectable"] = json!(true);
        assert_eq!(
            sidebar_receiver(
                &[terminal.clone(), ungranted.clone()],
                &client,
                "file:/private/bkpr.wasm"
            )
            .unwrap(),
            7
        );
        ungranted["is_floating"] = json!(true);
        assert!(
            sidebar_receiver(
                &[terminal.clone(), ungranted],
                &client,
                "file:/private/bkpr.wasm"
            )
            .is_err()
        );
        assert!(
            sidebar_receiver(
                &[terminal.clone(), sidebar.clone()],
                &client,
                "file:/other.wasm"
            )
            .is_err()
        );
        let mut duplicate = sidebar.clone();
        duplicate["id"] = json!(11);
        assert!(
            sidebar_receiver(
                &[terminal.clone(), sidebar, duplicate],
                &client,
                "file:/private/bkpr.wasm"
            )
            .is_err()
        );
        assert!(sidebar_receiver(&[terminal], &client, "file:/private/bkpr.wasm").is_err());
    }

    #[test]
    fn snapshots_keep_fresh_native_ids_without_guessing_or_conflicting_bindings() {
        use bkpr::context_contract::{ContextPane, ContextWorkspace};
        let session = LiveSession {
            session_id: "a".repeat(48),
            session_name: "snapshot".into(),
            runtime_name: "snapshot".into(),
            server: 1,
            workspaces: vec![
                ContextWorkspace {
                    id: "a".into(),
                    name: "A".into(),
                },
                ContextWorkspace {
                    id: "b".into(),
                    name: "B".into(),
                },
            ],
            panes: vec![
                ContextPane {
                    pane_id: "terminal_1".into(),
                    workspace_id: "a".into(),
                    pane_name: "one".into(),
                    agent_id: None,
                },
                ContextPane {
                    pane_id: "terminal_2".into(),
                    workspace_id: "b".into(),
                    pane_name: "two".into(),
                    agent_id: None,
                },
            ],
        };
        let panes = vec![
            json!({"id":1,"is_plugin":false,"tab_id":7}),
            json!({"id":2,"is_plugin":false,"tab_id":7}),
            json!({"id":99,"is_plugin":false,"tab_id":8}),
        ];
        let snapshot = combine_snapshot(
            &session,
            panes,
            vec![
                json!({"tab_id":7,"name":"mixed"}),
                json!({"tab_id":8,"name":"new"}),
            ],
            vec![],
        )
        .unwrap();
        assert_eq!(snapshot["panes"][2]["pane_id"], "terminal_99");
        assert!(snapshot["panes"][2].get("workspace_id").is_none());
        assert!(snapshot["panes"][0].get("workspace_id").is_none());
        assert!(snapshot["panes"][1].get("workspace_id").is_none());
        assert!(snapshot["tabs"][0].get("workspace_id").is_none());
        assert!(snapshot["tabs"][0]["binding_diagnostic"].is_string());
    }
}
