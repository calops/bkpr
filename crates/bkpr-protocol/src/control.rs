//! Discoverable CLI/MCP vocabulary mapped to the existing native protocol.
//! This is deliberately not an arbitrary Operation deserializer.
use crate::{
    HarnessRecovery, KeyEvent, LayoutNode, Operation, PaneRequest, PaneSpawnEntry, PaneSpawnSpec,
    SpawnLayoutNode,
};
use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub enum ControlAction {
    Local(Value),
    Native(Operation),
}

/// Plain viewport export, never scrollback recovery or image pixel transport.
#[derive(Serialize, JsonSchema)]
pub struct TerminalReadResult {
    pub pane_id: String,
    pub pid: Option<u32>,
    pub revision: u64,
    pub lifecycle: String,
    pub exit_code: Option<u32>,
    pub error: Option<String>,
    pub size: Option<crate::Size>,
    pub alternate_screen: Option<bool>,
    pub cursor: Option<TerminalCursor>,
    /// None for pane.inspect; pane.read exports native canonical cell text.
    pub text: Option<String>,
    pub images_omitted: bool,
}
#[derive(Serialize, JsonSchema)]
pub struct TerminalCursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SchemaArguments {
    operation: Option<String>,
}

// An empty view ID is an internal request for an owned, short-lived headless
// view. The authenticated dispatcher rejects every supplied native view ID.
fn pane(pane_id: String, request: PaneRequest) -> Operation {
    Operation::Pane { pane_id, request }
}

macro_rules! catalog {
    ($( $name:literal => $ty:ident { $( $field:ident : $kind:ty ),* $(,)? }, $input:literal, $description:literal, |$args:ident| $mapping:expr; )*) => {
        $(
            #[derive(Deserialize, JsonSchema)]
            #[serde(deny_unknown_fields)]
            struct $ty { $( $field: $kind, )* }
        )*
        pub fn is_input(operation: &str) -> bool {
            match operation { $( $name => $input, )* _ => false }
        }
        fn inventory() -> Value {
            json!([
                {"operation":"api.help","description":"List supported operations and native capability limits","input":false},
                {"operation":"api.schema","description":"Generate argument schemas from the same types used for decoding","input":false},
                $( {"operation":$name,"description":$description,"input":$input}, )*
            ])
        }
        fn schema(operation: &str) -> Result<Value> {
            let arguments = match operation {
                "api.help" => serde_json::to_value(schemars::schema_for!(Empty))?,
                "api.schema" => serde_json::to_value(schemars::schema_for!(SchemaArguments))?,
                $( $name => serde_json::to_value(schemars::schema_for!($ty))?, )*
                _ => bail!("unsupported control operation: {operation}"),
            };
            Ok(json!({
                "operation":operation,"input":is_input(operation),"arguments":arguments,
                "result":result_schema(operation),
                "semantics":{
                    "native_acceptance":"admitted/completed by the native backend; not model processing, prompt readiness or process success",
                    "input_acceptance":"queued once without replay; write_id may be null; no implicit Enter",
                    "failure":"a transport failure after dispatch may have unknown side effects; never retry mutations or input automatically"
                }
            }))
        }
        pub fn resolve(operation: &str, arguments: Value) -> Result<ControlAction> {
            ensure!(arguments.is_object(), "control arguments must be an object");
            let native = match operation {
                "api.help" => {
                    let _: Empty = serde_json::from_value(arguments)?;
                    return Ok(ControlAction::Local(json!({
                        "operations":inventory(),
                        "scope":{"default":"workspace","supported":["workspace","session","all"],"all":"current daemon only"},
                        "capabilities":{"atomic_pane_batch":true,"terminal_read":"complete plain UTF-8 canonical visible viewport, no history or image pixels; existing native response bounds apply","input":"separate terminal_input tool; no implicit Enter or replay","temporary_views":"owned, noninteractive, no size/focus/clipboard authority","cross_daemon":false,"repository_scope":false},
                        "notes":["expected_revision is the workspace revision for workspace mutations/window.create, and window revision for window mutations, pane.create/reopen and layout.set","pane.create uses local references and a complete final layout; Existing nodes must belong to its target window","workspace.create and unowned pane.create require explicit session/all scope","pane.close terminates the execution; remove its layout references before pane.forget","input.* never serves as peer-agent message delivery"]
                    })));
                }
                "api.schema" => {
                    let args: SchemaArguments = serde_json::from_value(arguments)?;
                    return Ok(ControlAction::Local(match args.operation {
                        Some(operation) => schema(&operation)?,
                        None => {
                            let operations = inventory().as_array().unwrap().iter()
                                .map(|item| schema(item["operation"].as_str().unwrap()))
                                .collect::<Result<Vec<_>>>()?;
                            json!({"operations":operations})
                        }
                    }));
                }
                $( $name => {
                    let $args: $ty = serde_json::from_value(arguments).with_context(|| format!("invalid {operation} arguments"))?;
                    $mapping
                }, )*
                _ => bail!("unsupported control operation: {operation}"),
            };
            validate_native(&native)?;
            Ok(ControlAction::Native(native))
        }
    }
}

catalog! {
    "session.inspect" => InspectArguments {}, false, "Inspect registry in the selected daemon-local scope", |_a| Operation::Inspect;
    "project.ci.refresh" => CiRefreshArguments { target:crate::CiRefreshTarget }, false, "Queue CI refresh for an exact retained pane project source; never fetch providers during this request", |a| Operation::CiRefresh { target:a.target };
    "recovery.list" => RecoveryArguments {}, false, "List saved native recovery candidates without starting them", |_a| Operation::RecoveryList;
    "pane.resume" => ResumeArguments { pane_id: String, expected: HarnessRecovery }, false, "Resume an exact saved native binding using guarded recovery", |a| Operation::PaneResume { pane_id:a.pane_id, expected:a.expected };
    "workspace.create" => WorkspaceCreateArguments { name: Option<String>, initial: Option<PaneSpawnSpec> }, false, "Create a workspace, optionally with its initial execution", |a| Operation::WorkspaceCreate { name:a.name, initial:a.initial };
    "workspace.rename" => WorkspaceRenameArguments { workspace_id:String, expected_revision:u64, name:String }, false, "Rename a workspace at its expected revision", |a| Operation::WorkspaceRename { workspace_id:a.workspace_id, expected_revision:a.expected_revision, name:a.name };
    "workspace.close" => WorkspaceCloseArguments { workspace_id:String, expected_revision:u64 }, false, "Close a workspace and terminate its executions", |a| Operation::WorkspaceClose { workspace_id:a.workspace_id, expected_revision:a.expected_revision };
    "window.create" => WindowCreateArguments { workspace_id:String, expected_revision:u64, name:Option<String>, spec:PaneSpawnSpec }, false, "Create a window and initial execution atomically", |a| Operation::WindowCreate { workspace_id:a.workspace_id, expected_revision:a.expected_revision, name:a.name, spec:a.spec };
    "window.rename" => WindowRenameArguments { workspace_id:String, window_id:String, expected_revision:u64, name:String }, false, "Rename a window at its expected revision", |a| Operation::WindowRename { workspace_id:a.workspace_id, window_id:a.window_id, expected_revision:a.expected_revision, name:a.name };
    "window.close" => WindowCloseArguments { workspace_id:String, window_id:String, expected_revision:u64 }, false, "Close a window and terminate its executions", |a| Operation::WindowClose { workspace_id:a.workspace_id, window_id:a.window_id, expected_revision:a.expected_revision };
    "pane.create" => PaneCreateArguments { workspace_id:Option<String>, window_id:Option<String>, expected_revision:Option<u64>, panes:Vec<PaneSpawnEntry>, layout:Option<SpawnLayoutNode> }, false, "Atomically create a pane batch and publish its complete final layout", |a| Operation::PaneSpawn { workspace_id:a.workspace_id, window_id:a.window_id, expected_revision:a.expected_revision, panes:a.panes, layout:a.layout };
    "pane.reopen" => PaneReopenArguments { workspace_id:String, window_id:String, expected_revision:u64, pane_id:String, spec:PaneSpawnSpec }, false, "Replace an unavailable pane with explicit argv, never saved argv", |a| Operation::PaneReopen { workspace_id:a.workspace_id, window_id:a.window_id, expected_revision:a.expected_revision, pane_id:a.pane_id, spec:a.spec };
    "pane.rename" => PaneRenameArguments { pane_id:String, title:String }, false, "Rename a pane", |a| Operation::PaneRename { pane_id:a.pane_id, title:a.title };
    "pane.close" => PaneCloseArguments { pane_id:String }, false, "Terminate one execution without removing its layout", |a| Operation::PaneTerminate { pane_id:a.pane_id };
    "pane.forget" => PaneForgetArguments { pane_id:String }, false, "Forget a retired unreferenced pane with no active views", |a| Operation::PaneForget { pane_id:a.pane_id };
    "layout.set" => LayoutArguments { workspace_id:String, window_id:String, expected_revision:u64, layout:Option<LayoutNode> }, false, "Replace a complete window layout or clear it with null", |a| Operation::LayoutUpdate { workspace_id:a.workspace_id, window_id:a.window_id, expected_revision:a.expected_revision, layout:a.layout };
    "pane.inspect" => PaneInspectArguments { pane_id:String }, false, "Inspect live terminal metadata without exposing view authorities", |a| pane(a.pane_id, PaneRequest::Poll { view_id:String::new(), after:0, frame_after:u64::MAX, resources:Vec::new(), snapshot_after:0, wait_ms:0, row_deltas:false, scene_encoding:None });
    "pane.read" => PaneReadArguments { pane_id:String }, false, "Read the canonical visible terminal frame using a temporary read-only view", |a| pane(a.pane_id, PaneRequest::Poll { view_id:String::new(), after:0, frame_after:0, resources:Vec::new(), snapshot_after:0, wait_ms:0, row_deltas:false, scene_encoding:None });
    "input.text" => TextArguments { pane_id:String, text:String }, true, "Send exact UTF-8 bytes once, without appending Enter", |a| pane(a.pane_id, PaneRequest::Input { view_id:String::new(), data:a.text.into_bytes() });
    "input.paste" => PasteArguments { pane_id:String, text:String }, true, "Paste UTF-8 using the terminal's native bracketed-paste policy", |a| pane(a.pane_id, PaneRequest::Paste { view_id:String::new(), data:a.text.into_bytes() });
    "input.bytes" => BytesArguments { pane_id:String, data:Vec<u8> }, true, "Send exact bytes once, without retries or appended input", |a| pane(a.pane_id, PaneRequest::Input { view_id:String::new(), data:a.data });
    "input.key" => KeyArguments { pane_id:String, event:KeyEvent }, true, "Send one explicit native key event", |a| pane(a.pane_id, PaneRequest::Key { view_id:String::new(), event:a.event });
}

/// Defense in depth for authenticated wire callers, not only JSON adapters.
/// No nested/raw/client/harness/shutdown escape and no borrowed view IDs.
pub fn validate_native(operation: &Operation) -> Result<()> {
    match operation {
        Operation::Inspect
        | Operation::RecoveryList
        | Operation::CiRefresh { .. }
        | Operation::PaneResume { .. }
        | Operation::WorkspaceCreate { .. }
        | Operation::WorkspaceRename { .. }
        | Operation::WorkspaceClose { .. }
        | Operation::WindowCreate { .. }
        | Operation::WindowRename { .. }
        | Operation::WindowClose { .. }
        | Operation::PaneSpawn { .. }
        | Operation::PaneReopen { .. }
        | Operation::PaneTerminate { .. }
        | Operation::PaneForget { .. }
        | Operation::PaneRename { .. }
        | Operation::LayoutUpdate { .. } => Ok(()),
        Operation::Pane { request, .. } => match request {
            PaneRequest::Inspect => Ok(()),
            PaneRequest::Poll {
                view_id,
                after: 0,
                frame_after,
                resources,
                snapshot_after: 0,
                wait_ms: 0,
                row_deltas: false,
                scene_encoding: None,
            } if view_id.is_empty()
                && resources.is_empty()
                && matches!(*frame_after, 0 | u64::MAX) =>
            {
                Ok(())
            }
            PaneRequest::Input { view_id, data } | PaneRequest::Paste { view_id, data } => {
                ensure!(view_id.is_empty(), "agent input cannot borrow a view");
                ensure!(data.len() <= crate::MAX_INPUT_BYTES, "input payload limit");
                Ok(())
            }
            PaneRequest::Key { view_id, event } => {
                ensure!(view_id.is_empty(), "agent input cannot borrow a view");
                ensure!(
                    event.text.len() <= crate::MAX_INPUT_BYTES,
                    "input payload limit"
                );
                Ok(())
            }
            _ => bail!("agent pane operation is not supported"),
        },
        _ => bail!("agent control operation is not supported"),
    }
}

fn result_schema(operation: &str) -> Value {
    if matches!(operation, "pane.read" | "pane.inspect") {
        return serde_json::to_value(schemars::schema_for!(TerminalReadResult))
            .expect("serializable schema");
    }
    if is_input(operation) {
        return json!({
            "type":"object",
            "properties":{
                "accepted":{"type":"boolean"},
                "write_id":{"type":["integer","null"],"minimum":0},
                "semantics":{"enum":["queued_once_no_replay","execution_completed"]}
            },
            "required":["accepted","write_id","semantics"],
            "additionalProperties":false
        });
    }
    // Native registry/recovery metadata is intentionally extensible Value data,
    // not a second response DTO. Describe only stable object-level guarantees.
    json!({"type":"object","additionalProperties":true,
        "description":match operation {
            "api.help" => "Operation inventory, supported daemon-local scopes and capability limits.",
            "api.schema" => "One generated contract, or an operations array of generated contracts.",
            "recovery.list" => "Current recovery policy and recoveries array, filtered to selected scope.",
            "project.ci.refresh" => "Current scoped registry with cached project CI provider/check/PR/issue facts; refresh is queued asynchronously, not provider completion.",
            _ => "Native registry result, filtered to selected scope; pane project.ci contains cached provider/check/PR/issue facts with explicit loading/stale/error state; creation replies also identify created workspace/window/panes."
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_decodes_exact_identity_and_rejects_missing_fields() {
        let arguments = json!({"target":{
            "pane_id":"pane", "cwd":"/project",
            "repository":{"provider":"github","host":"github.com","path":"o/r","web_url":"https://github.com/o/r"},
            "head_sha":"checked", "local_head_sha":"local", "checked_ref":"origin/topic"
        }});
        let ControlAction::Native(Operation::CiRefresh { target }) =
            resolve("project.ci.refresh", arguments.clone()).unwrap()
        else {
            panic!("refresh did not resolve to native operation")
        };
        assert_eq!(target.pane_id, "pane");
        assert_eq!(target.head_sha, "checked");
        assert_eq!(target.local_head_sha, "local");
        assert_eq!(target.checked_ref.as_deref(), Some("origin/topic"));
        for field in ["pane_id", "cwd", "repository", "head_sha", "local_head_sha"] {
            let mut missing = arguments.clone();
            missing["target"].as_object_mut().unwrap().remove(field);
            assert!(resolve("project.ci.refresh", missing).is_err());
        }
        let mut extra = arguments;
        extra["target"]["session_id"] = json!("another-session");
        assert!(resolve("project.ci.refresh", extra).is_err());
        assert!(!is_input("project.ci.refresh"));
    }

    #[test]
    fn catalog_schemas_and_decoder_share_closed_argument_types() {
        let ControlAction::Local(help) = resolve("api.help", json!({})).unwrap() else {
            panic!()
        };
        for operation in help["operations"].as_array().unwrap() {
            let name = operation["operation"].as_str().unwrap();
            let ControlAction::Local(schema) =
                resolve("api.schema", json!({"operation":name})).unwrap()
            else {
                panic!()
            };
            assert_eq!(schema["arguments"]["additionalProperties"], false);
            assert!(resolve(name, json!({"unexpected":true})).is_err());
            assert_eq!(schema["input"], is_input(name));
        }
        for name in [
            "shutdown",
            "agent",
            "client",
            "pane",
            "harness.begin",
            "raw",
        ] {
            assert!(resolve(name, json!({})).is_err());
        }
    }
    #[test]
    fn input_is_explicit_and_cannot_supply_view_authority() {
        let ControlAction::Native(Operation::Pane {
            request: PaneRequest::Input { data, view_id },
            ..
        }) = resolve("input.text", json!({"pane_id":"p","text":"echo hi"})).unwrap()
        else {
            panic!()
        };
        assert_eq!(data, b"echo hi");
        assert!(view_id.is_empty());
        assert!(
            resolve(
                "input.text",
                json!({"pane_id":"p","text":"x","view_id":"human"})
            )
            .is_err()
        );
        assert!(resolve("input.bytes", json!({"pane_id":"p","data":[256]})).is_err());
        assert!(resolve("pane.read", json!({"pane_id":"p","wait_ms":5000})).is_err());
        assert!(
            validate_native(&Operation::Pane {
                pane_id: "p".into(),
                request: PaneRequest::Input {
                    view_id: "human".into(),
                    data: vec![]
                }
            })
            .is_err()
        );
    }
    #[test]
    fn malformed_layout_and_nested_spawn_contracts_cannot_reach_native_dispatch() {
        assert!(
            resolve(
                "layout.set",
                json!({
                    "workspace_id":"w","window_id":"n","expected_revision":1,
                    "layout":{"kind":"leaf","pane_id":"p","request":{"op":"shutdown"}}
                })
            )
            .is_err()
        );
        assert!(
            resolve(
                "pane.create",
                json!({
                    "panes":[{"reference":"p","spec":{"argv":["/bin/sh"],"cwd":"/","home":"/",
                        "size":{"cols":80,"rows":24,"cell_width":8,"cell_height":16},
                        "unexpected":"not silently ignored"}}]
                })
            )
            .is_err()
        );
        assert!(resolve("input.key", json!({
            "pane_id":"p","event":{"key":"Enter","modifiers":0,"text":"","action":"Invalid","unshifted_codepoint":0}
        })).is_err());
        assert!(resolve("session.inspect", Value::Null).is_err());
    }
}
