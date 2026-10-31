use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub pane_id: String,
    pub pid: Option<u32>,
    pub revision: u64,
    pub snapshot_revision: u64,
    /// Last published native geometry/content, which may lag accepted view policy.
    pub frame: Option<Frame>,
    pub image_refs: Vec<ImageKey>,
    pub resource_protocol: u32,
    pub writer: Option<WriterSnapshot>,
    pub lifecycle: String,
    pub exit_code: Option<u32>,
    pub error: Option<String>,
    pub program_status: Vec<ProgramStatus>,
    /// Birth-bound foreground identity which owns these OSC records, not generic process status.
    #[serde(default)]
    pub program_identity: Option<ProcessIdentity>,
    pub events: Vec<Event>,
    pub event_sequence: u64,
    /// Current desired lease; not an acknowledgement that the native frame was resized.
    pub size_owner: Option<SizeOwner>,
    pub lease_generation: u64,
    pub views: Vec<ViewInfo>,
    pub clipboard: Option<ClipboardPending>,
    pub clipboard_pending: bool,
    pub capabilities: PaneCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_delta: Option<RowDelta>,
    /// Versioned compact scene; decode using scene_wire::decode before applying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub effect: Effect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardPending {
    pub request_id: String,
    pub view_id: String,
    pub request: ClipboardRequest,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SizeOwner {
    pub view_id: String,
    pub size: Size,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewInfo {
    pub id: String,
    pub spec: ViewSpec,
    pub activity: u64,
    pub scrollback_offset: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriterSnapshot {
    pub queued_bytes: usize,
    pub active: usize,
    pub receipts: Vec<WriteReceipt>,
    pub stopped: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriteReceipt {
    pub id: u64,
    pub state: String,
    pub total: usize,
    pub written: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneCapabilities {
    pub sixel: bool,
    pub normalized_frames: bool,
    pub clipboard_timeout_ms: u64,
    pub view_timeout_ms: u64,
    pub poll_wait_max_ms: u64,
    pub row_deltas: bool,
    pub scene_encodings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RowDelta {
    pub base_revision: u64,
    pub rows: Vec<(usize, Row)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachResult {
    pub view_id: String,
}
