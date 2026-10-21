//! Live native session context; authority tokens are confined to internal publications.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub session_id: String,
    pub session_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneContext {
    pub session_id: String,
    pub session_name: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub pane_id: String,
    pub pane_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub pane_name: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextWorkspace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPublication {
    pub session_id: String,
    pub session_name: String,
    pub server: i32,
    pub owner: u32,
    pub sequence: u64,
    pub token: String,
    pub workspaces: Vec<ContextWorkspace>,
    pub panes: Vec<ContextPane>,
}
