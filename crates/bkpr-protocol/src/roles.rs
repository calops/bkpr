//! Session-local workspace role identity, user decisions and live projections.

use crate::channels::{ChannelId, ChannelMentionBinding, Participant, valid_id};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Maximum lifetime of an undecided request, measured by the backend's monotonic clock.
pub const APPROVAL_TIMEOUT_MS: u64 = 120_000;

/// A role the session user can grant to one live launch in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRole {
    /// May relay the user's intent for work in this workspace, not act as the user.
    Coordinator,
}

/// Exact identity of one tenure, never reused or restored.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleGrantId(
    /// Opaque backend-assigned identity, not a credential.
    pub String,
);

/// Exact identity of one pending request, never reused.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleRequestId(
    /// Opaque backend-assigned identity, not a credential.
    pub String,
);

/// How the user authorized a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleOrigin {
    /// The user approved a pending request.
    Approval,
    /// The user explicitly set the role with a command.
    Command,
}

/// Historical authorship when stored in a post, not evidence of current delegation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAttribution {
    /// Server-observed role.
    pub role: WorkspaceRole,
    /// Exact tenure at the time of observation.
    pub grant_id: RoleGrantId,
}

/// Current role tenure in an owning workspace. No field grants access to another source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleGrant {
    /// Exact tenure.
    pub id: RoleGrantId,
    /// Owning workspace.
    pub workspace_id: String,
    /// Granted role.
    pub role: WorkspaceRole,
    /// Exact holder observed at grant time. Authority belongs to its launch, not its label.
    pub holder: ChannelMentionBinding,
    /// Holder display label as observed when the grant was issued.
    pub display_name: String,
    /// User authorization path.
    pub origin: RoleOrigin,
    /// Grant time in Unix milliseconds.
    pub granted_at_ms: u64,
    /// Private thread for this tenure.
    pub channel_id: ChannelId,
}

/// User-visible metadata for a managed private thread; archived threads are user-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleThread {
    /// Role associated with this tenure.
    pub role: WorkspaceRole,
    /// No live grant owns this thread; the user can read but not post or manage it.
    pub archived: bool,
}

/// A prompt visible only to the eligible registered attachment selected by the backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleApproval {
    /// Exact pending request identity.
    pub id: RoleRequestId,
    /// Owning workspace.
    pub workspace_id: String,
    /// Requested role.
    pub role: WorkspaceRole,
    /// Exact requester selected for the decision fence.
    pub requester: ChannelMentionBinding,
    /// Display label for the requester; never an authorization selector.
    pub participant: Participant,
    /// Prior holder shown to the user, if any.
    pub prior_holder: Option<RoleGrant>,
    /// Owning workspace display name as observed when the prompt was issued.
    pub workspace_name: String,
    /// Display deadline in Unix milliseconds; only the backend decides expiry.
    pub expires_at_ms: u64,
}

/// Registered-session user operations. Agent tools cannot dispatch these operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoleRequest {
    /// Opt into role events and report local input eligibility. A true report records activity.
    Activity {
        /// False for a read-only or locally ineligible attachment.
        eligible: bool,
    },
    /// Current grants and prompts assigned to this exact attachment.
    Snapshot {},
    /// Decide the exact prompt shown to the user. A stale fence never grants or denies.
    Decide {
        /// Prompt being answered.
        request_id: RoleRequestId,
        /// Exact requester shown in the prompt.
        requester: ChannelMentionBinding,
        /// Prior tenure shown in the prompt, or absent if it was vacant.
        expected_holder: Option<RoleGrantId>,
        /// True approves; false permanently denies this request.
        approve: bool,
    },
    /// Direct user command; does not request consent and never uses a display name as identity.
    Set {
        /// Owning workspace, from the selected workspace view.
        workspace_id: String,
        /// Role to set.
        role: WorkspaceRole,
        /// Exact selected launcher and binding; absent explicitly revokes.
        target: Option<ChannelMentionBinding>,
    },
}

impl RoleRequest {
    /// Check bounded structural selectors. Does not authorize or resolve them.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Activity { .. } | Self::Snapshot {} => Ok(()),
            Self::Decide {
                request_id,
                requester,
                expected_holder,
                ..
            } => {
                valid_id(&request_id.0)?;
                requester.validate()?;
                if let Some(id) = expected_holder {
                    valid_id(&id.0)?;
                }
                Ok(())
            }
            Self::Set {
                workspace_id,
                target,
                ..
            } => {
                valid_id(workspace_id)?;
                if let Some(target) = target {
                    target.validate()?;
                }
                Ok(())
            }
        }
    }
}

/// Typed outcome of a registered user's role operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoleResponse {
    /// Role-event support is active for this attachment.
    Activity {},
    /// Replace the current session role projection and this attachment's prompt list.
    Snapshot {
        /// Latest session role-event revision represented by the snapshot.
        revision: u64,
        /// Current committed grants. Does not include historical tenures.
        grants: Vec<RoleGrant>,
        /// Only prompts currently assigned to the caller.
        approvals: Vec<RoleApproval>,
    },
    /// The command or decision was accepted, independently of native wake delivery.
    Updated {
        /// Resulting tenure, if a grant is current.
        grant: Option<RoleGrant>,
    },
}

/// Invalidate the live role/roster projection and this attachment's assigned prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleEvent {
    /// Session-local revision. Refresh the bounded role snapshot, not a stored prompt.
    pub revision: u64,
}
