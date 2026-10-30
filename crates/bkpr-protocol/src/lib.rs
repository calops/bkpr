//! Canonical terminal scenes and versioned backend registry protocol.
mod model;
pub use model::*;
mod registry;
pub mod scene_wire;
pub use registry::*;
mod snapshot;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
pub use snapshot::*;
pub const PROTOCOL_VERSION: u32 = 2;
/// Maximum aggregate input, paste or clipboard-response payload accepted per request.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Maximum decoded pixel bytes retained for one canonical image.
pub const IMAGE_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Admission bounds shared by registry, view authority and transport.
pub const MAX_PANE_RECORDS: usize = 256;
pub const MAX_LIVE_PANES: usize = 64;
pub const MAX_VIEWS_PER_PANE: usize = 32;
pub const MAX_ADMITTED_VIEWS: usize = 128;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_LAYOUT_DEPTH: usize = 16;
pub const MAX_REGISTRY_WATCHERS: usize = 32;
// Every successful Attach owns one daemon-global view slot, including retired scenes.
pub const MAX_WAITING_PANE_POLLS: usize = MAX_ADMITTED_VIEWS;
pub const MAX_CONTROL_CONNECTIONS: usize = 32;
pub const MAX_CONNECTIONS: usize =
    MAX_WAITING_PANE_POLLS + MAX_REGISTRY_WATCHERS + MAX_CONTROL_CONNECTIONS;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewSpec {
    pub size: Size,
    pub interactive: bool,
    pub visible: bool,
    pub input: bool,
    pub clipboard: bool,
}
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PaneRequest {
    Inspect,
    /// Accept view policy and schedule size reconciliation; does not wait for native resize.
    Attach {
        #[serde(flatten)]
        spec: ViewSpec,
    },
    /// Accept policy changes independently of blocked native input or device callbacks.
    Update {
        view_id: String,
        #[serde(flatten)]
        spec: ViewSpec,
    },
    /// Revoke the view and release admission immediately; never terminates the pane.
    Detach {
        view_id: String,
    },
    Poll {
        view_id: String,
        #[serde(default)]
        after: u64,
        #[serde(default)]
        frame_after: u64,
        #[serde(default)]
        resources: Vec<ImageKey>,
        #[serde(default)]
        snapshot_after: u64,
        #[serde(default)]
        wait_ms: u64,
        #[serde(default)]
        row_deltas: bool,
        #[serde(default)]
        scene_encoding: Option<String>,
    },
    /// Accept the desired size lease; actual frame geometry converges asynchronously.
    Pin {
        view_id: String,
        size: Option<Size>,
    },
    Input {
        view_id: String,
        data: Vec<u8>,
    },
    Paste {
        view_id: String,
        data: Vec<u8>,
    },
    PasteContents {
        view_id: String,
        contents: Vec<ClipboardContent>,
        location: ClipboardLocation,
        #[serde(default)]
        allow_unsafe: bool,
    },
    Key {
        view_id: String,
        event: KeyEvent,
    },
    Mouse {
        view_id: String,
        event: MouseEvent,
    },
    Focus {
        view_id: String,
        focused: bool,
    },
    Scroll {
        view_id: String,
        delta: i64,
    },
    Clipboard {
        view_id: String,
        request_id: String,
        response: ClipboardResponse,
    },
    Colors {
        view_id: String,
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
        palette: Option<Vec<[u8; 3]>>,
    },
    Terminate {
        view_id: String,
    },
    Shutdown,
}
