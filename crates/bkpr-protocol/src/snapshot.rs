use crate::*;
use serde::{Deserialize, Serialize};

/// Published pane state and optional metadata for the requesting view.
/// `Id` and `State` serialize as strings; `Encodings` as a string sequence.
/// The defaults own decoded metadata; producers may borrow those storage fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot<Id = String, State = String, Encodings = Vec<String>> {
    /// Opaque pane identity.
    pub pane_id: Id,
    /// PTY child process ID, when published.
    pub pid: Option<u32>,
    /// Published frame revision used as the next `frame_after` baseline.
    pub revision: u64,
    /// Change token for polling pane metadata, independent of the frame revision.
    pub snapshot_revision: u64,
    /// Last published native geometry/content, which may lag accepted view policy.
    /// Native transport carries this scene separately from snapshot metadata.
    pub frame: Option<Frame>,
    /// Image generations required by the current frame, including cached resources.
    pub image_refs: Vec<ImageKey>,
    /// Version of the image-generation receipt protocol.
    pub resource_protocol: u32,
    /// PTY input queue state, when the writer exists.
    pub writer: Option<WriterSnapshot<State>>,
    /// Backend lifecycle label, such as running, draining, exited, terminated, or fault.
    pub lifecycle: String,
    /// Published child exit status, when available.
    pub exit_code: Option<u32>,
    /// Pane execution or cleanup diagnostic, when present.
    pub error: Option<String>,
    /// Retained semantic OSC 7501 records, not generic process activity.
    pub program_status: Vec<ProgramStatus>,
    /// Birth-bound foreground identity which owns these OSC records, not generic process status.
    #[serde(default)]
    pub program_identity: Option<ProcessIdentity>,
    /// Retained terminal events newer than the requested event cursor.
    pub events: Vec<Event>,
    /// Latest terminal event sequence, including events no longer retained.
    pub event_sequence: u64,
    /// Current desired lease; not an acknowledgement that the native frame was resized.
    pub size_owner: Option<SizeOwner>,
    /// Generation of accepted view and sizing authority changes.
    pub lease_generation: u64,
    /// Currently retained view attachments; their presence does not own pane lifetime.
    pub views: Vec<ViewInfo>,
    /// Pending clipboard request only when assigned to the requesting view.
    pub clipboard: Option<ClipboardPending>,
    /// Whether any clipboard request is pending, even for another view.
    pub clipboard_pending: bool,
    /// Supported pane features and request time limits.
    pub capabilities: PaneCapabilities<Encodings>,
    /// View-local metadata, independent of canonical scene revisions and resources.
    #[serde(default)]
    pub selection: Option<SelectionSnapshot>,
    /// View-local search progress and visible match spans.
    #[serde(default)]
    pub search: Option<SearchSnapshot>,
}
/// Sequenced terminal effect retained in the bounded pane event history.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// Monotonic pane-local event cursor.
    pub sequence: u64,
    /// Terminal observation; PTY response bytes are routed separately by the backend.
    pub effect: Effect,
}
/// Clipboard request assigned to one view for a policy response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipboardPending {
    /// Opaque correlation required by the clipboard reply.
    pub request_id: String,
    /// View authorized to answer this request.
    pub view_id: String,
    /// Requested transfer and native grant context.
    pub request: ClipboardRequest,
}
/// Desired geometry leased to one eligible view, not proof of native resize.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SizeOwner {
    /// View holding the current size lease.
    pub view_id: String,
    /// Requested canonical terminal geometry.
    pub size: Size,
}
/// Attachment policy and view-local inspection position.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewInfo {
    /// Opaque view attachment identity.
    pub id: String,
    /// Accepted visibility, sizing and independent capability policy.
    pub spec: ViewSpec,
    /// Monotonic attachment/update order used for size and clipboard arbitration.
    pub activity: u64,
    /// Distance above live output in rows; zero follows live output.
    pub scrollback_offset: u64,
}
/// Bounded PTY input queue accounting and retained write receipts.
/// `State` is string storage shared with [`WriteReceipt`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriterSnapshot<State = String> {
    /// Full byte lengths of unfinished packets, including the packet being written.
    pub queued_bytes: usize,
    /// Number of unfinished packets, including the packet being written.
    pub active: usize,
    /// Retained per-packet progress; older completed receipts may be evicted.
    pub receipts: Vec<WriteReceipt<State>>,
    /// Whether the writer is stopping; unfinished packets are cancelled.
    pub stopped: bool,
    /// PTY writer failure diagnostic, when present.
    pub error: Option<String>,
}
/// Progress writing an accepted packet to the PTY, not application consumption.
/// `State` must serialize as a string; the default owns decoded state labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WriteReceipt<State = String> {
    /// Pane-local packet identifier returned when queued.
    pub id: u64,
    /// Queue state: queued, writing, written, cancelled, or failed.
    pub state: State,
    /// Total bytes accepted for this packet.
    pub total: usize,
    /// Bytes accepted by PTY writes so far; not proof of application effects.
    pub written: usize,
}
/// Pane feature support and backend-advertised timeout limits.
/// `Encodings` must serialize as a sequence of encoding names.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneCapabilities<Encodings = Vec<String>> {
    /// Whether Sixel image output is supported.
    pub sixel: bool,
    /// Whether canonical terminal frames are available.
    pub normalized_frames: bool,
    /// Frame mouse-reporting facts come from the native terminal, not defaults.
    #[serde(default)]
    pub native_mouse_reporting: bool,
    /// Maximum wait for a clipboard policy response, in milliseconds.
    pub clipboard_timeout_ms: u64,
    /// Idle expiry for unowned view attachments, in milliseconds.
    pub view_timeout_ms: u64,
    /// Maximum server-side poll wait, in milliseconds.
    pub poll_wait_max_ms: u64,
    /// Whether frame updates can carry row deltas.
    pub row_deltas: bool,
    /// Supported binary scene encoding names.
    pub scene_encodings: Encodings,
}

/// Identity returned for a newly attached view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachResult {
    /// Opaque attachment ID used for subsequent view-scoped operations.
    pub view_id: String,
}

/// View-local selection state and visible highlighting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    /// Whether a selection currently exists.
    pub active: bool,
    /// Whether the selection is being dragged.
    pub dragging: bool,
    /// Whether selection uses rectangular rather than linear geometry.
    pub rectangle: bool,
    /// Sorted by row, with at most one nonempty, nonoverlapping span per visible row.
    pub spans: Vec<SelectionSpan>,
    /// -1=up, 0=none, 1=down. Only meaningful during an active drag.
    pub autoscroll: i8,
}

/// Nonempty selection interval in zero-based visible cell coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSpan {
    /// Zero-based viewport row.
    pub row: u16,
    /// Inclusive starting column.
    pub start: u16,
    /// Exclusive endpoint, bounded by the visible terminal columns.
    pub end: u16,
}

/// Native literal matching is byte-exact except ASCII case-insensitivity.
/// Counts cover the active screen and are provisional while running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSnapshot {
    /// Literal query text retained for this view.
    pub query: String,
    /// Monotonic query-change generation, unchanged by match navigation.
    pub generation: u64,
    /// Whether native matching has more work to perform.
    pub running: bool,
    /// Number of matches found so far on the active screen, including retained history.
    pub total_matches: u64,
    /// Zero-based, newest-to-oldest native ordering.
    pub selected_index: Option<u64>,
    /// Visible match intervals, sorted by row and column; matches may overlap.
    pub spans: Vec<SearchSpan>,
}

/// Visible portion of a search match in zero-based cell coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSpan {
    /// Zero-based viewport row.
    pub row: u16,
    /// Inclusive starting column.
    pub start: u16,
    /// Exclusive ending column, bounded by the visible terminal columns.
    pub end: u16,
    /// Whether this interval belongs to the selected native match.
    pub selected: bool,
}
