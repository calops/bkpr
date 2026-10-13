//! Provider-independent CI metadata. Agent semantic status is deliberately separate.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ForgeProvider {
    Github,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForgeRepository {
    pub provider: ForgeProvider,
    pub host: String,
    pub path: String,
    pub web_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CiRefreshTarget {
    pub repository: ForgeRepository,
    pub head_sha: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: ChangeRequestState,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiFetchState {
    Loading,
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    NoChecks,
    Pending,
    Running,
    Passed,
    Failed,
    Cancelled,
    Skipped,
    Unknown,
}

impl CiStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoChecks => "no checks",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }

    pub fn aggregate(statuses: impl IntoIterator<Item = Self>) -> Self {
        statuses
            .into_iter()
            .max_by_key(|status| match status {
                Self::NoChecks => 0,
                Self::Skipped => 1,
                Self::Passed => 2,
                Self::Cancelled => 3,
                Self::Unknown => 4,
                Self::Pending => 5,
                Self::Running => 6,
                Self::Failed => 7,
            })
            .unwrap_or(Self::NoChecks)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CiCheck {
    pub name: String,
    pub status: CiStatus,
    pub url: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CiSnapshot {
    pub status: CiStatus,
    pub checks: Vec<CiCheck>,
    pub change_request: Option<ChangeRequest>,
    pub fetch_state: CiFetchState,
    pub updated_at: u64,
    pub stale: bool,
    pub error: Option<String>,
    pub repository: ForgeRepository,
    pub head_sha: String,
    pub branch: Option<String>,
    /// The bounded provider connection had additional results; never claim full success.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_and_active_checks_take_precedence_over_success() {
        assert_eq!(
            CiStatus::aggregate([CiStatus::Passed, CiStatus::Pending]),
            CiStatus::Pending
        );
        assert_eq!(
            CiStatus::aggregate([CiStatus::Running, CiStatus::Failed]),
            CiStatus::Failed
        );
        assert_eq!(
            CiStatus::aggregate([CiStatus::Passed, CiStatus::Unknown]),
            CiStatus::Unknown
        );
        assert_eq!(CiStatus::aggregate([]), CiStatus::NoChecks);
    }
}
