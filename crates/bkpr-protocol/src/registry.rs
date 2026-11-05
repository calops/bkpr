use crate::{CaptureEvent, HarnessInfo, HarnessKind, PROTOCOL_VERSION, PaneRequest, Size};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub operation: Operation,
}
impl Request {
    pub fn new(operation: Operation) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    SessionInfo,
    /// Upgrade this connection to a dedicated attachment lifetime stream.
    ClientRegister {
        session_id: String,
        daemon_id: String,
    },
    /// Authenticate an exact attachment, never inferred from the caller PID.
    Client {
        authority: crate::ClientAuthority,
        request: Box<Operation>,
    },
    /// One-shot readiness and source publication; repeated activation changes neither.
    ClientActivate {
        detach_others: bool,
        /// Launch-local source; explicit absence disables forwarding for this attachment.
        #[serde(with = "crate::path_wire::optional")]
        agent_socket: Option<PathBuf>,
    },
    ClientDetach,
    Inspect,
    /// Query live execution membership; omission selects the authenticated OS peer's pane.
    Context {
        pane_id: Option<String>,
    },
    /// Enumerate saved native conversations without starting executions.
    RecoveryList,
    /// Start a fresh managed execution for an explicitly selected saved binding.
    PaneResume {
        pane_id: String,
        expected: crate::HarnessRecovery,
    },
    HarnessBegin {
        kind: HarnessKind,
    },
    HarnessEvent {
        launch_id: String,
        token: String,
        event: CaptureEvent,
    },
    HarnessContext {
        launch_id: String,
        token: String,
    },
    Agent {
        launch_id: String,
        token: String,
        request: crate::AgentRequest,
    },
    WorkspaceCreate {
        name: Option<String>,
        initial: Option<PaneSpawnSpec>,
    },
    WorkspaceRename {
        workspace_id: String,
        expected_revision: u64,
        name: String,
    },
    WorkspaceClose {
        workspace_id: String,
        expected_revision: u64,
    },
    WindowCreate {
        workspace_id: String,
        expected_revision: u64,
        name: Option<String>,
        spec: PaneSpawnSpec,
    },
    WindowRename {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        name: String,
    },
    WindowClose {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
    },
    PaneSpawn {
        workspace_id: Option<String>,
        window_id: Option<String>,
        expected_revision: Option<u64>,
        panes: Vec<PaneSpawnEntry>,
        layout: Option<SpawnLayoutNode>,
    },
    /// Replace an unavailable pane in its owning window; never replay saved argv.
    PaneReopen {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        pane_id: String,
        spec: PaneSpawnSpec,
    },
    PaneTerminate {
        pane_id: String,
    },
    /// Forget a retired, unreferenced pane with no active views; never terminates a process.
    PaneForget {
        pane_id: String,
    },
    PaneRename {
        pane_id: String,
        title: String,
    },
    LayoutUpdate {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        layout: Option<LayoutNode>,
    },
    Pane {
        pane_id: String,
        request: PaneRequest,
    },
    Wait {
        after: u64,
        wait_ms: u64,
    },
    Shutdown,
}
impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut value = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
        fn redact(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        if key == "token" || key == "agent_socket" {
                            *value = serde_json::Value::String("[REDACTED]".into());
                        } else {
                            redact(value);
                        }
                    }
                }
                serde_json::Value::Array(values) => values.iter_mut().for_each(redact),
                _ => {}
            }
        }
        redact(&mut value);
        std::fmt::Debug::fmt(&value, f)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneSpawnEntry {
    pub reference: String,
    pub spec: PaneSpawnSpec,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnLayoutNode {
    Existing {
        pane_id: String,
    },
    New {
        reference: String,
    },
    Split {
        axis: Axis,
        /// First child's share in thousandths; 500 is an equal split.
        #[schemars(range(min = 1, max = 999))]
        ratio: u16,
        first: Box<SpawnLayoutNode>,
        second: Box<SpawnLayoutNode>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneSpawnSpec {
    pub argv: Vec<String>,
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub cwd: PathBuf,
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub home: PathBuf,
    pub size: Size,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub isolated_environment: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub revision: u64,
    pub workspaces: Vec<Workspace>,
    pub panes: Vec<PaneInfo>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub windows: Vec<Window>,
}
/// Backend-owned independent layout. Active-window selection belongs to each client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub layout: Option<LayoutNode>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayoutNode {
    Leaf {
        pane_id: String,
    },
    Split {
        axis: Axis,
        /// First child's share in thousandths; 500 is an equal split.
        #[schemars(range(min = 1, max = 999))]
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    /// Execution ownership, paired with window_id; both absent only for admin panes.
    pub workspace_id: Option<String>,
    pub window_id: Option<String>,
    pub title: String,
    pub status: PaneStatus,
    pub exit_code: Option<i32>,
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    /// Observed live foreground facts; never adopted from durable metadata.
    #[serde(default, with = "crate::path_wire::optional")]
    pub foreground_cwd: Option<PathBuf>,
    #[serde(default)]
    pub harness: Option<HarnessInfo>,
    /// Backend-selected, identity-scoped OSC fact, including when no view is attached.
    #[serde(default)]
    pub harness_status: Option<crate::ProgramStatus>,
    /// Live backend cache result for the effective foreground CWD; never restored.
    #[serde(default)]
    pub project: Option<crate::ProjectSnapshot>,
    #[serde(with = "crate::path_wire")]
    pub home: PathBuf,
    pub isolated_environment: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaneStatus {
    Running,
    Stopped,
    Exited,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<crate::Payload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RequestFailure>,
}

/// Machine-readable failure facts; an error string alone grants no shutdown semantics.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestFailure {
    SessionClosing { outcome: RequestOutcome },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    /// This specific request did not enter application dispatch.
    NotDispatched,
    /// Dispatch began; failure does not prove that the request had no effects.
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryResult {
    pub registry: RegistrySnapshot,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub created_panes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
}
