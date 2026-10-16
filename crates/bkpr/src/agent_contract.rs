//! Native process inspection and terminal-owned program status contracts.
use serde::{Deserialize, Serialize};

use crate::{Attention, Status};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneProcess {
    pub pane_id: u32,
    pub root_pid: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectRequest {
    pub panes: Vec<PaneProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExecution {
    pub pane_id: u32,
    pub root_pid: i32,
    pub pid: i32,
    /// OS process-birth identity, never a transcript or a command-name identity.
    pub identity: String,
    pub harness: String,
    pub cwd: Option<String>,
    /// Bkpr relay launch ID, only when present in the foreground harness environment.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Display name only for a recognized harness, never an arbitrary OSC app id.
pub fn harness_name(harness: &str) -> Option<&'static str> {
    Some(match harness {
        "claude" | "claude-code" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "omp" => "omp",
        "agy" | "antigravity" => "Antigravity",
        "cursor" | "cursor-agent" => "Cursor Agent",
        "copilot" | "copilot-cli" | "github-copilot" => "GitHub Copilot CLI",
        "aider" => "Aider",
        "cline" => "Cline CLI",
        "goose" => "Goose",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    /// Empty id is the terminal's root record; child apps are resolved by Zellij.
    pub id: String,
    pub state: String,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

impl ProgramStatus {
    pub fn status(&self) -> Status {
        match self.state.as_str() {
            "idle" => Status::Idle,
            "working" => Status::Working,
            "done" => Status::Done,
            "blocked" | "error" => Status::Blocked,
            _ => Status::Unknown,
        }
    }

    pub fn attention(&self) -> Option<Attention> {
        match self.state.as_str() {
            "error" => Some(Attention::Error),
            "blocked" => match self.kind.as_deref() {
                Some("permission") => Some(Attention::Permission),
                Some("question") => Some(Attention::Question),
                Some("auth") => Some(Attention::Auth),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatusUpdate {
    pub pane_id: u32,
    /// A full terminal snapshot; empty records explicitly clear its status.
    pub records: Vec<ProgramStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectResponse {
    /// Only currently live, recognized foreground agent executions are included.
    pub executions: Vec<AgentExecution>,
}
