//! Restart-safe native conversation intent, never a live launch capability.
use crate::HarnessKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HarnessRecovery {
    pub kind: HarnessKind,
    pub conversation_id: String,
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Live,
    Recoverable,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryCandidate {
    pub pane_id: String,
    pub workspace_id: Option<String>,
    pub window_id: Option<String>,
    pub window_revision: Option<u64>,
    pub title: String,
    pub harness: HarnessRecovery,
    pub status: RecoveryStatus,
    pub reason: Option<String>,
}
