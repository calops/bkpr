//! Versioned companion/frontend contracts. Terminal bytes never cross this interface.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}

impl Status {
    pub fn priority(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Working => 3,
            Self::Done => 2,
            Self::Idle => 1,
            Self::Unknown => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub active_tab_id: String,
    pub projects: Vec<String>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: String,
    pub description: String,
    pub harness: Option<String>,
    pub status: Status,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    /// Identifies this companion process; revisions are ordered only within it.
    pub instance_id: String,
    /// Companion-local revision, not a Herdr event sequence or recovery cursor.
    pub revision: u64,
    pub session: String,
    pub machine: Option<String>,
    pub connected: bool,
    pub error: Option<String>,
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<Workspace>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    OpenPane {
        pane_id: String,
        command: CommandSpec,
    },
    FocusPane {
        zellij_pane_id: u32,
    },
    /// Resolved executable for the native attachment process, not for the plugin.
    Attachment {
        command: CommandSpec,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Snapshot,
    SelectWorkspace {
        workspace_id: String,
    },
    SelectPane {
        pane_id: String,
    },
    FocusPane {
        pane_id: String,
    },
    CreateWorkspace {
        label: Option<String>,
        cwd: Option<String>,
    },
    CreatePane {
        workspace_id: String,
        cwd: Option<String>,
    },
    RenameWorkspace {
        workspace_id: String,
        label: String,
    },
    CloseWorkspace {
        workspace_id: String,
    },
    RenamePane {
        pane_id: String,
        label: String,
    },
    ClosePane {
        pane_id: String,
    },
    RegisterPane {
        pane_id: String,
        zellij_pane_id: u32,
    },
    PaneClosed {
        zellij_pane_id: u32,
    },
    ResolveAttachment {
        pane_id: String,
    },
    /// Enables targeted state publication only after the real sidebar has loaded.
    RegisterFrontend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub request_id: u64,
    pub action: Action,
}

impl Request {
    pub fn new(request_id: u64, action: Action) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id,
            action,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub request_id: u64,
    pub state: Option<Snapshot>,
    pub effects: Vec<Effect>,
    pub error: Option<String>,
}
