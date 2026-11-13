use crate::{CaptureEvent, HarnessInfo, HarnessKind, PROTOCOL_VERSION, PaneRequest, Size};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Native request envelope carrying an explicit wire family.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    /// Required native wire family.
    pub version: u32,
    #[serde(flatten)]
    /// Backend operation to dispatch.
    pub operation: Operation,
}
impl Request {
    /// Wrap an operation with the current native wire family.
    pub fn new(operation: Operation) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation,
        }
    }
}
/// Backend requests; authorization and ownership are enforced by the receiving daemon.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Read the logical session and current daemon identities.
    SessionInfo,
    /// Upgrade this connection to a dedicated attachment lifetime stream.
    ClientRegister {
        /// Expected persistent session identity.
        session_id: String,
        /// Expected daemon incarnation.
        daemon_id: String,
    },
    /// Authenticate an exact attachment, never inferred from the caller PID.
    Client {
        /// Credentials issued by this attachment's lifetime stream.
        authority: crate::ClientAuthority,
        /// Operation submitted on behalf of the attachment.
        request: Box<Operation>,
    },
    /// One-shot readiness and source publication; repeated activation changes neither.
    ClientActivate {
        /// Whether activation should revoke other attachments.
        detach_others: bool,
        /// Launch-local source; explicit absence disables forwarding for this attachment.
        #[serde(with = "crate::path_wire::optional")]
        agent_socket: Option<PathBuf>,
    },
    /// Revoke the authenticated client attachment without terminating panes.
    ClientDetach,
    /// Read the current workspace and pane registry.
    Inspect,
    /// Read a filtered, paginated control projection.
    ControlInspect {
        /// Filters, detail policy and pagination cursor.
        query: crate::control::InspectQuery,
    },
    /// Read viewport metadata or a revision-bound UTF-8 text page.
    ControlPaneRead {
        /// Exact pane identity.
        pane_id: String,
        /// Text selection and continuation parameters.
        query: crate::control::TerminalReadQuery,
    },
    /// Queue a provider refresh only for this exact retained project source.
    CiRefresh {
        /// Retained source identity that must still match.
        target: crate::CiRefreshTarget,
    },
    /// Query live execution membership; omission selects the authenticated OS peer's pane.
    Context {
        /// Exact pane, or `None` for the authenticated peer's pane.
        pane_id: Option<String>,
    },
    /// Enumerate saved native conversations without starting executions.
    RecoveryList,
    /// Start a fresh managed execution for an explicitly selected saved binding.
    PaneResume {
        /// Retired pane whose saved conversation is selected.
        pane_id: String,
        /// Exact recovery descriptor expected to remain current.
        expected: crate::HarnessRecovery,
    },
    /// Begin managed capture for the authenticated harness process.
    HarnessBegin {
        /// Harness integration being launched.
        kind: HarnessKind,
    },
    /// Submit a launch-authenticated native conversation event.
    HarnessEvent {
        /// Exact live launch identity.
        launch_id: String,
        /// Capture capability issued to this launch.
        token: String,
        /// Native conversation binding event.
        event: CaptureEvent,
    },
    /// Read context using a launch's capture capability.
    HarnessContext {
        /// Exact live launch identity.
        launch_id: String,
        /// Capture capability issued to this launch.
        token: String,
    },
    /// Submit a launch-authenticated communication or control request.
    Agent {
        /// Exact live launch identity.
        launch_id: String,
        /// Capability issued to this launch.
        token: String,
        /// Exact native root and caller context required by guarded integrations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<crate::AgentBindingGuard>,
        /// Agent operation; scope and authority are checked by the backend.
        request: crate::AgentRequest,
    },
    /// Create a workspace, optionally with an initial execution.
    WorkspaceCreate {
        /// User-visible name, or the backend's generated name.
        name: Option<String>,
        /// Initial pane launch specification, if requested.
        initial: Option<PaneSpawnSpec>,
    },
    /// Rename a workspace at its current revision.
    WorkspaceRename {
        /// Workspace to rename.
        workspace_id: String,
        /// Required workspace revision.
        expected_revision: u64,
        /// Replacement user-visible name.
        name: String,
    },
    /// Close a workspace and terminate its owned executions.
    WorkspaceClose {
        /// Workspace to close.
        workspace_id: String,
        /// Required workspace revision.
        expected_revision: u64,
    },
    /// Create a window and its initial execution atomically.
    WindowCreate {
        /// Owning workspace.
        workspace_id: String,
        /// Required workspace revision.
        expected_revision: u64,
        /// User-visible window name, if supplied.
        name: Option<String>,
        /// Initial pane launch specification.
        spec: PaneSpawnSpec,
    },
    /// Rename a window without changing its membership or layout.
    WindowRename {
        /// Owning workspace.
        workspace_id: String,
        /// Window to rename.
        window_id: String,
        /// Required window revision.
        expected_revision: u64,
        /// Replacement user-visible name.
        name: String,
    },
    /// Close a window and terminate its owned executions.
    WindowClose {
        /// Owning workspace.
        workspace_id: String,
        /// Window to close.
        window_id: String,
        /// Required window revision.
        expected_revision: u64,
    },
    /// Atomically create a batch with its complete final layout, or unowned panes.
    PaneSpawn {
        /// Owner; present together with window, revision and layout.
        workspace_id: Option<String>,
        /// Target window within the owner, or absent for unowned panes.
        window_id: Option<String>,
        /// Required target-window revision for owned creation.
        expected_revision: Option<u64>,
        /// Launches keyed by batch-local references.
        panes: Vec<PaneSpawnEntry>,
        /// Complete target-window layout; absent only for unowned creation.
        layout: Option<SpawnLayoutNode>,
    },
    /// Replace an unavailable pane in its owning window; never replay saved argv.
    PaneReopen {
        /// Owning workspace.
        workspace_id: String,
        /// Owning window.
        window_id: String,
        /// Required window revision.
        expected_revision: u64,
        /// Unavailable pane being replaced.
        pane_id: String,
        /// Explicit replacement launch specification.
        spec: PaneSpawnSpec,
    },
    /// Terminate one execution without explicitly removing its layout reference.
    PaneTerminate {
        /// Pane to terminate.
        pane_id: String,
    },
    /// Forget a retired, unreferenced pane with no active views; never terminates a process.
    PaneForget {
        /// Retired pane to forget.
        pane_id: String,
    },
    /// Set the pane's explicit user title.
    PaneRename {
        /// Pane to rename.
        pane_id: String,
        /// Explicit title; takes precedence over observed application titles.
        title: String,
    },
    /// Replace a complete window layout without transferring pane ownership.
    LayoutUpdate {
        /// Owning workspace.
        workspace_id: String,
        /// Window whose layout changes.
        window_id: String,
        /// Required window revision.
        expected_revision: u64,
        /// Complete replacement tree, or `None` to clear the layout only.
        layout: Option<LayoutNode>,
    },
    /// Route a terminal operation to one pane.
    Pane {
        /// Exact pane identity.
        pane_id: String,
        /// Terminal or attachment operation.
        request: PaneRequest,
    },
    /// Wait for a registry revision newer than the supplied cursor.
    Wait {
        /// Last consumed registry revision.
        after: u64,
        /// Maximum wait in milliseconds.
        wait_ms: u64,
    },
    /// Shut down the daemon and its owned executions.
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
/// One launch in an atomic batch, addressable by a caller-chosen reference.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneSpawnEntry {
    /// Unique batch-local key used by [`SpawnLayoutNode::New`].
    pub reference: String,
    /// Explicit execution specification.
    pub spec: PaneSpawnSpec,
}
/// Complete creation-time layout referring to existing panes or new batch entries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpawnLayoutNode {
    /// Retain a pane already owned by the target window.
    Existing {
        /// Existing pane identity.
        pane_id: String,
    },
    /// Place a pane created by this batch.
    New {
        /// Key matching a [`PaneSpawnEntry::reference`].
        reference: String,
    },
    /// Split the region between two children.
    Split {
        /// Direction in which the children are arranged.
        axis: Axis,
        /// First child's share in thousandths; 500 is an equal split.
        #[schemars(range(min = 1, max = 999))]
        ratio: u16,
        /// Child receiving the ratio's share.
        first: Box<SpawnLayoutNode>,
        /// Child receiving the remaining share.
        second: Box<SpawnLayoutNode>,
    },
}
/// Explicit process environment and terminal geometry for a new pane.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneSpawnSpec {
    /// Executable followed by its arguments; no shell is inserted.
    pub argv: Vec<String>,
    /// Initial working directory.
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub cwd: PathBuf,
    /// Home directory supplied to the execution.
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub home: PathBuf,
    /// Initial grid and cell-pixel dimensions.
    pub size: Size,
    /// Explicit initial user title, if supplied.
    #[serde(default)]
    pub title: Option<String>,
    /// Whether to launch without inheriting the daemon's ambient environment.
    #[serde(default)]
    pub isolated_environment: bool,
}
/// Current backend registry, including retained pane records.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    /// Registry-wide publication revision.
    pub revision: u64,
    /// Workspaces and their authoritative windows/layouts.
    pub workspaces: Vec<Workspace>,
    /// Pane records, including ownership and live metadata.
    pub panes: Vec<PaneInfo>,
}
/// Session-owned collection of windows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    /// Opaque workspace identity.
    pub id: String,
    /// User-visible workspace name.
    pub name: String,
    /// Revision required for workspace-level mutations.
    pub revision: u64,
    /// Windows owned by this workspace.
    pub windows: Vec<Window>,
}
/// Backend-owned independent layout. Active-window selection belongs to each client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    /// Opaque window identity.
    pub id: String,
    /// User-visible window name.
    pub name: String,
    /// Revision required for window/layout mutations.
    pub revision: u64,
    /// Authoritative split tree, or no displayed panes.
    pub layout: Option<LayoutNode>,
}
/// Authoritative split tree; every leaf must belong to the target window.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayoutNode {
    /// Display one pane.
    Leaf {
        /// Pane owned by the containing window.
        pane_id: String,
    },
    /// Divide the region between two children.
    Split {
        /// Direction in which the children are arranged.
        axis: Axis,
        /// First child's share in thousandths; 500 is an equal split.
        #[schemars(range(min = 1, max = 999))]
        ratio: u16,
        /// Child receiving the ratio's share.
        first: Box<LayoutNode>,
        /// Child receiving the remaining share.
        second: Box<LayoutNode>,
    },
}
/// Direction in which a split arranges its two children.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Children arranged left to right.
    Horizontal,
    /// Children arranged top to bottom.
    Vertical,
}
/// Backend pane record with execution ownership and transient observed facts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Opaque pane identity.
    pub id: String,
    /// Execution ownership, paired with window_id; both absent only for admin panes.
    pub workspace_id: Option<String>,
    /// Owning window, paired with `workspace_id`.
    pub window_id: Option<String>,
    /// Effective user or observed application title.
    pub title: String,
    /// Execution lifecycle state.
    pub status: PaneStatus,
    /// Process exit code, when known.
    pub exit_code: Option<i32>,
    /// Configured launch working directory.
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    /// Observed live foreground facts; never adopted from durable metadata.
    #[serde(default, with = "crate::path_wire::optional")]
    pub foreground_cwd: Option<PathBuf>,
    /// Current foreground harness observation, if recognized.
    #[serde(default)]
    pub harness: Option<HarnessInfo>,
    /// Backend-selected, identity-scoped OSC fact, including when no view is attached.
    #[serde(default)]
    pub harness_status: Option<crate::ProgramStatus>,
    /// Backend observation time of the selected OSC semantic status transition.
    #[serde(default)]
    pub harness_status_changed_at_ms: Option<u64>,
    /// Live backend cache result for the effective foreground CWD; never restored.
    #[serde(default)]
    pub project: Option<crate::ProjectSnapshot>,
    /// Home directory supplied at launch.
    #[serde(with = "crate::path_wire")]
    pub home: PathBuf,
    /// Whether launch omitted the daemon's ambient environment.
    pub isolated_environment: bool,
}
/// Execution lifecycle, independent of attachment visibility.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaneStatus {
    /// Execution is currently running.
    Running,
    /// Pane is retained without an active execution.
    Stopped,
    /// Execution has exited.
    Exited,
}

/// Native response envelope with optional typed failure metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    /// Native wire family.
    pub version: u32,
    /// Whether dispatch returned success.
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Successful result, when supplied.
    pub result: Option<crate::Payload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Human-readable failure diagnostic.
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Machine-readable failure facts, when available.
    pub failure: Option<RequestFailure>,
}

/// Machine-readable failure facts; an error string alone grants no shutdown semantics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestFailure {
    /// Session closure interrupted or rejected this request.
    SessionClosing {
        /// Whether dispatch may have produced effects.
        outcome: RequestOutcome,
    },
    /// A target cannot currently accept this operation.
    Busy {
        /// Identity of the occupied target.
        target_id: String,
    },
}

/// Whether a failed request is proven not to have reached dispatch.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    /// This specific request did not enter application dispatch.
    NotDispatched,
    /// Dispatch began; failure does not prove that the request had no effects.
    Unknown,
}
/// Registry mutation result with any newly created identities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryResult {
    /// Registry after the mutation.
    pub registry: RegistrySnapshot,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Batch-local launch references mapped to allocated pane identities.
    pub created_panes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Created or affected pane, when applicable.
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Created or affected workspace, when applicable.
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Created or affected window, when applicable.
    pub window_id: Option<String>,
}
