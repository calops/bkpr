//! Discoverable CLI/MCP vocabulary mapped to the existing native protocol.
//! This is deliberately not an arbitrary Operation deserializer.
use crate::{
    HarnessRecovery, KeyEvent, LayoutNode, Operation, PaneRequest, PaneSpawnEntry, PaneSpawnSpec,
    SpawnLayoutNode,
};
use anyhow::{Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub enum ControlAction {
    Local(Value),
    Native(Operation),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlReply {
    #[default]
    Receipt,
    Full,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct InspectQuery {
    pub workspace_id: Option<String>,
    pub window_id: Option<String>,
    pub pane_id: Option<String>,
    pub detail: bool,
    pub include_retired: bool,
    pub after: Option<String>,
    pub limit: Option<usize>,
}
impl InspectQuery {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.limit.is_none_or(|n| (1..=50).contains(&n)),
            "limit must be 1..50"
        );
        ensure!(
            self.after.as_ref().is_none_or(|cursor| cursor.len() <= 256
                && cursor
                    .split_once(':')
                    .is_some_and(|(kind, id)| matches!(kind, "pane" | "window" | "workspace")
                        && !id.is_empty())),
            "invalid inspect cursor"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalReadQuery {
    pub text: bool,
    pub tail_rows: Option<usize>,
    pub offset: Option<usize>,
    pub max_bytes: Option<usize>,
    pub expected_revision: Option<u64>,
}
impl Default for TerminalReadQuery {
    fn default() -> Self {
        Self {
            text: true,
            tail_rows: None,
            offset: None,
            max_bytes: None,
            expected_revision: None,
        }
    }
}
impl TerminalReadQuery {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_bytes.is_none_or(|n| (4..=16_384).contains(&n)),
            "max_bytes must be 4..16384"
        );
        ensure!(
            self.tail_rows.is_none_or(|n| n > 0),
            "tail_rows must be positive"
        );
        ensure!(
            self.offset.unwrap_or(0) == 0 || self.expected_revision.is_some(),
            "continuation requires expected_revision"
        );
        Ok(())
    }
}

/// Control-only convenience defaults; the native wire KeyEvent remains strict.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ControlKeyEvent {
    key: String,
    #[serde(default)]
    modifiers: u16,
    #[serde(default)]
    text: String,
    #[serde(default = "key_press")]
    action: crate::KeyAction,
    #[serde(default)]
    unshifted_codepoint: u32,
}
fn key_press() -> crate::KeyAction {
    crate::KeyAction::Press
}
impl From<ControlKeyEvent> for KeyEvent {
    fn from(event: ControlKeyEvent) -> Self {
        Self {
            key: event.key,
            modifiers: event.modifiers,
            text: event.text,
            action: event.action,
            unshifted_codepoint: event.unshifted_codepoint,
        }
    }
}

/// Plain viewport export, never scrollback recovery or image pixel transport.
#[derive(Serialize, JsonSchema)]
pub struct TerminalReadResult {
    pub pane_id: String,
    pub pid: Option<u32>,
    /// Canonical viewport revision, stable across unchanged view/publication wakes.
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
    /// UTF-8 byte offsets in the right-trimmed selected viewport rows.
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_bytes: usize,
    pub start_row: usize,
    pub total_rows: usize,
    pub complete: bool,
}
#[derive(Serialize, JsonSchema)]
pub struct TerminalCursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

#[derive(JsonSchema)]
pub struct InspectResult {
    pub registry_revision: u64,
    pub items: Vec<InspectItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}
#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectKind {
    Workspace,
    Window,
    Pane,
}
#[derive(JsonSchema)]
pub struct InspectItem {
    pub kind: InspectKind,
    pub id: String,
    pub workspace_id: Option<String>,
    pub workspace_revision: Option<u64>,
    pub window_id: Option<String>,
    pub window_revision: Option<u64>,
    pub pane_id: Option<String>,
    pub terminal_revision: Option<u64>,
    pub status: Option<String>,
    pub harness: Option<Value>,
    pub harness_status: Option<Value>,
    pub harness_status_changed_at_ms: Option<u64>,
    /// Explicit detail only.
    pub name: Option<String>,
    pub layout: Option<LayoutNode>,
    pub detail: Option<Value>,
}
#[derive(JsonSchema)]
pub struct MutationReceipt {
    pub operation: String,
    pub accepted: bool,
    pub registry_revision: Option<u64>,
    pub workspace_id: Option<String>,
    pub workspace_revision: Option<u64>,
    pub window_id: Option<String>,
    pub window_revision: Option<u64>,
    pub pane_id: Option<String>,
    pub replaced_pane_id: Option<String>,
    pub created_panes: Option<std::collections::BTreeMap<String, String>>,
    pub closed_pane_ids: Option<Vec<String>>,
    pub workspace_closed: Option<bool>,
    pub window_closed: Option<bool>,
    pub pane_closed: Option<bool>,
    pub forgotten: Option<bool>,
    pub source_pane_id: Option<String>,
    pub policy: Option<String>,
    pub started: Option<bool>,
    pub binding: Option<String>,
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
    ($( $name:literal => $ty:ident { $( $(#[$attr:meta])* $field:ident : $kind:ty ),* $(,)? }, $input:literal, $description:literal, |$args:ident| $mapping:expr; )*) => {
        $(
            #[derive(Deserialize, JsonSchema)]
            #[serde(deny_unknown_fields)]
            struct $ty { $( $(#[$attr])* $field: $kind, )* }
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
                _ => return Err(unsupported(operation)),
            };
            let arguments = argument_contract(operation, arguments);
            Ok(json!({
                "operation":operation,"input":is_input(operation),"arguments":arguments,
                "signature": signature_from_schema(operation, &arguments),
                "result":result_schema(operation)
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
                        "capabilities":{"atomic_pane_batch":true,"terminal_read":"paged right-trimmed UTF-8 visible viewport; no history/image pixels","input":"explicit authority, no implicit Enter/replay","temporary_views":"owned read-only; no size/focus/clipboard authority","cross_daemon":false},
                        "notes":["expected_revision: workspace for workspace mutations/window.create; window for window mutations/pane.create/reopen/layout.set","pane.create: workspace_id,window_id,expected_revision,layout all present or all absent; unowned requires session/all scope","pane.close terminates; remove layout references before pane.forget"]
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
                    let $args: $ty = serde_json::from_value(arguments).map_err(|error| {
                        anyhow::anyhow!("invalid arguments: {}; {}", bounded_message(&error.to_string()), signature_from_schema(operation, &argument_contract(operation, serde_json::to_value(schemars::schema_for!($ty)).expect("schema"))))
                    })?;
                    $mapping
                }, )*
                _ => return Err(unsupported(operation)),
            };
            validate_native(&native)?;
            Ok(ControlAction::Native(native))
        }
    }
}

catalog! {
    "session.inspect" => InspectArguments { workspace_id:Option<String>, window_id:Option<String>, pane_id:Option<String>, #[serde(default)] detail:bool, #[serde(default)] include_retired:bool, after:Option<String>, limit:Option<usize> }, false, "Page flat IDs/revisions; detail opts into metadata/layout", |a| Operation::ControlInspect { query:InspectQuery { workspace_id:a.workspace_id,window_id:a.window_id,pane_id:a.pane_id,detail:a.detail,include_retired:a.include_retired,after:a.after,limit:a.limit } };
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
    "pane.inspect" => PaneInspectArguments { pane_id:String }, false, "Inspect terminal metadata without text or view authority", |a| Operation::ControlPaneRead { pane_id:a.pane_id, query:TerminalReadQuery { text:false, ..Default::default() } };
    "pane.read" => PaneReadArguments { pane_id:String, tail_rows:Option<usize>, offset:Option<usize>, max_bytes:Option<usize>, expected_revision:Option<u64> }, false, "Page right-trimmed viewport text; continue at next_offset with expected_revision", |a| Operation::ControlPaneRead { pane_id:a.pane_id, query:TerminalReadQuery { text:true,tail_rows:a.tail_rows,offset:a.offset,max_bytes:a.max_bytes,expected_revision:a.expected_revision } };
    "input.text" => TextArguments { pane_id:String, text:String }, true, "Send exact UTF-8 bytes once, without appending Enter", |a| pane(a.pane_id, PaneRequest::Input { view_id:String::new(), data:a.text.into_bytes() });
    "input.paste" => PasteArguments { pane_id:String, text:String }, true, "Paste UTF-8 using the terminal's native bracketed-paste policy", |a| pane(a.pane_id, PaneRequest::Paste { view_id:String::new(), data:a.text.into_bytes() });
    "input.bytes" => BytesArguments { pane_id:String, data:Vec<u8> }, true, "Send exact bytes once, without retries or appended input", |a| pane(a.pane_id, PaneRequest::Input { view_id:String::new(), data:a.data });
    "input.key" => KeyArguments { pane_id:String, event:ControlKeyEvent }, true, "Named key, e.g. event:{key:\"Escape\"}; printable text uses input.text", |a| pane(a.pane_id, PaneRequest::Key { view_id:String::new(), event:a.event.into() });
}

/// Defense in depth for authenticated wire callers, not only JSON adapters.
/// No nested/raw/client/harness/shutdown escape and no borrowed view IDs.
pub fn validate_native(operation: &Operation) -> Result<()> {
    match operation {
        Operation::ControlInspect { query } => query.validate(),
        Operation::ControlPaneRead { query, .. } => query.validate(),
        Operation::RecoveryList
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
        | Operation::LayoutUpdate { .. } => {
            if let Operation::PaneSpawn {
                workspace_id,
                window_id,
                expected_revision,
                layout,
                ..
            } = operation
            {
                let all = workspace_id.is_some()
                    && window_id.is_some()
                    && expected_revision.is_some()
                    && layout.is_some();
                let none = workspace_id.is_none()
                    && window_id.is_none()
                    && expected_revision.is_none()
                    && layout.is_none();
                ensure!(
                    all || none,
                    "pane.create requires workspace_id, window_id, expected_revision and layout all present or all absent"
                );
            }
            Ok(())
        }
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

fn bounded_message(message: &str) -> String {
    let mut characters = message.chars();
    let mut brief: String = characters.by_ref().take(180).collect();
    if characters.next().is_some() {
        brief.push_str("... (diagnostic shortened)");
    }
    brief
}

fn argument_contract(operation: &str, mut arguments: Value) -> Value {
    if operation == "pane.create" {
        arguments["description"] = json!(
            "workspace_id, window_id, expected_revision and layout must all be present (owned) or all absent (unowned; explicit session/all scope). Layout is the complete final target-window layout. Existing nodes must belong to that window."
        );
        arguments["oneOf"] = json!([
            {"required":["workspace_id","window_id","expected_revision","layout"],"properties":{
                "workspace_id":{"type":"string"},"window_id":{"type":"string"},
                "expected_revision":{"type":"integer"},"layout":{"type":"object"}}},
            {"properties":{"workspace_id":{"type":"null"},"window_id":{"type":"null"},
                "expected_revision":{"type":"null"},"layout":{"type":"null"}}}
        ]);
    }
    if operation == "session.inspect" {
        arguments["description"] = json!(
            "Flat page of elementary workspace/window/pane IDs and current revisions. Filters and scope apply before paging. limit defaults10 (1..50); next_cursor continues after identity, not registry revision. detail opts into metadata/layout; include_retired opts into retained history, never deletes it."
        );
    }
    if operation == "pane.read" {
        arguments["description"] = json!(
            "Right-trimmed visible UTF-8 text, indentation preserved. tail_rows selects bottom rows; max_bytes defaults2048 (4..16384). offset/next_offset are UTF-8 bytes in selected text. Continuation requires the returned terminal revision as expected_revision and the same tail_rows; changed revision fails rather than mixing frames. No scrollback/image pixels."
        );
    }
    arguments
}

fn signature_from_schema(operation: &str, schema: &Value) -> String {
    fn fields(schema: &Value, root: &Value, nested: bool) -> String {
        let required = schema["required"].as_array();
        schema["properties"]
            .as_object()
            .map(|properties| {
                properties
                    .iter()
                    .map(|(name, property)| {
                        let suffix = if required
                            .is_some_and(|required| required.iter().any(|field| field == name))
                        {
                            ""
                        } else {
                            "?"
                        };
                        let target = property["$ref"]
                            .as_str()
                            .and_then(|reference| reference.strip_prefix('#'))
                            .and_then(|pointer| root.pointer(pointer))
                            .unwrap_or(property);
                        if nested && target["properties"].is_object() {
                            format!("{name}{suffix}:{{{}}}", fields(target, root, false))
                        } else {
                            format!("{name}{suffix}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    }
    format!("{operation}({})", fields(schema, schema, true))
}

/// Compact signatures generated from the decoder schemas, suitable for tool help.
pub fn common_signatures() -> String {
    [
        "session.inspect",
        "window.create",
        "window.close",
        "pane.create",
        "pane.read",
        "input.key",
    ]
    .iter()
    .map(|name| {
        schema(name).expect("known operation")["signature"]
            .as_str()
            .unwrap()
            .to_owned()
    })
    .collect::<Vec<_>>()
    .join("; ")
}

fn unsupported(operation: &str) -> anyhow::Error {
    let prefix = operation.split('.').next().unwrap_or_default();
    let inventory = inventory();
    let names = inventory
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["operation"].as_str())
        .filter(|name| name.split('.').next() == Some(prefix))
        .take(6)
        .collect::<Vec<_>>();
    anyhow::anyhow!(
        "unsupported control operation; use {}",
        if names.is_empty() {
            "api.help".to_owned()
        } else {
            names.join(", ")
        }
    )
}

/// Canonical compact projection for authenticated control and the admin CLI.
/// Read-only operations and explicit input receipts are already projections.
pub fn mutation_receipt(operation: &Operation, result: Value) -> Result<Value> {
    match receipt_target(operation) {
        Some(target) => project_receipt(target, result),
        None => Ok(result),
    }
}

/// Retain only elementary target IDs while a consuming transport owns the
/// request. In particular, CLI receipt projection never clones spawn argv.
pub struct MutationProjection {
    workspace: Option<String>,
    window: Option<String>,
    pane: Option<String>,
    operation: &'static str,
}
impl MutationProjection {
    pub fn for_operation(operation: &Operation) -> Option<Self> {
        receipt_target(operation).map(|(workspace, window, pane, operation)| Self {
            workspace: workspace.cloned(),
            window: window.cloned(),
            pane: pane.cloned(),
            operation,
        })
    }
    pub fn apply(self, result: Value) -> Result<Value> {
        project_receipt(
            (
                self.workspace.as_ref(),
                self.window.as_ref(),
                self.pane.as_ref(),
                self.operation,
            ),
            result,
        )
    }
}

type ReceiptTarget<'a> = (
    Option<&'a String>,
    Option<&'a String>,
    Option<&'a String>,
    &'static str,
);
fn receipt_target(operation: &Operation) -> Option<ReceiptTarget<'_>> {
    Some(match operation {
        Operation::WorkspaceCreate { .. } => (None, None, None, "workspace.create"),
        Operation::WorkspaceRename { workspace_id, .. } => {
            (Some(workspace_id), None, None, "workspace.rename")
        }
        Operation::WorkspaceClose { workspace_id, .. } => {
            (Some(workspace_id), None, None, "workspace.close")
        }
        Operation::WindowCreate { workspace_id, .. } => {
            (Some(workspace_id), None, None, "window.create")
        }
        Operation::WindowRename {
            workspace_id,
            window_id,
            ..
        } => (Some(workspace_id), Some(window_id), None, "window.rename"),
        Operation::WindowClose {
            workspace_id,
            window_id,
            ..
        } => (Some(workspace_id), Some(window_id), None, "window.close"),
        Operation::PaneSpawn {
            workspace_id,
            window_id,
            ..
        } => (
            workspace_id.as_ref(),
            window_id.as_ref(),
            None,
            "pane.create",
        ),
        Operation::PaneReopen {
            workspace_id,
            window_id,
            pane_id,
            ..
        } => (
            Some(workspace_id),
            Some(window_id),
            Some(pane_id),
            "pane.reopen",
        ),
        Operation::PaneRename { pane_id, .. } => (None, None, Some(pane_id), "pane.rename"),
        Operation::PaneTerminate { pane_id } => (None, None, Some(pane_id), "pane.close"),
        Operation::PaneForget { pane_id } => (None, None, Some(pane_id), "pane.forget"),
        Operation::PaneResume { pane_id, .. } => (None, None, Some(pane_id), "pane.resume"),
        Operation::LayoutUpdate {
            workspace_id,
            window_id,
            ..
        } => (Some(workspace_id), Some(window_id), None, "layout.set"),
        Operation::CiRefresh { target } => {
            (None, None, Some(&target.pane_id), "project.ci.refresh")
        }
        _ => return None,
    })
}

fn project_receipt(
    (workspace, window, pane, name): ReceiptTarget<'_>,
    result: Value,
) -> Result<Value> {
    let mut receipt = json!({"operation":name,"accepted":true});
    if name == "pane.reopen" {
        receipt["replaced_pane_id"] = json!(pane);
    }
    for key in [
        "registry_revision",
        "workspace_id",
        "workspace_revision",
        "window_id",
        "window_revision",
        "pane_id",
        "created_panes",
        "closed_pane_ids",
        "workspace_closed",
        "window_closed",
        "pane_closed",
        "forgotten",
        "source_pane_id",
        "policy",
        "started",
        "binding",
    ] {
        if let Some(value) = result.get(key) {
            receipt[key] = value.clone();
        }
    }
    if receipt.get("registry_revision").is_none()
        && let Some(revision) = result.pointer("/registry/revision")
    {
        receipt["registry_revision"] = revision.clone();
    }
    for (key, id) in [
        ("workspace_id", workspace),
        ("window_id", window),
        ("pane_id", pane),
    ] {
        if receipt.get(key).is_none()
            && let Some(id) = id
        {
            receipt[key] = json!(id);
        }
    }
    if (receipt.get("workspace_id").is_none() || receipt.get("window_id").is_none())
        && let Some(pane) = result
            .pointer("/registry/panes")
            .and_then(Value::as_array)
            .and_then(|panes| panes.iter().find(|pane| pane["id"] == receipt["pane_id"]))
    {
        for key in ["workspace_id", "window_id"] {
            if receipt.get(key).is_none() {
                receipt[key] = pane[key].clone();
            }
        }
    }
    let workspaces = result
        .pointer("/registry/workspaces")
        .and_then(Value::as_array);
    let owner = workspaces.and_then(|items| {
        items
            .iter()
            .find(|item| item["id"] == receipt["workspace_id"])
    });
    if let Some(owner) = owner {
        receipt["workspace_revision"] = owner["revision"].clone();
        if let Some(window) = owner["windows"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == receipt["window_id"]))
        {
            receipt["window_revision"] = window["revision"].clone();
        }
    }
    match name {
        "workspace.close" => receipt["workspace_closed"] = json!(true),
        "window.close" => receipt["window_closed"] = json!(true),
        "pane.close" => receipt["pane_closed"] = json!(true),
        "pane.forget" => receipt["forgotten"] = json!(true),
        _ => {}
    }
    Ok(receipt)
}

fn result_schema(operation: &str) -> Value {
    if matches!(operation, "pane.read" | "pane.inspect") {
        return serde_json::to_value(schemars::schema_for!(TerminalReadResult))
            .expect("serializable schema");
    }
    if operation == "session.inspect" {
        return serde_json::to_value(schemars::schema_for!(InspectResult)).expect("schema");
    }
    if !matches!(operation, "api.help" | "api.schema" | "recovery.list") && !is_input(operation) {
        return serde_json::to_value(schemars::schema_for!(MutationReceipt)).expect("schema");
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
    json!({"type":"object","additionalProperties":true,
        "description":match operation {
            "api.help" => "Operation inventory and shared authority/acceptance rules.",
            "api.schema" => "Generated argument/signature/default result contract; reply:full opts into the scoped native mutation result.",
            "recovery.list" => "Retained recovery descriptors in the selected scope; not automatically deleted.",
            _ => "Scoped native result."
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

    #[test]
    fn control_queries_are_bounded_and_named_keys_have_safe_defaults() {
        let ControlAction::Native(Operation::ControlInspect { query }) =
            resolve("session.inspect", json!({})).unwrap()
        else {
            panic!()
        };
        assert!(!query.detail);
        assert!(!query.include_retired);
        let ControlAction::Native(Operation::ControlPaneRead { query, .. }) = resolve(
            "pane.read",
            json!({"pane_id":"p","tail_rows":4,"max_bytes":64}),
        )
        .unwrap() else {
            panic!()
        };
        assert!(query.text);
        assert_eq!(query.tail_rows, Some(4));
        assert!(resolve("session.inspect", json!({"limit":0})).is_err());
        assert!(resolve("session.inspect", json!({"limit":51})).is_err());
        assert!(resolve("pane.read", json!({"pane_id":"p","max_bytes":3})).is_err());
        assert!(resolve("pane.read", json!({"pane_id":"p","offset":1})).is_err());
        let ControlAction::Native(Operation::Pane {
            request: PaneRequest::Key { event, .. },
            ..
        }) = resolve("input.key", json!({"pane_id":"p","event":{"key":"Escape"}})).unwrap()
        else {
            panic!()
        };
        assert_eq!(event.key, "Escape");
        assert_eq!(event.action, crate::KeyAction::Press);
        assert_eq!(event.modifiers, 0);
        assert!(event.text.is_empty());
        assert_eq!(event.unshifted_codepoint, 0);
        let ControlAction::Native(Operation::Pane {
            request: PaneRequest::Key { event, .. },
            ..
        }) = resolve(
            "input.key",
            json!({"pane_id":"p","event":{"key":"KeyA","modifiers":2}}),
        )
        .unwrap()
        else {
            panic!()
        };
        assert!(event.text.is_empty());
        assert_eq!(event.unshifted_codepoint, 0);
        assert!(serde_json::from_value::<KeyEvent>(json!({"key":"Escape"})).is_err());
    }

    #[test]
    fn receipts_preserve_created_mappings_and_exact_revision_owners_not_payloads() {
        let operation = Operation::PaneSpawn {
            workspace_id: Some("w".into()),
            window_id: Some("n".into()),
            expected_revision: Some(4),
            panes: Vec::new(),
            layout: None,
        };
        let receipt = mutation_receipt(&operation,json!({
            "registry":{"revision":99,"workspaces":[{"id":"w","revision":7,"windows":[{"id":"n","revision":5,"layout":{"large":"excluded"}}]}],
                "panes":[{"id":"p","project":{"huge":"excluded"},"title":"excluded"}]},
            "created_panes":{"first":"p","second":"q"},"pane_id":null
        })).unwrap();
        assert_eq!(receipt["registry_revision"], 99);
        assert_eq!(receipt["workspace_revision"], 7);
        assert_eq!(receipt["window_revision"], 5);
        assert_eq!(receipt["created_panes"], json!({"first":"p","second":"q"}));
        assert!(receipt.get("registry").is_none());
        assert!(receipt.get("layout").is_none());
        let closed = mutation_receipt(
            &Operation::WindowClose {
                workspace_id: "w".into(),
                window_id: "n".into(),
                expected_revision: 5,
            },
            json!({"registry":{"revision":100,"workspaces":[],"panes":[]}}),
        )
        .unwrap();
        assert_eq!(closed["workspace_id"], "w");
        assert_eq!(closed["window_id"], "n");
        assert_eq!(closed["window_closed"], true);
        assert!(closed.get("window_revision").is_none());
    }

    #[test]
    fn schema_and_errors_expose_complete_ownership_contract_and_bounded_signatures() {
        let ControlAction::Local(schema) =
            resolve("api.schema", json!({"operation":"pane.create"})).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            schema["arguments"]["oneOf"][0]["required"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        for arguments in [
            json!({"panes":[],"workspace_id":"w"}),
            json!({"panes":[],"workspace_id":"w","window_id":"n","expected_revision":1}),
            json!({"panes":[],"layout":{"kind":"new","reference":"p"}}),
        ] {
            assert!(resolve("pane.create", arguments).is_err());
        }
        let error = resolve("window.close", json!({"workspace_id":"w"}))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("expected_revision"));
        assert!(error.contains("window.close("));
        let mut arguments = json!({"pane_id":"p"});
        arguments
            .as_object_mut()
            .unwrap()
            .insert("x".repeat(10000), json!(true));
        let oversized = resolve("pane.read", arguments).err().unwrap().to_string();
        assert!(oversized.len() < 600);
        let signatures = common_signatures();
        assert!(signatures.contains("event:{"));
        assert!(signatures.contains("key"));
    }
}
