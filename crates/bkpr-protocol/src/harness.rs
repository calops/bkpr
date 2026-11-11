//! Live execution and exact native conversation facts. Capture capabilities are never pane metadata.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Recognized harness executable family, independent of status or capture readiness.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    /// Anthropic Claude Code.
    Claude,
    /// OpenAI Codex.
    Codex,
    /// Omp coding agent.
    Omp,
    /// Antigravity's native harness.
    Agy,
    /// OpenCode coding agent.
    Opencode,
    /// Cursor's command-line agent.
    Cursor,
    /// GitHub Copilot CLI.
    Copilot,
    /// Aider coding agent.
    Aider,
    /// Cline command-line agent.
    Cline,
    /// Goose coding agent.
    Goose,
}
impl HarnessKind {
    /// Stable lowercase identifier used in configuration and launch arguments.
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
    /// Human-facing harness label.
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
    /// Recognize an executable basename, including supported aliases and Nix wrappers.
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
    /// Whether this family supports Bkpr-managed launch and capture integration.
    pub fn is_managed(self) -> bool {
        matches!(self, Self::Claude | Self::Codex | Self::Omp | Self::Agy)
    }
}
/// Native integration that authenticated a conversation binding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSource {
    /// Claude's native hook callback.
    ClaudeHook,
    /// Omp's loaded extension.
    OmpExtension,
    /// A newly started Codex conversation.
    CodexStart,
    /// A resumed Codex conversation.
    CodexResume,
    /// A forked Codex conversation.
    CodexFork,
    /// Antigravity's MCP conversation handshake.
    AgyMcp,
}
impl ConversationSource {
    /// Harness family permitted to report this source.
    pub fn harness(self) -> HarnessKind {
        match self {
            Self::ClaudeHook => HarnessKind::Claude,
            Self::OmpExtension => HarnessKind::Omp,
            Self::CodexStart | Self::CodexResume | Self::CodexFork => HarnessKind::Codex,
            Self::AgyMcp => HarnessKind::Agy,
        }
    }
}
/// Exact native conversation bound to one managed launch; not receiver readiness.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeConversation {
    /// Harness-native conversation ID, distinct from the Bkpr launch ID.
    pub id: String,
    /// Authenticated integration that supplied this binding.
    pub source: ConversationSource,
    /// Launch-local binding generation, advanced when the ID or source changes.
    pub generation: u64,
}
/// Live managed-launch facts, excluding private capture capabilities.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedHarness {
    /// Fresh launch identity; restarting a conversation creates a different launch.
    pub launch_id: String,
    /// Current authenticated native binding, absent until capture succeeds.
    pub conversation: Option<NativeConversation>,
    /// Latest capture diagnostic; successful binding clears it.
    pub error: Option<String>,
    /// First authenticated native prompt submission observed in this launch.
    /// Unix milliseconds; absent means unknown, not that no prompt was submitted.
    #[serde(default)]
    pub first_prompt_submitted_at_ms: Option<u64>,
}
/// Observed foreground harness process and any associated managed-launch facts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessInfo {
    /// Recognized executable family, not semantic activity status.
    pub kind: HarnessKind,
    /// Observed harness process ID.
    pub pid: u32,
    /// Opaque process-birth identity used with the PID to reject reuse.
    pub birth: String,
    /// Observed process working directory, preserving native path bytes.
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    /// Associated managed launch, absent for recognized unmanaged executions.
    pub managed: Option<ManagedHarness>,
}
/// Process identity qualified by birth to distinguish reused PIDs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Operating-system process ID.
    pub pid: u32,
    /// Opaque birth identity obtained from the operating system.
    pub birth: String,
}
/// Private launch capability; callbacks also require verified process ancestry.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureGrant {
    /// Launch to which this capability belongs.
    pub launch_id: String,
    /// Secret authentication token; never persist or publish it as pane metadata.
    pub token: String,
    /// Pane containing the authorized execution.
    pub pane_id: String,
    /// Harness family authorized to report capture events.
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
/// Authenticated native capture updates; these do not report semantic harness status.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    /// Bind this launch to an exact native conversation.
    Bind {
        /// Nonblank native ID, at most 1024 UTF-8 bytes without controls.
        conversation_id: String,
        /// Integration matching the launch's harness family.
        source: ConversationSource,
    },
    /// A body-free native event for the currently bound conversation.
    PromptSubmitted {
        /// Exact ID of the launch's currently bound conversation.
        conversation_id: String,
        /// Integration matching the current conversation binding.
        source: ConversationSource,
    },
    /// Publish a capture diagnostic without establishing a conversation binding.
    Error {
        /// Nonblank safe diagnostic, at most 2048 UTF-8 bytes without controls.
        message: String,
    },
    /// Revoke this launch's capture and control authority without terminating its pane.
    End,
}
