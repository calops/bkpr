use crate::{PROTOCOL_VERSION, PaneRequest, Size};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Inspect,
    WorkspaceCreate {
        name: String,
    },
    WorkspaceRename {
        workspace_id: String,
        expected_revision: u64,
        name: String,
    },
    WorkspaceRemove {
        workspace_id: String,
        expected_revision: u64,
    },
    PaneSpawn {
        workspace_id: Option<String>,
        expected_revision: Option<u64>,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSpawnSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
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
    pub layout: Option<LayoutNode>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Leaf {
        pane_id: String,
    },
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: String,
    /// Backend execution membership; independent of every workspace's projected layout.
    pub workspace_id: Option<String>,
    pub title: String,
    pub status: PaneStatus,
    pub exit_code: Option<i32>,
    pub cwd: PathBuf,
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
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryResult {
    pub registry: RegistrySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}
