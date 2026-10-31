//! Live execution and exact native conversation facts. Capture capabilities are never pane metadata.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Claude,
    Codex,
    Omp,
    Agy,
    Opencode,
    Cursor,
    Copilot,
    Aider,
    Cline,
    Goose,
}
impl HarnessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
            Self::Agy => "agy",
            Self::Opencode => "opencode",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Aider => "aider",
            Self::Cline => "cline",
            Self::Goose => "goose",
        }
    }
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Omp => "Omp",
            Self::Agy => "Antigravity",
            Self::Opencode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::Copilot => "Copilot",
            Self::Aider => "Aider",
            Self::Cline => "Cline",
            Self::Goose => "Goose",
        }
    }
    pub fn from_command(command: &str) -> Option<Self> {
        let name = Path::new(command).file_name()?.to_str()?;
        let name = name
            .strip_prefix('.')
            .and_then(|n| n.strip_suffix("-wrapped"))
            .unwrap_or(name);
        match name {
            "claude" | "claude-code" | "claude.exe" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "omp" => Some(Self::Omp),
            "agy" | "antigravity" => Some(Self::Agy),
            "opencode" => Some(Self::Opencode),
            "cursor" | "cursor-agent" => Some(Self::Cursor),
            "copilot" | "copilot-cli" | "github-copilot" => Some(Self::Copilot),
            "aider" => Some(Self::Aider),
            "cline" | ".cline" => Some(Self::Cline),
            "goose" => Some(Self::Goose),
            _ => None,
        }
    }
    pub fn is_managed(self) -> bool {
        matches!(self, Self::Claude | Self::Codex | Self::Omp | Self::Agy)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSource {
    ClaudeHook,
    OmpExtension,
    CodexStart,
    CodexResume,
    CodexFork,
    AgyMcp,
}
impl ConversationSource {
    pub fn harness(self) -> HarnessKind {
        match self {
            Self::ClaudeHook => HarnessKind::Claude,
            Self::OmpExtension => HarnessKind::Omp,
            Self::CodexStart | Self::CodexResume | Self::CodexFork => HarnessKind::Codex,
            Self::AgyMcp => HarnessKind::Agy,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeConversation {
    pub id: String,
    pub source: ConversationSource,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedHarness {
    pub launch_id: String,
    pub conversation: Option<NativeConversation>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessInfo {
    pub kind: HarnessKind,
    pub pid: u32,
    pub birth: String,
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    pub managed: Option<ManagedHarness>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureGrant {
    pub launch_id: String,
    pub token: String,
    pub pane_id: String,
    pub kind: HarnessKind,
}
// Capability-bearing types redact secrets even in accidental debug output.
impl std::fmt::Debug for CaptureGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureGrant")
            .field("launch_id", &self.launch_id)
            .field("token", &"[REDACTED]")
            .field("pane_id", &self.pane_id)
            .field("kind", &self.kind)
            .finish()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    Bind {
        conversation_id: String,
        source: ConversationSource,
    },
    Error {
        message: String,
    },
    End,
}
