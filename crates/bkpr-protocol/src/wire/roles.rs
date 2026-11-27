use super::channels::{
    read_mention_binding, read_participant, write_mention_binding, write_participant,
};
use super::types::{read_text, write_text};
use super::{Budget, g};
use crate::channels::{ChannelId, ParticipantKind, valid_id, valid_name};
use crate::roles::*;
use anyhow::{Result, ensure};
use planus::{Builder, Offset, UnionOffset};

pub(super) fn write_request(
    b: &mut Builder,
    request: &RoleRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::RoleRequest>> {
    budget.node(depth)?;
    request.validate()?;
    Ok(match request {
        RoleRequest::Activity { eligible } => {
            let offset = g::RoleRequestActivity::create(b, *eligible);
            g::RoleRequest::create_role_request_activity(b, offset)
        }
        RoleRequest::Snapshot {} => {
            let offset = g::RoleRequestSnapshot::create(b);
            g::RoleRequest::create_role_request_snapshot(b, offset)
        }
        RoleRequest::Decide {
            request_id,
            requester,
            expected_holder,
            approve,
        } => {
            let id = write_text(b, &request_id.0, budget)?;
            let requester = write_mention_binding(b, requester, budget, depth + 1)?;
            let holder = expected_holder
                .as_ref()
                .map(|id| write_text(b, &id.0, budget))
                .transpose()?;
            let offset = g::RoleRequestDecide::create(b, id, requester, holder, *approve);
            g::RoleRequest::create_role_request_decide(b, offset)
        }
        RoleRequest::Set {
            workspace_id,
            role,
            target,
        } => {
            let workspace = write_text(b, workspace_id, budget)?;
            let target = target
                .as_ref()
                .map(|v| write_mention_binding(b, v, budget, depth + 1))
                .transpose()?;
            let offset = g::RoleRequestSet::create(b, workspace, write_role(*role), target);
            g::RoleRequest::create_role_request_set(b, offset)
        }
    })
}

pub(super) fn read_request(
    request: g::RoleRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<RoleRequest> {
    budget.node(depth)?;
    Ok(match request {
        g::RoleRequestRef::RoleRequestActivity(v) => RoleRequest::Activity {
            eligible: v.eligible()?,
        },
        g::RoleRequestRef::RoleRequestSnapshot(_) => RoleRequest::Snapshot {},
        g::RoleRequestRef::RoleRequestDecide(v) => RoleRequest::Decide {
            request_id: RoleRequestId(read_id(v.request_id()?, budget)?),
            requester: read_mention_binding(v.requester()?, budget, depth + 1)?,
            expected_holder: v
                .expected_holder()?
                .map(|id| read_id(id, budget).map(RoleGrantId))
                .transpose()?,
            approve: v.approve()?,
        },
        g::RoleRequestRef::RoleRequestSet(v) => RoleRequest::Set {
            workspace_id: read_id(v.workspace_id()?, budget)?,
            role: read_role(v.role()?),
            target: v
                .target()?
                .map(|v| read_mention_binding(v, budget, depth + 1))
                .transpose()?,
        },
    })
}

fn read_id(value: &str, budget: &mut Budget) -> Result<String> {
    valid_id(value)?;
    read_text(value, budget)
}

fn read_name(value: &str, budget: &mut Budget) -> Result<String> {
    valid_name(value)?;
    read_text(value, budget)
}

pub(super) fn write_role(role: WorkspaceRole) -> g::WorkspaceRole {
    match role {
        WorkspaceRole::Coordinator => g::WorkspaceRole::Coordinator,
    }
}

pub(super) fn read_role(role: g::WorkspaceRole) -> WorkspaceRole {
    match role {
        g::WorkspaceRole::Coordinator => WorkspaceRole::Coordinator,
    }
}

fn write_grant(
    b: &mut Builder,
    v: &RoleGrant,
    budget: &mut Budget,
) -> Result<Offset<g::RoleGrant>> {
    budget.node(1)?;
    valid_id(&v.id.0)?;
    valid_id(&v.workspace_id)?;
    valid_id(v.channel_id.as_ref())?;
    valid_name(&v.display_name)?;
    v.holder.validate()?;
    let id = write_text(b, &v.id.0, budget)?;
    let workspace = write_text(b, &v.workspace_id, budget)?;
    let holder = write_mention_binding(b, &v.holder, budget, 2)?;
    let channel = write_text(b, v.channel_id.as_ref(), budget)?;
    let display = write_text(b, &v.display_name, budget)?;
    let origin = match v.origin {
        RoleOrigin::Approval => g::RoleOrigin::Approval,
        RoleOrigin::Command => g::RoleOrigin::Command,
    };
    Ok(g::RoleGrant::create(
        b,
        id,
        workspace,
        write_role(v.role),
        holder,
        origin,
        v.granted_at_ms,
        channel,
        display,
    ))
}

fn read_grant(v: g::RoleGrantRef<'_>, budget: &mut Budget) -> Result<RoleGrant> {
    budget.node(1)?;
    Ok(RoleGrant {
        id: RoleGrantId(read_id(v.id()?, budget)?),
        workspace_id: read_id(v.workspace_id()?, budget)?,
        role: read_role(v.role()?),
        holder: read_mention_binding(v.holder()?, budget, 2)?,
        origin: match v.origin()? {
            g::RoleOrigin::Approval => RoleOrigin::Approval,
            g::RoleOrigin::Command => RoleOrigin::Command,
        },
        granted_at_ms: v.granted_at_ms()?,
        channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
        display_name: read_name(v.display_name()?, budget)?,
    })
}

fn write_approval(
    b: &mut Builder,
    v: &RoleApproval,
    budget: &mut Budget,
) -> Result<Offset<g::RoleApproval>> {
    budget.node(1)?;
    valid_id(&v.id.0)?;
    valid_id(&v.workspace_id)?;
    v.requester.validate()?;
    valid_approval(v)?;
    let id = write_text(b, &v.id.0, budget)?;
    let workspace = write_text(b, &v.workspace_id, budget)?;
    let requester = write_mention_binding(b, &v.requester, budget, 2)?;
    let participant = write_participant(b, &v.participant, budget, 2)?;
    let holder = v
        .prior_holder
        .as_ref()
        .map(|grant| write_grant(b, grant, budget))
        .transpose()?;
    let name = write_text(b, &v.workspace_name, budget)?;
    Ok(g::RoleApproval::create(
        b,
        id,
        workspace,
        write_role(v.role),
        requester,
        participant,
        holder,
        v.expires_at_ms,
        name,
    ))
}

fn read_approval(v: g::RoleApprovalRef<'_>, budget: &mut Budget) -> Result<RoleApproval> {
    budget.node(1)?;
    let approval = RoleApproval {
        id: RoleRequestId(read_id(v.id()?, budget)?),
        workspace_id: read_id(v.workspace_id()?, budget)?,
        workspace_name: read_text(v.workspace_name()?, budget)?,
        role: read_role(v.role()?),
        requester: read_mention_binding(v.requester()?, budget, 2)?,
        participant: read_participant(v.participant()?, budget, 2)?,
        prior_holder: v
            .prior_holder()?
            .map(|grant| read_grant(grant, budget))
            .transpose()?,
        expires_at_ms: v.expires_at_ms()?,
    };
    valid_approval(&approval)?;
    Ok(approval)
}

fn valid_approval(v: &RoleApproval) -> Result<()> {
    ensure!(
        v.participant.kind == ParticipantKind::Agent
            && v.participant.id == v.requester.participant_id,
        "role approval requester does not match its agent participant"
    );
    if let Some(holder) = &v.prior_holder {
        ensure!(
            holder.workspace_id == v.workspace_id && holder.role == v.role,
            "role approval prior tenure must match its workspace and role"
        );
    }
    Ok(())
}

pub(super) fn write_response(
    b: &mut Builder,
    response: &RoleResponse,
    budget: &mut Budget,
) -> Result<UnionOffset<g::RoleResponse>> {
    budget.node(0)?;
    Ok(match response {
        RoleResponse::Activity {} => {
            let offset = g::RoleResponseActivity::create(b);
            g::RoleResponse::create_role_response_activity(b, offset)
        }
        RoleResponse::Snapshot {
            revision,
            grants,
            approvals,
        } => {
            budget.vector(grants.len(), std::mem::size_of::<RoleGrant>())?;
            budget.vector(approvals.len(), std::mem::size_of::<RoleApproval>())?;
            let grants = grants
                .iter()
                .map(|grant| write_grant(b, grant, budget))
                .collect::<Result<Vec<_>>>()?;
            let approvals = approvals
                .iter()
                .map(|v| write_approval(b, v, budget))
                .collect::<Result<Vec<_>>>()?;
            let grants = b.create_vector(&grants);
            let approvals = b.create_vector(&approvals);
            let offset = g::RoleResponseSnapshot::create(b, *revision, grants, approvals);
            g::RoleResponse::create_role_response_snapshot(b, offset)
        }
        RoleResponse::Updated { grant } => {
            let grant = grant
                .as_ref()
                .map(|v| write_grant(b, v, budget))
                .transpose()?;
            let offset = g::RoleResponseUpdated::create(b, grant);
            g::RoleResponse::create_role_response_updated(b, offset)
        }
    })
}

pub(super) fn read_response(
    response: g::RoleResponseRef<'_>,
    budget: &mut Budget,
) -> Result<RoleResponse> {
    budget.node(0)?;
    Ok(match response {
        g::RoleResponseRef::RoleResponseActivity(_) => RoleResponse::Activity {},
        g::RoleResponseRef::RoleResponseSnapshot(v) => {
            let grants = v.grants()?;
            let approvals = v.approvals()?;
            budget.vector(grants.len(), std::mem::size_of::<RoleGrant>())?;
            budget.vector(approvals.len(), std::mem::size_of::<RoleApproval>())?;
            RoleResponse::Snapshot {
                revision: v.revision()?,
                grants: grants
                    .iter()
                    .map(|v| read_grant(v?, budget))
                    .collect::<Result<_>>()?,
                approvals: approvals
                    .iter()
                    .map(|v| read_approval(v?, budget))
                    .collect::<Result<_>>()?,
            }
        }
        g::RoleResponseRef::RoleResponseUpdated(v) => RoleResponse::Updated {
            grant: v.grant()?.map(|v| read_grant(v, budget)).transpose()?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{MAX_ID_BYTES, ParticipantId};
    use crate::{
        ClientAuthority, Operation, Request,
        wire::{Codec, decode_request},
    };

    #[test]
    fn registered_decision_preserves_every_fence_and_rejects_invalid_selectors() {
        let mut codec = Codec::new();
        let operation = |request| {
            Request::new(Operation::Client {
                authority: ClientAuthority {
                    client_id: "client".into(),
                    token: "private".into(),
                },
                request: Box::new(Operation::Roles { request }),
            })
        };
        let requester = crate::channels::ChannelMentionBinding {
            participant_id: ParticipantId("participant".into()),
            launch_id: "launch".into(),
            binding_generation: 8,
        };
        let decision = RoleRequest::Decide {
            request_id: RoleRequestId("request".into()),
            requester: requester.clone(),
            expected_holder: Some(RoleGrantId("tenure".into())),
            approve: false,
        };
        let frame = codec.encode_request(&operation(decision.clone())).unwrap();
        let framed = crate::wire::encode_frame(crate::wire::Kind::Request, frame).unwrap();
        let header = &framed[..crate::wire::HEADER_LEN];
        let capabilities = u64::from_le_bytes(header[16..24].try_into().unwrap());
        assert_eq!(
            capabilities,
            crate::wire::SUPPORTED_CAPABILITIES | crate::wire::CAP_WORKSPACE_ROLES
        );
        assert!(
            crate::wire::frame_payload_len(
                header,
                crate::wire::Kind::Request,
                crate::wire::MAX_REQUEST
            )
            .is_ok()
        );
        let Operation::Client { request, .. } = decode_request(frame).unwrap().operation else {
            panic!()
        };
        let Operation::Roles { request } = *request else {
            panic!()
        };
        assert_eq!(request, decision);
        for invalid in [
            RoleRequest::Set {
                workspace_id: "x".repeat(MAX_ID_BYTES + 1),
                role: WorkspaceRole::Coordinator,
                target: None,
            },
            RoleRequest::Set {
                workspace_id: "workspace".into(),
                role: WorkspaceRole::Coordinator,
                target: Some(crate::channels::ChannelMentionBinding {
                    binding_generation: 0,
                    ..requester
                }),
            },
        ] {
            assert!(codec.encode_request(&operation(invalid)).is_err());
        }
    }
}
