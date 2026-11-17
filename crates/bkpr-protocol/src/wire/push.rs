use super::types::{read_text, write_text};
use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::{Result, ensure};
use planus::{Builder, Offset, UnionOffset};
pub(super) fn write_push(
    builder: &mut Builder,
    request: &PushRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::PushRequest>> {
    budget.node(depth)?;
    Ok(match request {
        PushRequest::PublicKey {} => {
            let v = g::PushRequestPublicKey::create(builder);
            g::PushRequest::create_push_request_public_key(builder, v)
        }
        PushRequest::List {} => {
            let v = g::PushRequestList::create(builder);
            g::PushRequest::create_push_request_list(builder, v)
        }
        PushRequest::Diagnostics {} => {
            let v = g::PushRequestDiagnostics::create(builder);
            g::PushRequest::create_push_request_diagnostics(builder, v)
        }
        PushRequest::Update {
            request_id,
            destination_id,
            generation,
            scope,
            kinds,
        } => {
            let request_id = write_text(builder, request_id, budget)?;
            let id = write_text(builder, destination_id, budget)?;
            let scope = super::notifications::write_scope(builder, scope, budget, depth + 1)?;
            let kinds = super::notifications::write_kinds(builder, kinds, budget)?;
            let v =
                g::PushRequestUpdate::create(builder, request_id, id, *generation, scope, kinds);
            g::PushRequest::create_push_request_update(builder, v)
        }
        PushRequest::Enroll {
            request_id,
            subscription,
            policy,
            scope,
            kinds,
            replaces,
            device_label,
        } => {
            let request_id = write_text(builder, request_id, budget)?;
            budget.node(depth + 1)?;
            let endpoint = write_text(builder, &subscription.endpoint, budget)?;
            let p256dh = write_text(builder, &subscription.p256dh, budget)?;
            let auth = write_text(builder, &subscription.auth, budget)?;
            let subscription = g::PushSubscription::create(builder, endpoint, p256dh, auth);
            ensure!(
                policy.allow_private_cidrs.len() <= 8,
                "push policy exceeds bound"
            );
            budget.vector(
                policy.allow_private_cidrs.len(),
                std::mem::size_of::<Offset<str>>(),
            )?;
            let mut cidrs = Vec::with_capacity(policy.allow_private_cidrs.len());
            for cidr in &policy.allow_private_cidrs {
                cidrs.push(write_text(builder, cidr, budget)?);
            }
            let cidrs = builder.create_vector(&cidrs);
            let policy = g::PushEndpointPolicy::create(builder, cidrs);
            let scope = super::notifications::write_scope(builder, scope, budget, depth + 1)?;
            let kinds = super::notifications::write_kinds(builder, kinds, budget)?;
            let replaces = if let Some(r) = replaces {
                let id = write_text(builder, &r.destination_id, budget)?;
                Some(g::PushRevision::create(builder, id, r.generation))
            } else {
                None
            };
            let device_label =
                super::types::write_optional_text(builder, device_label.as_deref(), budget)?;
            let v = g::PushRequestEnroll::create(
                builder,
                request_id,
                subscription,
                policy,
                scope,
                kinds,
                replaces,
                device_label,
            );
            g::PushRequest::create_push_request_enroll(builder, v)
        }
        PushRequest::Confirm {
            destination_id,
            generation,
            nonce,
        } => {
            let id = write_text(builder, destination_id, budget)?;
            let nonce = write_text(builder, nonce, budget)?;
            let v = g::PushRequestConfirm::create(builder, id, *generation, nonce);
            g::PushRequest::create_push_request_confirm(builder, v)
        }
        PushRequest::Revoke {
            destination_id,
            generation,
        } => {
            let id = write_text(builder, destination_id, budget)?;
            let v = g::PushRequestRevoke::create(builder, id, *generation);
            g::PushRequest::create_push_request_revoke(builder, v)
        }
        PushRequest::Test {
            request_id,
            destination_id,
            generation,
        } => {
            let rid = write_text(builder, request_id, budget)?;
            let id = write_text(builder, destination_id, budget)?;
            let v = g::PushRequestTest::create(builder, rid, id, *generation);
            g::PushRequest::create_push_request_test(builder, v)
        }
    })
}
pub(super) fn read_push(
    value: g::PushRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<PushRequest> {
    budget.node(depth)?;
    Ok(match value {
        g::PushRequestRef::PushRequestPublicKey(_) => PushRequest::PublicKey {},
        g::PushRequestRef::PushRequestList(_) => PushRequest::List {},
        g::PushRequestRef::PushRequestDiagnostics(_) => PushRequest::Diagnostics {},
        g::PushRequestRef::PushRequestUpdate(v) => PushRequest::Update {
            request_id: read_text(v.request_id()?, budget)?,
            destination_id: read_text(v.destination_id()?, budget)?,
            generation: v.generation()?,
            scope: super::notifications::read_scope(v.scope()?, budget, depth + 1)?,
            kinds: super::notifications::read_kinds(v.kinds()?, budget)?,
        },
        g::PushRequestRef::PushRequestEnroll(v) => {
            let subscription = v.subscription()?;
            let policy = v.policy()?;
            let cidrs = policy.allow_private_cidrs()?;
            ensure!(cidrs.len() <= 8, "push policy exceeds bound");
            budget.vector(cidrs.len(), std::mem::size_of::<String>())?;
            let mut allow_private_cidrs = Vec::with_capacity(cidrs.len());
            for cidr in cidrs.iter() {
                allow_private_cidrs.push(read_text(cidr?, budget)?);
            }
            PushRequest::Enroll {
                request_id: read_text(v.request_id()?, budget)?,
                subscription: PushSubscription {
                    endpoint: read_text(subscription.endpoint()?, budget)?,
                    p256dh: read_text(subscription.p256dh()?, budget)?,
                    auth: read_text(subscription.auth()?, budget)?,
                },
                policy: PushEndpointPolicy {
                    allow_private_cidrs,
                },
                scope: super::notifications::read_scope(v.scope()?, budget, depth + 1)?,
                kinds: super::notifications::read_kinds(v.kinds()?, budget)?,
                device_label: super::types::read_optional_text(v.device_label()?, budget)?,
                replaces: v
                    .replaces()?
                    .map(|r| {
                        Ok::<_, anyhow::Error>(PushRevision {
                            destination_id: read_text(r.destination_id()?, budget)?,
                            generation: r.generation()?,
                        })
                    })
                    .transpose()?,
            }
        }
        g::PushRequestRef::PushRequestConfirm(v) => PushRequest::Confirm {
            destination_id: read_text(v.destination_id()?, budget)?,
            generation: v.generation()?,
            nonce: read_text(v.nonce()?, budget)?,
        },
        g::PushRequestRef::PushRequestRevoke(v) => PushRequest::Revoke {
            destination_id: read_text(v.destination_id()?, budget)?,
            generation: v.generation()?,
        },
        g::PushRequestRef::PushRequestTest(v) => PushRequest::Test {
            request_id: read_text(v.request_id()?, budget)?,
            destination_id: read_text(v.destination_id()?, budget)?,
            generation: v.generation()?,
        },
    })
}
