use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScope {
    #[default]
    Workspace,
    Session,
    Repository,
    All,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelivery {
    #[default]
    Notify,
    Quiet,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReceiverMode {
    ActiveTurn,
    FutureTurn,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentWakeOutcome {
    Accepted,
    Failed { reason: String },
    Unknown { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentRequest {
    Whoami {
        #[serde(default)]
        detail: bool,
    },
    /// Shared typed control; implementation allowlists supported operations.
    Control {
        request: Box<crate::Operation>,
        #[serde(default)]
        scope: AgentScope,
        #[serde(default)]
        reply: crate::control::ControlReply,
    },
    SetTitle {
        title: String,
    },
    DiscoverAgents {
        #[serde(default)]
        scope: AgentScope,
        #[serde(default)]
        detail: bool,
        #[serde(default)]
        launch_id: Option<String>,
        #[serde(default)]
        pane_id: Option<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    SendMessage {
        to: String,
        text: String,
        idempotency_key: String,
        #[serde(default)]
        scope: AgentScope,
        #[serde(default)]
        delivery: AgentDelivery,
    },
    ReadInbox {
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        ack_ids: Vec<String>,
        #[serde(default)]
        include_read: bool,
    },
    ReadMessage {
        message_id: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        max_bytes: Option<usize>,
    },
    AckMessage {
        ids: Vec<String>,
    },
    InspectOutbox {
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    ReceiverRegister {
        session_id: String,
        mode: AgentReceiverMode,
        pid: u32,
    },
    ReceiverSubscribe {
        receiver_id: String,
        wait_ms: u64,
    },
    WakeValidate {
        receiver_id: String,
        notification_id: String,
        generation: u64,
    },
    WakeOutcome {
        receiver_id: String,
        notification_id: String,
        generation: u64,
        outcome: AgentWakeOutcome,
    },
}
