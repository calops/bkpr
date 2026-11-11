//! Backend-observed local project facts; never recovered as live metadata.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Current local Git observation and independently cached provider metadata for a pane.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSnapshot {
    /// Foreground project directory used for this observation, preserving path bytes.
    #[serde(with = "crate::path_wire")]
    pub cwd: PathBuf,
    /// Availability of the local Git observation, not the CI fetch.
    pub state: ProjectState,
    /// Live session ordinal for the canonical common Git repository, not an RGB
    /// colour or persisted identity. Retained while an existing owner's identity
    /// is temporarily unknown; absent for unrecognized/nonrepository projects.
    pub color_slot: Option<u32>,
    /// Local worktree facts, absent when no repository was identified.
    pub repository: Option<RepositoryInfo>,
    /// Cached provider facts, fetched independently of local Git observation.
    pub ci: Option<crate::CiSnapshot>,
    /// Local observation diagnostic, independent of provider errors in `ci`.
    pub error: Option<String>,
}

/// Availability of backend-observed local repository facts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    /// Local Git observation is pending.
    Loading,
    /// Local repository facts are available.
    Ready,
    /// The observed directory is not in a recognized Git repository.
    NotRepository,
    /// Local Git observation failed; inspect the snapshot diagnostic.
    Error,
}

/// Worktree-local HEAD and changes, qualified by a shared common Git identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryInfo {
    /// Display name derived from the common repository's directory when recognized.
    pub name: String,
    /// Canonical worktree root; distinct from the shared repository identity.
    #[serde(with = "crate::path_wire")]
    pub root: PathBuf,
    /// Canonical common Git directory shared by linked worktrees.
    #[serde(with = "crate::path_wire")]
    pub common_dir: PathBuf,
    /// Canonical administrative Git directory specific to this worktree.
    #[serde(with = "crate::path_wire")]
    pub git_dir: PathBuf,
    /// Local branch name, absent for detached HEAD.
    pub branch: Option<String>,
    /// Local HEAD commit ID, absent before the first commit.
    pub head_sha: Option<String>,
    /// Index changes relative to HEAD.
    pub staged: ChangeSummary,
    /// Tracked working-tree changes relative to the index.
    pub unstaged: ChangeSummary,
    /// Untracked, nonignored file count.
    pub untracked_files: u64,
    /// Selected local comparison target, absent when no base can be resolved.
    pub base_ref: Option<String>,
    /// Merge base of the selected local target and HEAD, not the target tip.
    pub base_sha: Option<String>,
    /// Whole-tree HEAD versus merge-base diff; None means the base is unknown.
    pub committed: Option<ChangeSummary>,
}

/// Whole-tree Git diff totals; binary files contribute no line counts.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    /// Changed file entries, including binary changes.
    pub files: u64,
    /// Added text lines.
    pub insertions: u64,
    /// Removed text lines.
    pub deletions: u64,
    /// Changed entries reported by Git as binary.
    pub binary_files: u64,
}
