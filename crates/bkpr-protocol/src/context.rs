use crate::SessionInfo;
use serde::{Deserialize, Serialize};

/// Current execution membership and labels, not inherited environment or view focus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneContext {
    pub session: SessionInfo,
    pub pane_id: String,
    pub pane_title: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    pub window_id: Option<String>,
    pub window_name: Option<String>,
    pub revision: u64,
}
