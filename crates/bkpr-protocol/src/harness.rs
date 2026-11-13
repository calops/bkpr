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
    /// Pi coding agent.
    Pi,
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
            Self::Pi => "pi",
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
            Self::Pi => "Pi",
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
            "opencode" | "opencode2" => Some(Self::Opencode),
            "cursor" | "cursor-agent" => Some(Self::Cursor),
            "copilot" | "copilot-cli" | "github-copilot" => Some(Self::Copilot),
            "aider" => Some(Self::Aider),
            "cline" | ".cline" => Some(Self::Cline),
            "goose" => Some(Self::Goose),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }
    /// Whether this family is admitted by the managed capture protocol.
    pub fn is_managed(self) -> bool {
        matches!(
            self,
            Self::Claude | Self::Codex | Self::Omp | Self::Agy | Self::Opencode | Self::Pi
        )
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
    /// OpenCode's launch-local native plugin.
    OpencodePlugin,
    /// Pi's launch-local native extension.
    PiExtension,
}
impl ConversationSource {
    /// Harness family permitted to report this source.
    pub fn harness(self) -> HarnessKind {
        match self {
            Self::ClaudeHook => HarnessKind::Claude,
            Self::OmpExtension => HarnessKind::Omp,
            Self::CodexStart | Self::CodexResume | Self::CodexFork => HarnessKind::Codex,
            Self::AgyMcp => HarnessKind::Agy,
            Self::OpencodePlugin => HarnessKind::Opencode,
            Self::PiExtension => HarnessKind::Pi,
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
    /// Launch-local binding generation, advanced for each replacement or rebind.
    pub generation: u64,
}
/// Exact launch binding state used by conditional capture mutations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeBindingSnapshot {
    /// Monotonic launch-local revision, including unbound enrollment epochs.
    pub revision: u64,
    /// Current root binding, absent when the launch is unbound.
    pub conversation: Option<NativeConversation>,
}
/// Exact native root and attested caller context required for one agent request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBindingGuard {
    /// Current native root binding, including its launch-local generation.
    pub conversation: NativeConversation,
    /// Actual native MCP caller session, either the root or an attested child.
    pub context_id: String,
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
/// Authenticated launch facts and volatile native semantic-status updates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    /// Bind this launch to an exact native conversation.
    Bind {
        /// Nonblank native ID, at most 1024 UTF-8 bytes without controls.
        conversation_id: String,
        /// Integration matching the launch's harness family.
        source: ConversationSource,
        /// Expected launch binding revision; managed OpenCode producers must supply it.
        /// A successful conditional bind advances the revision even for the same root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<u64>,
    },
    /// Withdraw this exact binding snapshot while retaining launch and saved recovery.
    /// An unbound snapshot reserves a new enrollment epoch by advancing its revision.
    Unbind {
        /// Exact current root, or none when reserving an unbound enrollment epoch.
        conversation: Option<NativeConversation>,
        /// Required launch binding revision; zero is a valid initial revision.
        expected_revision: u64,
    },
    /// Attest a native child context for the exact current root generation.
    NativeContext {
        /// Current root binding authorizing this context.
        conversation: NativeConversation,
        /// Actual native caller session ID, subject to native ID bounds.
        context_id: String,
    },
    /// A body-free native event for the currently bound conversation.
    PromptSubmitted {
        /// Exact ID of the launch's currently bound conversation.
        conversation_id: String,
        /// Integration matching the current conversation binding.
        source: ConversationSource,
    },
    /// Enroll the authenticated originating process for the current native binding.
    /// Returns its frozen generation; enrollment alone publishes no status.
    StatusEnroll {
        /// Exact current native conversation ID.
        conversation_id: String,
        /// Integration matching the current binding and harness.
        source: ConversationSource,
    },
    /// Publish a volatile status using the originating process's enrolled authority.
    /// Acceptance is not durable storage, receiver readiness or a terminal write.
    StatusReport {
        /// Exact native conversation ID captured at enrollment.
        conversation_id: String,
        /// Integration captured at enrollment.
        source: ConversationSource,
        /// Frozen binding generation returned by enrollment.
        generation: u64,
        /// Minimal semantic state; application and record identity are backend-owned.
        status: NativeStatus,
    },
    /// Publish a capture diagnostic without establishing a conversation binding.
    Error {
        /// Nonblank safe diagnostic, at most 2048 UTF-8 bytes without controls.
        message: String,
    },
    /// Revoke this launch's capture and control authority without terminating its pane.
    End,
}

/// Minimal semantic state accepted from an authenticated native integration.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeStatusState {
    /// Waiting without active work.
    Idle,
    /// Performing work.
    Working,
    /// Completed the current work.
    Done,
    /// Waiting for user action.
    Blocked,
    /// Failed the current work.
    Error,
    /// Remove the native observation and restore independent OSC status precedence.
    Clear,
}
impl NativeStatusState {
    /// Stable public semantic state identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }
}

/// User action needed by a blocked native integration.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeStatusKind {
    /// A permission decision is required.
    Permission,
    /// An answer is required.
    Question,
    /// Authentication is required.
    Auth,
}
impl NativeStatusKind {
    /// Stable public blocked-kind identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

/// Bounded native status payload, without caller-supplied application or record IDs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeStatus {
    /// Semantic activity or an explicit removal of this native observation.
    pub state: NativeStatusState,
    /// Optional action classification, valid only for a blocked state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NativeStatusKind>,
}
impl NativeStatus {
    /// Reject action classifications on nonblocked states.
    pub fn validate(self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.kind.is_none() || self.state == NativeStatusState::Blocked,
            "native status kind requires blocked state"
        );
        Ok(())
    }
}
