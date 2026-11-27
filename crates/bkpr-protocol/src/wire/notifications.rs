use super::push::{read_push, write_push};
use super::types::{read_optional_text, read_text, write_optional_text, write_text};
use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::{Result, ensure};
use planus::{Builder, Offset, UnionOffset};

pub(super) fn write_request(
    builder: &mut Builder,
    request: &NotificationRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::NotificationRequest>> {
    request.validate()?;
    budget.node(depth)?;
    Ok(match request {
        NotificationRequest::Inspect { query, after } => {
            let query = write_query(builder, query, budget, depth + 1)?;
            let after = write_cursor(builder, after.as_ref(), budget, depth + 1)?;
            let offset = g::NotificationRequestInspect::create(builder, query, after);
            g::NotificationRequest::create_notification_request_inspect(builder, offset)
        }
        NotificationRequest::Subscribe {
            query,
            after,
            wait_ms,
        } => {
            let query = write_query(builder, query, budget, depth + 1)?;
            let after = write_cursor(builder, after.as_ref(), budget, depth + 1)?;
            let offset = g::NotificationRequestSubscribe::create(builder, query, after, *wait_ms);
            g::NotificationRequest::create_notification_request_subscribe(builder, offset)
        }
        NotificationRequest::PolicyGet {} => {
            let offset = g::NotificationRequestPolicyGet::create(builder);
            g::NotificationRequest::create_notification_request_policy_get(builder, offset)
        }
        NotificationRequest::Health {} => {
            let offset = g::NotificationRequestHealth::create(builder);
            g::NotificationRequest::create_notification_request_health(builder, offset)
        }
        NotificationRequest::PolicySet { policy } => {
            budget.node(depth + 1)?;
            let scope = write_scope(builder, &policy.scope, budget, depth + 2)?;
            let kinds = write_kinds(builder, &policy.kinds, budget)?;
            let policy = g::NotificationPolicy::create(builder, scope, kinds);
            let offset = g::NotificationRequestPolicySet::create(builder, policy);
            g::NotificationRequest::create_notification_request_policy_set(builder, offset)
        }
        NotificationRequest::Push { request } => {
            let request = write_push(builder, request, budget, depth + 1)?;
            let offset = g::NotificationRequestPush::create(builder, request);
            g::NotificationRequest::create_notification_request_push(builder, offset)
        }
    })
}

pub(super) fn read_request(
    value: g::NotificationRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<NotificationRequest> {
    budget.node(depth)?;
    let request = match value {
        g::NotificationRequestRef::NotificationRequestInspect(value) => {
            NotificationRequest::Inspect {
                query: read_query(value.query()?, budget, depth + 1)?,
                after: read_cursor(value.after()?, budget, depth + 1)?,
            }
        }
        g::NotificationRequestRef::NotificationRequestSubscribe(value) => {
            NotificationRequest::Subscribe {
                query: read_query(value.query()?, budget, depth + 1)?,
                after: read_cursor(value.after()?, budget, depth + 1)?,
                wait_ms: value.wait_ms()?,
            }
        }
        g::NotificationRequestRef::NotificationRequestPolicyGet(_) => {
            NotificationRequest::PolicyGet {}
        }
        g::NotificationRequestRef::NotificationRequestHealth(_) => NotificationRequest::Health {},
        g::NotificationRequestRef::NotificationRequestPolicySet(value) => {
            budget.node(depth + 1)?;
            let policy = value.policy()?;
            NotificationRequest::PolicySet {
                policy: NotificationPolicy {
                    scope: read_scope(policy.scope()?, budget, depth + 2)?,
                    kinds: read_kinds(policy.kinds()?, budget)?,
                },
            }
        }
        g::NotificationRequestRef::NotificationRequestPush(value) => NotificationRequest::Push {
            request: Box::new(read_push(value.request()?, budget, depth + 1)?),
        },
    };
    request.validate()?;
    Ok(request)
}

pub(super) fn write_scope(
    builder: &mut Builder,
    scope: &NotificationScope,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::NotificationScope>> {
    budget.node(depth)?;
    let session = write_optional_text(builder, scope.session_id.as_deref(), budget)?;
    let workspace = write_optional_text(builder, scope.workspace_id.as_deref(), budget)?;
    let pane = write_optional_text(builder, scope.pane_id.as_deref(), budget)?;
    Ok(g::NotificationScope::create(
        builder, session, workspace, pane,
    ))
}

pub(super) fn read_scope(
    value: g::NotificationScopeRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<NotificationScope> {
    budget.node(depth)?;
    for text in [value.session_id()?, value.workspace_id()?, value.pane_id()?]
        .into_iter()
        .flatten()
    {
        ensure!(
            text.len() <= MAX_NOTIFICATION_ID_BYTES,
            "notification selector exceeds limit"
        );
    }
    Ok(NotificationScope {
        session_id: read_optional_text(value.session_id()?, budget)?,
        workspace_id: read_optional_text(value.workspace_id()?, budget)?,
        pane_id: read_optional_text(value.pane_id()?, budget)?,
    })
}

fn write_cursor(
    builder: &mut Builder,
    value: Option<&NotificationCursor>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<Offset<g::NotificationCursor>>> {
    value
        .map(|value| {
            budget.node(depth)?;
            let store_id = write_text(builder, &value.store_id, budget)?;
            Ok(g::NotificationCursor::create(
                builder,
                store_id,
                value.sequence,
                value.loss_generation,
            ))
        })
        .transpose()
}

fn read_cursor(
    value: Option<g::NotificationCursorRef<'_>>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<NotificationCursor>> {
    value
        .map(|value| {
            budget.node(depth)?;
            let store_id = value.store_id()?;
            ensure!(
                store_id.len() <= MAX_NOTIFICATION_ID_BYTES,
                "notification store identity exceeds limit"
            );
            Ok(NotificationCursor {
                store_id: read_text(store_id, budget)?,
                sequence: value.sequence()?,
                loss_generation: value.loss_generation()?,
            })
        })
        .transpose()
}

fn write_query(
    builder: &mut Builder,
    value: &NotificationQuery,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::NotificationQuery>> {
    budget.node(depth)?;
    let scope = write_scope(builder, &value.scope, budget, depth + 1)?;
    let kinds = write_kinds(builder, &value.kinds, budget)?;
    Ok(g::NotificationQuery::create(
        builder,
        scope,
        kinds,
        value.limit,
        value.include_channel_attention,
    ))
}

fn read_query(
    value: g::NotificationQueryRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<NotificationQuery> {
    budget.node(depth)?;
    Ok(NotificationQuery {
        scope: read_scope(value.scope()?, budget, depth + 1)?,
        kinds: read_kinds(value.kinds()?, budget)?,
        limit: value.limit()?,
        include_channel_attention: value.include_channel_attention()?,
    })
}

pub(super) fn write_kinds(
    builder: &mut Builder,
    values: &[NotificationKind],
    budget: &mut Budget,
) -> Result<Offset<[g::NotificationKind]>> {
    budget.vector(values.len(), 1)?;
    let mut kinds = [g::NotificationKind::Attention; 4];
    for (target, value) in kinds.iter_mut().zip(values) {
        *target = match value {
            NotificationKind::Attention => g::NotificationKind::Attention,
            NotificationKind::Error => g::NotificationKind::Error,
            NotificationKind::Idle => g::NotificationKind::Idle,
            NotificationKind::Done => g::NotificationKind::Done,
        };
    }
    Ok(builder.create_vector(&kinds[..values.len()]))
}

pub(super) fn read_kinds(
    values: planus::Vector<
        '_,
        core::result::Result<g::NotificationKind, planus::errors::UnknownEnumTag>,
    >,
    budget: &mut Budget,
) -> Result<Vec<NotificationKind>> {
    ensure!(values.len() <= 4, "too many notification kinds");
    budget.vector(values.len(), std::mem::size_of::<NotificationKind>())?;
    values
        .iter()
        .map(|value| {
            Ok(match value? {
                g::NotificationKind::Attention => NotificationKind::Attention,
                g::NotificationKind::Error => NotificationKind::Error,
                g::NotificationKind::Idle => NotificationKind::Idle,
                g::NotificationKind::Done => NotificationKind::Done,
            })
        })
        .collect()
}
