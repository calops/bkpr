//! Provider-independent CI metadata. Agent semantic status is deliberately separate.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Forge API family used to obtain repository and check metadata.
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ForgeProvider {
    /// GitHub checks and pull requests.
    Github,
}

/// Provider-qualified remote repository identity and browser destination.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, JsonSchema)]
pub struct ForgeRepository {
    /// API family serving this repository.
    pub provider: ForgeProvider,
    /// Forge hostname, currently `github.com`.
    pub host: String,
    /// Provider repository path, such as `owner/repository`.
    pub path: String,
    /// Browser URL for the repository.
    pub web_url: String,
}

/// Exact retained CI source selected for an asynchronous refresh.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiRefreshTarget {
    /// Running pane whose current project must still match this target.
    pub pane_id: String,
    /// Observed project directory, preserving native path bytes.
    #[serde(with = "crate::path_wire")]
    #[schemars(with = "crate::path_wire::PathSchema")]
    pub cwd: std::path::PathBuf,
    /// Provider repository whose checks were displayed.
    pub repository: ForgeRepository,
    /// Previously checked provider commit, which may differ from local HEAD.
    pub head_sha: String,
    /// Local worktree HEAD used to reject a stale project selection.
    pub local_head_sha: String,
    /// Configured remote/branch label, absent for a detached commit.
    pub checked_ref: Option<String>,
}

/// Provider-reported lifecycle of a pull request or equivalent change request.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeRequestState {
    /// Open but not yet marked ready for review.
    Draft,
    /// Open and ready for review.
    Open,
    /// Integrated into its target.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Issue trackers are independent of the forge hosting a change request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueTracker {
    /// Linear issue tracking.
    Linear,
}

/// Issue referenced by a change request's metadata.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LinkedIssue {
    /// Tracker that owns the issue.
    pub provider: IssueTracker,
    /// Human-facing tracker key, such as `ENG-42`.
    pub identifier: String,
    /// Issue title when available from provider metadata.
    pub title: Option<String>,
    /// Browser URL for the issue.
    pub url: String,
}

/// The provider's authoritative target, independent of any local branch name or remote.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeRequestBase {
    /// Provider target branch name.
    pub branch: String,
    /// Provider target commit ID, not the local merge base.
    pub commit: String,
    /// Repository receiving the proposed change.
    pub repository: ForgeRepository,
}

/// Provider change request associated with the checked source.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeRequest {
    /// Provider-assigned number within the repository.
    pub number: u64,
    /// Human-facing request title.
    pub title: String,
    /// Browser URL for the request.
    pub url: String,
    /// Current provider lifecycle state.
    pub state: ChangeRequestState,
    /// Provider request body; empty when absent from older cached metadata.
    #[serde(default)]
    pub description: String,
    /// Linked issue metadata, independent of the request's forge.
    #[serde(default)]
    pub issues: Vec<LinkedIssue>,
    /// Authoritative target, absent when unavailable from the provider.
    #[serde(default)]
    pub base: Option<ChangeRequestBase>,
}

/// Availability of the provider fetch, independent of aggregated check status.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiFetchState {
    /// A provider query is pending; retained facts may still be present.
    Loading,
    /// A provider query returned metadata.
    Ready,
    /// Provider metadata could not be obtained.
    Unavailable,
}

/// Provider-independent check result used for individual and aggregate status.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// No checks were returned.
    NoChecks,
    /// Checks are queued or awaiting execution.
    Pending,
    /// At least one check is executing.
    Running,
    /// Checks completed successfully.
    Passed,
    /// At least one check failed.
    Failed,
    /// Execution was cancelled.
    Cancelled,
    /// Execution was skipped or reported neutral.
    Skipped,
    /// The provider result cannot be classified.
    Unknown,
}

impl CiStatus {
    /// Lowercase human-facing status label.
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

    /// Combine checks with precedence: failed, running, pending, unknown,
    /// cancelled, passed, skipped, no checks. An empty input yields no checks.
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

/// One provider check or workflow result.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CiCheck {
    /// Provider's human-facing check name.
    pub name: String,
    /// Normalized check execution state.
    pub status: CiStatus,
    /// Browser URL for check details, when available.
    pub url: Option<String>,
    /// Observed execution duration in seconds, when timestamps are available.
    #[serde(default)]
    pub duration_seconds: Option<u64>,
}

/// Cached provider facts qualified by remote source and local worktree identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CiSnapshot {
    /// Aggregate of the returned checks; inspect truncation and fetch state as well.
    pub status: CiStatus,
    /// Individual checks returned within the provider result bound.
    pub checks: Vec<CiCheck>,
    /// Associated pull request or equivalent, when available.
    pub change_request: Option<ChangeRequest>,
    /// State of the provider query, separate from check success.
    pub fetch_state: CiFetchState,
    /// Unix seconds of the retained fetch result; zero means no completed fetch.
    pub updated_at: u64,
    /// Retained provider facts need refreshing.
    pub stale: bool,
    /// Latest provider diagnostic, possibly retained while a retry is loading.
    pub error: Option<String>,
    /// Provider repository to which these checks belong.
    pub repository: ForgeRepository,
    /// The commit whose checks were returned by the provider.
    pub head_sha: String,
    /// Local branch name, absent for detached HEAD.
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
