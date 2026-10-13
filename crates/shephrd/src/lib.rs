//! Versioned workspace models and semantic contracts. Terminal bytes never cross this interface.

use serde::{Deserialize, Serialize};

pub mod agent_contract;
pub mod ci_contract;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    Permission,
    Question,
    Auth,
    Error,
}

impl Attention {
    pub fn label(self) -> &'static str {
        match self {
            Self::Permission => "Permission approval",
            Self::Question => "Question",
            Self::Auth => "Authentication",
            Self::Error => "Error",
        }
    }

    /// Mixed or unspecified attention must not claim a single reason.
    pub fn aggregate(mut reasons: impl Iterator<Item = Option<Self>>) -> Option<Self> {
        let first = reasons.next().flatten()?;
        reasons.all(|reason| reason == Some(first)).then_some(first)
    }
}

/// Stable source identity, independent of a repository's display name or worktree branch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub id: String,
    pub name: String,
    pub repository: bool,
}

/// Line counts for a committed tree diff. Binary changes have no line counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub added: u64,
    pub deleted: u64,
    pub binary_files: u32,
}

/// Entire committed branch compared with its locally resolved base's merge-base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchChanges {
    pub base_ref: String,
    pub merge_base: String,
    pub total: DiffStat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub active_tab_id: String,
    pub projects: Vec<ProjectIdentity>,
    pub status: Status,
    pub attention: Option<Attention>,
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
    pub attention: Option<Attention>,
    pub cwd: Option<String>,
    /// Git branch at the pane's current directory, or `detached @ <short SHA>`.
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub branch_changes: Option<BranchChanges>,
    #[serde(default)]
    pub project: Option<ProjectIdentity>,
    #[serde(default)]
    pub ci: Option<ci_contract::CiSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    /// Identifies the authoritative state owner; revisions are ordered only within it.
    pub instance_id: String,
    /// Owner-local revision, independent of terminal event counters and recovery cursors.
    pub revision: u64,
    pub session: String,
    pub connected: bool,
    pub error: Option<String>,
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<Workspace>,
    pub panes: Vec<Pane>,
}

/// Native tabs contain ordinary Zellij-owned terminals, including stock-created tabs.
pub fn native_layout(
    plugin_url: &str,
    configuration: &std::collections::BTreeMap<String, String>,
    tab_name: &str,
    cwd: &str,
    sidebar_width: usize,
) -> Result<String, serde_json::Error> {
    use std::fmt::Write;

    let quote = |value: &str| serde_json::to_string(value);
    let mut sidebar = format!(
        "   pane size={sidebar_width} borderless=true {{\n    plugin location={} {{\n",
        quote(plugin_url)?
    );
    for (key, value) in configuration {
        writeln!(sidebar, "     {key} {}", quote(value)?).unwrap();
    }
    sidebar.push_str("    }\n   }\n");
    Ok(format!(
        "layout {{\n default_tab_template split_direction=\"vertical\" {{\n{sidebar}   children\n }}\n tab name={} focus=true {{\n  pane cwd={} focus=true\n }}\n}}\n",
        quote(tab_name)?,
        quote(cwd)?,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    FocusPane { zellij_pane_id: u32 },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_branch_is_optional_on_the_wire_and_independent_of_activity() {
        let mut value = serde_json::json!({
            "id": "terminal_1",
            "terminal_id": "terminal_1",
            "workspace_id": "workspace_1",
            "tab_id": "tab_1",
            "name": "Agent",
            "description": "Reading source files",
            "harness": "codex",
            "status": "working",
            "attention": null,
            "cwd": "/project"
        });
        let legacy: Pane = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(legacy.branch, None);
        assert_eq!(legacy.project, None);
        assert_eq!(legacy.ci, None);
        assert_eq!(legacy.description, "Reading source files");

        for branch in [Some("main"), Some("detached @ a1b2c3d"), None] {
            value["branch"] = serde_json::json!(branch);
            let pane: Pane = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(pane.branch.as_deref(), branch);
            assert_eq!(pane.description, legacy.description);
            let round_trip: Pane =
                serde_json::from_value(serde_json::to_value(&pane).unwrap()).unwrap();
            assert_eq!(round_trip, pane);
        }
    }
}
