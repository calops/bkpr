//! Launch-authenticated discovery, durable messaging and native wake coordination.
use serde::{Deserialize, Serialize};

/// Reach of discovery, messaging or supported control within the current daemon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScope {
    /// Foreground launches in the caller's current workspace.
    #[default]
    Workspace,
    /// Foreground launches in the caller's current session.
    Session,
    /// Launches sharing the caller's canonical common Git repository.
    Repository,
    /// All eligible launches in this daemon, including background launches.
    All,
}
/// Whether durable message acceptance also requests a native wake.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelivery {
    /// Request a wake when a current native receiver is subscribed.
    #[default]
    Notify,
    /// Store the message without requesting a wake.
    Quiet,
}
/// Native turn boundary at which the receiver can surface an inbox pointer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReceiverMode {
    /// The receiver can steer an already-running turn.
    ActiveTurn,
    /// The receiver delivers at a subsequent turn boundary.
    FutureTurn,
}
/// Result of native wake dispatch, independent of message read or processing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentWakeOutcome {
    /// The native integration confirmed acceptance of the wake.
    Accepted,
    /// Dispatch failed without confirmed acceptance.
    Failed {
        /// Safe diagnostic text, at most 2048 UTF-8 bytes with no control characters.
        reason: String,
    },
    /// Dispatch may have taken effect; callers must not blindly replay it.
    Unknown {
        /// Safe diagnostic text, at most 2048 UTF-8 bytes with no control characters.
        reason: String,
    },
}
/// Requests authorized by a live managed launch and its authenticated process.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRequest {
    /// Inspect the caller's launch, conversation binding and receiver readiness.
    Whoami {
        /// Include expanded pane context and project metadata.
        #[serde(default)]
        detail: bool,
    },
    /// Shared typed control; implementation allowlists supported operations.
    Control {
        /// Allowlisted native operation; not every registry operation is available.
        request: Box<crate::Operation>,
        /// Context boundary for the operation; repository scope is unsupported.
        #[serde(default)]
        scope: AgentScope,
        /// Amount of result detail returned for mutations.
        #[serde(default)]
        reply: crate::control::ControlReply,
    },
    /// Change the launch's discovery label without renaming its pane.
    SetTitle {
        /// Label of at most 160 UTF-8 bytes, without controls or the launch token.
        title: String,
    },
    /// Page live launches without treating discovery as communication readiness.
    DiscoverAgents {
        /// Boundary relative to the caller's current execution context.
        #[serde(default)]
        scope: AgentScope,
        /// Include expanded pane context and project metadata.
        #[serde(default)]
        detail: bool,
        /// Restrict results to this exact opaque launch ID.
        #[serde(default)]
        launch_id: Option<String>,
        /// Restrict results to this exact opaque pane ID.
        #[serde(default)]
        pane_id: Option<String>,
        /// Opaque cursor returned by the preceding discovery page.
        #[serde(default)]
        after: Option<String>,
        /// Requested page size, subject to service limits.
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Durably send untrusted peer text to one exact launch.
    SendMessage {
        /// Recipient launch ID, never a native conversation ID.
        to: String,
        /// Nonblank UTF-8 body of at most 65536 bytes, excluding the launch token.
        text: String,
        /// Nonempty key of at most 256 bytes; retries must preserve all send fields.
        idempotency_key: String,
        /// Required relationship between sender and recipient.
        #[serde(default)]
        scope: AgentScope,
        /// Whether acceptance should also request a native wake.
        #[serde(default)]
        delivery: AgentDelivery,
    },
    /// Page pending inbox summaries and mark returned messages read, not processed.
    ReadInbox {
        /// Message cursor returned by the preceding inbox page.
        #[serde(default)]
        after: Option<String>,
        /// Requested page size, subject to service limits.
        #[serde(default)]
        limit: Option<usize>,
        /// Previously read message IDs the caller has now processed.
        #[serde(default)]
        ack_ids: Vec<String>,
        /// Include already-read messages that remain unacknowledged.
        #[serde(default)]
        include_read: bool,
    },
    /// Read a UTF-8 body page without acknowledging processing.
    ReadMessage {
        /// Exact message ID from this launch's inbox.
        message_id: String,
        /// UTF-8 byte boundary within the body; defaults to zero.
        #[serde(default)]
        offset: Option<usize>,
        /// Requested body-page byte limit, from 4 through 4096.
        #[serde(default)]
        max_bytes: Option<usize>,
    },
    /// Acknowledge processed inbox messages and discard their retained bodies.
    AckMessage {
        /// Recipient-owned message IDs; repeated acknowledgements are harmless.
        ids: Vec<String>,
    },
    /// Inspect sender-owned delivery receipts without returning message bodies.
    InspectOutbox {
        /// Select one exact message instead of paging the outbox.
        #[serde(default)]
        message_id: Option<String>,
        /// Message cursor returned by the preceding outbox page.
        #[serde(default)]
        after: Option<String>,
        /// Requested page size, subject to service limits.
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Register a native receiver; registration alone does not establish readiness.
    ReceiverRegister {
        /// Currently bound native conversation ID, not the Bkpr session ID.
        session_id: String,
        /// Turn boundary supported by this receiver; Codex requires future-turn mode.
        mode: AgentReceiverMode,
        /// Native receiver PID; zero selects the launch root's child on callback ancestry.
        pid: u32,
    },
    /// Wait for an inbox wake claim using the current receiver registration.
    ReceiverSubscribe {
        /// Opaque ID returned by receiver registration.
        receiver_id: String,
        /// Maximum wait in milliseconds, capped at 25000 by the service.
        wait_ms: u64,
    },
    /// Check that a claimed notification is still pending before native dispatch.
    WakeValidate {
        /// Current receiver registration that owns the claim.
        receiver_id: String,
        /// Exact notification ID returned by subscription.
        notification_id: String,
        /// Notification generation, distinct from conversation generation.
        generation: u64,
    },
    /// Record native dispatch independently of message read and processed receipts.
    WakeOutcome {
        /// Receiver registration that owns the notification claim.
        receiver_id: String,
        /// Exact notification ID returned by subscription.
        notification_id: String,
        /// Notification generation returned with the claim.
        generation: u64,
        /// Confirmed, failed or ambiguous native dispatch result.
        outcome: AgentWakeOutcome,
    },
}
