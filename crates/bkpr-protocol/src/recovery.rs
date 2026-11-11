//! Restart-safe native conversation intent, never a live launch capability.
use crate::HarnessKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Saved native resume target, without credentials or proof of a live execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HarnessRecovery {
    /// Harness family whose current configuration will perform the resume.
    pub kind: HarnessKind,
    /// Exact harness-native conversation ID to resume.
    pub conversation_id: String,
    /// Saved resume working directory, preserving native path bytes.
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub cwd: PathBuf,
}

/// Current resume admission state; not a harness activity or receiver-ready status.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    /// This native conversation is bound to a launch or has a resume pending.
    Live,
    /// A resume attempt is currently permitted, without guaranteeing native success.
    Recoverable,
    /// Current execution, configuration or lifecycle state prevents a resume attempt.
    Unavailable,
}

/// Saved pane intent combined with its current resume admission state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryCandidate {
    /// Retained pane record to resume.
    pub pane_id: String,
    /// Owning workspace, absent for an unassigned pane.
    pub workspace_id: Option<String>,
    /// Owning window, absent for an unassigned pane.
    pub window_id: Option<String>,
    /// Current owning-window revision, absent when there is no owning window.
    pub window_revision: Option<u64>,
    /// Retained pane title, not a recovered live OSC title.
    pub title: String,
    /// Saved native conversation and launch directory.
    pub harness: HarnessRecovery,
    /// Current admission result, recomputed independently of saved intent.
    pub status: RecoveryStatus,
    /// Blocking reason or diagnostic from a previous failed automatic resume.
    pub reason: Option<String>,
}
