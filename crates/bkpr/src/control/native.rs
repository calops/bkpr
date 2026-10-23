use super::Caller;
use crate::context::LiveSession;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST: AtomicU64 = AtomicU64::new(1);
// Native control protocol v1 bounds the complete ingress frame, including its newline.
const MAX_NATIVE_REQUEST_BYTES: usize = 1024 * 1024;

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
    pub fn call(&self, method: &str, arguments: Value) -> Result<Value> {
        self.call_from(method, arguments, self.source.as_deref())
    }
    fn call_from(&self, method: &str, arguments: Value, source: Option<&str>) -> Result<Value> {
        let request_id = format!(
            "{}-{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        let request = json!({"protocol":1,"request_id":request_id,"session_id":self.session.session_id,"source_pane_id":source,"client_id":null,"method":method,"arguments":arguments});
        let mut bytes = serde_json::to_vec(&request)?;
        if bytes.len() >= MAX_NATIVE_REQUEST_BYTES {
            bail!("native control request exceeds the protocol's 1 MiB frame limit");
        }
        bytes.push(b'\n');
        let mut command =
            crate::launcher::control_command(&self.session.runtime_name, self.zellij.as_deref())?;
        command
            .arg("bkpr-control")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Ingress(command.spawn().context(
            "start native control ingress; this runtime may predate control capabilities",
        )?);
        let mut input = child.0.stdin.take().context("open native control stdin")?;
        input.write_all(&bytes).context("write native control request: runtime may not support control, or completion is unknown; do not replay automatically")?;
        drop(input);
        let mut stdout = child
            .0
            .stdout
            .take()
            .context("open native control stdout")?;
        let mut stderr = child
            .0
            .stderr
            .take()
            .context("open native control stderr")?;
        let error_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            bytes
        });
        let mut output = Vec::new();
        stdout
            .by_ref()
            .take(bkpr::MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_end(&mut output)
            .context(
                "read native control response; completion unknown, do not replay automatically",
            )?;
        if output.len() > bkpr::MAX_MESSAGE_BYTES {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let _ = error_reader.join();
            bail!("native control response exceeds size limit; completion unknown, do not replay");
        }
        let status = child.0.wait()?;
        let stderr = error_reader.join().unwrap_or_default();
        if output.is_empty() {
            bail!(
                "native control unavailable or completion unknown ({status}): {}; do not replay automatically",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        decode(&output, &request_id)
    }
    pub fn action(&self, argv: Vec<String>) -> Result<Value> {
        self.call("action", json!({"argv":argv}))
    }
    pub fn action_from(&self, source: &str, argv: Vec<String>) -> Result<Value> {
        self.call_from("action", json!({"argv":argv}), Some(source))
    }
    pub fn snapshot(&self) -> Result<Value> {
        let mut receipt = self.call("snapshot", json!({}))?;
        result(&receipt)?;
        Ok(receipt["result"].take())
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
    if response["protocol"] != 1 || response["request_id"].as_str() != Some(request_id) {
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
    fn refuses_uncorrelated_and_preserves_unknown_receipts() {
        assert!(
            decode(
                br#"{"protocol":1,"request_id":"other","outcome":"completed","result":{}}"#,
                "ours"
            )
            .is_err()
        );
        let unknown = decode(
            br#"{"protocol":1,"request_id":"ours","outcome":"unknown","error":{"code":"timeout"}}"#,
            "ours",
        )
        .unwrap();
        assert_eq!(unknown["outcome"], "unknown");
        assert_eq!(unknown["error"]["code"], "timeout");
    }
    #[test]
    fn preserves_native_failure_diagnostics_without_converting_to_success() {
        let failed = decode(br#"{"protocol":1,"request_id":"ours","outcome":"failed","result":{"exit_status":1,"stdout":"pane missing"},"error":{"code":"action_failed","message":"pane missing"}}"#, "ours").unwrap();
        assert_eq!(failed["outcome"], "failed");
        assert_eq!(failed["result"]["exit_status"], 1);
        assert_eq!(failed["result"]["stdout"], "pane missing");
        assert!(result(&failed).is_err());
    }
    #[test]
    fn preserves_dispatched_instead_of_claiming_completion() {
        let result = decode(br#"{"protocol":1,"request_id":"ours","outcome":"dispatched","result":{"pane_id":"terminal_7"}}"#, "ours").unwrap();
        assert_eq!(result["outcome"], "dispatched");
    }
}
