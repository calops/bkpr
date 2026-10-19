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

/// Issue trackers are independent of the forge hosting a change request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueTracker {
    Linear,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LinkedIssue {
    pub provider: IssueTracker,
    pub identifier: String,
    pub title: Option<String>,
    pub url: String,
}

/// The provider's authoritative target, independent of any local branch name or remote.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeRequestBase {
    pub branch: String,
    pub commit: String,
    pub repository: ForgeRepository,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: ChangeRequestState,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub issues: Vec<LinkedIssue>,
    #[serde(default)]
    pub base: Option<ChangeRequestBase>,
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
    /// The commit whose checks were returned by the provider.
    pub head_sha: String,
    pub branch: Option<String>,
    /// Local worktree identity used to validate cached provider metadata.
    #[serde(default)]
    pub local_head_sha: String,
    /// Remote tracking label for the branch whose tip was checked.
    #[serde(default)]
    pub checked_ref: Option<String>,
    /// The bounded provider connection had additional results; never claim full success.
    pub truncated: bool,
    /// The requested remote branch or detached commit is not available on the provider.
    #[serde(default)]
    pub commit_missing: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prior_change_requests_deserialize_without_description_or_issues() {
        let request: ChangeRequest = serde_json::from_value(serde_json::json!({
            "number": 42,
            "title": "Existing cached request",
            "url": "https://github.com/owner/repo/pull/42",
            "state": "open"
        }))
        .unwrap();
        assert!(request.description.is_empty());
        assert!(request.issues.is_empty());
        assert!(request.base.is_none());
        let mut updated = request;
        updated.description = "Full request body".into();
        updated.issues.push(LinkedIssue {
            provider: IssueTracker::Linear,
            identifier: "ENG-42".into(),
            title: Some("Keep linked metadata".into()),
            url: "https://linear.app/team/issue/ENG-42/keep-metadata".into(),
        });
        updated.base = Some(ChangeRequestBase {
            branch: "stack/parent".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            repository: ForgeRepository {
                provider: ForgeProvider::Github,
                host: "github.com".into(),
                path: "owner/repo".into(),
                web_url: "https://github.com/owner/repo".into(),
            },
        });
        let restored: ChangeRequest =
            serde_json::from_value(serde_json::to_value(&updated).unwrap()).unwrap();
        assert_eq!(restored, updated);
    }

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
