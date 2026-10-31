//! Backend-observed local project facts; never recovered as live metadata.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSnapshot {
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    pub state: ProjectState,
    pub repository: Option<RepositoryInfo>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    Loading,
    Ready,
    NotRepository,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryInfo {
    pub name: String,
    /// Canonical worktree root; distinct from the shared repository identity.
    #[serde(with = "crate::path_wire")]
    pub root: PathBuf,
    #[serde(with = "crate::path_wire")]
    pub common_dir: PathBuf,
    #[serde(with = "crate::path_wire")]
    pub git_dir: PathBuf,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub staged: ChangeSummary,
    pub unstaged: ChangeSummary,
    pub untracked_files: u64,
    pub base_ref: Option<String>,
    /// Merge base of the selected local target and HEAD, not the target tip.
    pub base_sha: Option<String>,
    /// Whole-tree HEAD versus merge-base diff; None means the base is unknown.
    pub committed: Option<ChangeSummary>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
    pub binary_files: u64,
}
