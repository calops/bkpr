//! Canonical terminal scenes and versioned backend registry protocol.
#![deny(missing_docs)]

mod agent;
pub use agent::*;
mod ci;
mod context;
pub mod control;
pub use ci::*;
pub use context::*;
mod harness;
pub use harness::*;
mod model;
pub mod path_wire;
pub use model::*;
mod project;
pub use project::*;
mod registry;
pub mod scene_wire;
pub use registry::*;
mod recovery;
pub use recovery::*;
mod session;
pub use session::*;
mod snapshot;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
pub use snapshot::*;
mod payload;
pub use payload::*;
pub mod remote_wire;
pub mod wire;
/// Native wire family; peers with a different family are rejected.
pub const PROTOCOL_VERSION: u32 = 8;
/// Maximum aggregate input, paste or clipboard-response payload accepted per request.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 query bytes; rejected rather than truncated.
pub const MAX_SEARCH_BYTES: usize = 4096;
/// Maximum decoded pixel bytes retained for one canonical image.
pub const IMAGE_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Maximum nesting depth accepted for a window's split tree.
pub const MAX_LAYOUT_DEPTH: usize = 16;
/// Attachment policy; input, clipboard access and sizing eligibility are independent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewSpec {
    /// Desired terminal geometry for this view.
    pub size: Size,
    /// Whether this attachment is eligible to own terminal sizing.
    pub interactive: bool,
    /// Whether the pane is currently displayed by this view.
    pub visible: bool,
    /// Whether this view may submit application input.
    pub input: bool,
    /// Whether this view may handle clipboard requests.
    pub clipboard: bool,
}
/// Reject zero or excessive grid and cell-pixel dimensions.
pub fn valid_size(size: Size) -> Result<()> {
    if size.cols == 0
        || size.rows == 0
        || size.cols > 500
        || size.rows > 300
        || size.cell_width == 0
        || size.cell_height == 0
        || size.cell_width > 256
        || size.cell_height > 256
    {
        bail!("invalid or excessive terminal dimensions");
    }
    Ok(())
}
/// Pane-scoped commands; view IDs refer to live backend attachments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PaneRequest {
    /// Read pane state without creating a view.
    Inspect,
    /// Accept view policy and schedule size reconciliation; does not wait for native resize.
    Attach {
        #[serde(flatten)]
        /// Initial view policy.
        spec: ViewSpec,
    },
    /// Accept policy changes independently of blocked native input or device callbacks.
    Update {
        /// Attachment whose policy is being replaced.
        view_id: String,
        #[serde(flatten)]
        /// Complete replacement policy.
        spec: ViewSpec,
    },
    /// Revoke the view and release admission immediately; never terminates the pane.
    Detach {
        /// Attachment to revoke.
        view_id: String,
    },
    /// Read changes after the supplied event, scene and snapshot revisions.
    Poll {
        /// Attachment receiving this update.
        view_id: String,
        #[serde(default)]
        /// Last consumed event sequence.
        after: u64,
        #[serde(default)]
        /// Last retained scene revision.
        frame_after: u64,
        #[serde(default)]
        /// Image generations already retained by the consumer.
        resources: Vec<ImageKey>,
        #[serde(default)]
        /// Last consumed pane-state revision.
        snapshot_after: u64,
        #[serde(default)]
        /// Maximum long-poll wait in milliseconds.
        wait_ms: u64,
        #[serde(default)]
        /// Whether complete changed-row deltas are acceptable.
        row_deltas: bool,
        #[serde(default)]
        /// Requested scene representation, validated by the backend.
        scene_encoding: Option<String>,
    },
    /// Accept the desired size lease; actual frame geometry converges asynchronously.
    Pin {
        /// Attachment requesting the lease change.
        view_id: String,
        /// Fixed geometry, or `None` to release this view's pin.
        size: Option<Size>,
    },
    /// Queue exact application input bytes without appending a newline.
    Input {
        /// Input-authorized attachment.
        view_id: String,
        /// Bytes to deliver in order.
        data: Vec<u8>,
    },
    /// Paste bytes using the terminal's native paste policy.
    Paste {
        /// Input-authorized attachment.
        view_id: String,
        /// Unframed paste contents.
        data: Vec<u8>,
    },
    /// Paste MIME contents through native clipboard/paste handling.
    PasteContents {
        /// Input-authorized attachment.
        view_id: String,
        /// Exact MIME bytes offered for paste.
        contents: Vec<ClipboardContent>,
        /// Clipboard selection associated with the contents.
        location: ClipboardLocation,
        /// Whether the caller explicitly permits unsafe paste content.
        #[serde(default)]
        allow_unsafe: bool,
    },
    /// Encode and queue a physical key event.
    Key {
        /// Input-authorized attachment.
        view_id: String,
        /// Physical key, modifiers and text.
        event: KeyEvent,
    },
    /// Encode a pointer event according to the application's mouse mode.
    Mouse {
        /// Attachment supplying the event.
        view_id: String,
        /// Pane-relative pointer event.
        event: MouseEvent,
    },
    /// Report application focus for one view.
    Focus {
        /// Attachment whose focus changed.
        view_id: String,
        /// Whether this view now has focus.
        focused: bool,
    },
    /// Move a view's native scrollback viewport.
    Scroll {
        /// Attachment whose viewport moves.
        view_id: String,
        /// Signed row movement; positive values move toward older history.
        delta: i64,
    },
    /// View-local inspection; never injects PTY input or acquires an input grant.
    Select {
        /// Attachment owning the selection.
        view_id: String,
        /// Selection gesture in pane-content coordinates.
        event: SelectionEvent,
    },
    /// Export a view's current selection without injecting keyboard input.
    CopySelection {
        /// Attachment owning the selection.
        view_id: String,
    },
    /// Native literal search, scoped to one visible inspection lifetime.
    Search {
        /// Visible attachment owning the search.
        view_id: String,
        /// Query or match-navigation action.
        action: SearchAction,
    },
    /// Complete a pending device request; never delivers keyboard input.
    Clipboard {
        /// Attachment that received the request.
        view_id: String,
        /// Exact pending request correlation.
        request_id: String,
        /// Consent decision and exact returned MIME bytes.
        response: ClipboardResponse,
    },
    /// Publish the attached terminal's observed colors.
    Colors {
        /// Attachment supplying the color observations.
        view_id: String,
        /// Default foreground RGB.
        foreground: [u8; 3],
        /// Default background RGB.
        background: [u8; 3],
        /// Observed cursor RGB, if available.
        cursor: Option<[u8; 3]>,
        /// Observed indexed palette, if available.
        palette: Option<Vec<[u8; 3]>>,
    },
    /// Terminate the pane's process through an authorized view.
    Terminate {
        /// Attachment requesting termination.
        view_id: String,
    },
    /// Shut down the pane owner independently of any attachment.
    Shutdown,
}

/// Literal-search query and navigation within a view's retained terminal history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchAction {
    /// Replace the literal query.
    Query {
        /// Exact UTF-8 search text, bounded by [`MAX_SEARCH_BYTES`].
        text: String,
    },
    /// Select the next match.
    Next,
    /// Select the preceding match.
    Previous,
    /// Remove the query and selected match.
    Clear,
}

/// Gesture phase for view-local text selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionAction {
    /// Begin selection or a repeated-click selection.
    Press,
    /// Extend selection to the current pointer position.
    Drag,
    /// Finish the drag.
    Release,
    /// Remove the selection.
    Clear,
    /// Advance edge autoscroll while the pointer remains outside the grid.
    Tick,
}

/// View-local selection gesture, separate from application mouse reporting.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SelectionEvent {
    /// Gesture phase.
    pub action: SelectionAction,
    /// Pane-content-relative cell coordinates, including fractional/outside positions.
    pub x: f32,
    /// Vertical pane-content coordinate in cells, including outside positions.
    pub y: f32,
    /// Shift=1, Ctrl=2, Alt=4 (rectangle), Super=8.
    pub modifiers: u8,
    /// Client monotonic elapsed milliseconds, used only for repeat-click semantics.
    pub time_ms: u64,
}
