//! Authoritative context for a currently verified pane execution.
use crate::SessionInfo;
use serde::{Deserialize, Serialize};

/// Current execution membership and labels, not inherited environment or view focus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneContext {
    /// Logical session and current daemon incarnation owning the execution.
    pub session: SessionInfo,
    /// Exact execution's pane ID.
    pub pane_id: String,
    /// Current effective pane title, including applicable live title projection.
    pub pane_title: String,
    /// Current owning workspace ID, absent for an unassigned pane.
    pub workspace_id: Option<String>,
    /// Current workspace label, absent when no workspace owns the pane.
    pub workspace_name: Option<String>,
    /// Current owning window ID, absent for an unassigned pane.
    pub window_id: Option<String>,
    /// Current window label, absent when no window owns the pane.
    pub window_name: Option<String>,
    /// Registry revision from which membership and labels were read.
    pub revision: u64,
}
