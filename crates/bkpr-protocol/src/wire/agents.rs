use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::Result;
use planus::{Builder, UnionOffset};

use super::operations::{read_operation, write_operation};
use super::types::*;

pub(super) fn write_agent_request(
    builder: &mut Builder,
    value: &AgentRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::AgentRequest>> {
    budget.node(depth)?;
    Ok(match value {
        AgentRequest::Whoami => {
            let offset = g::AgentRequestWhoami::create(builder);
            g::AgentRequest::create_agent_request_whoami(builder, offset)
        }
        AgentRequest::Control { request, scope } => {
            let request = write_operation(builder, request, budget, depth + 1)?;
            let scope = write_agent_scope(*scope);
            let offset = g::AgentRequestControl::create(builder, request, scope);
            g::AgentRequest::create_agent_request_control(builder, offset)
        }
        AgentRequest::SetTitle { title } => {
            let title = {
                budget.bytes(title.len())?;
                builder.create_string(title.as_str())
            };
            let offset = g::AgentRequestSetTitle::create(builder, title);
            g::AgentRequest::create_agent_request_set_title(builder, offset)
        }
        AgentRequest::DiscoverAgents { scope, detail } => {
            let scope = write_agent_scope(*scope);
            let detail = *detail;
            let offset = g::AgentRequestDiscoverAgents::create(builder, scope, detail);
            g::AgentRequest::create_agent_request_discover_agents(builder, offset)
        }
        AgentRequest::SendMessage {
            to,
            text,
            idempotency_key,
            scope,
        } => {
            let to = {
                budget.bytes(to.len())?;
                builder.create_string(to.as_str())
            };
            let text = {
                budget.bytes(text.len())?;
                builder.create_string(text.as_str())
            };
            let idempotency_key = {
                budget.bytes(idempotency_key.len())?;
                builder.create_string(idempotency_key.as_str())
            };
            let scope = write_agent_scope(*scope);
            let offset =
                g::AgentRequestSendMessage::create(builder, to, text, idempotency_key, scope);
            g::AgentRequest::create_agent_request_send_message(builder, offset)
        }
        AgentRequest::ReadInbox { after, limit } => {
            let after = after
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let limit = limit
                .as_ref()
                .map(|value| -> Result<_> {
                    let value = u64::try_from(*value)?;
                    Ok(g::OptionalUsize::create(builder, value))
                })
                .transpose()?;
            let offset = g::AgentRequestReadInbox::create(builder, after, limit);
            g::AgentRequest::create_agent_request_read_inbox(builder, offset)
        }
        AgentRequest::AckMessage { ids } => {
            let ids = {
                budget.vector(ids.len(), 8)?;
                let mut values = Vec::with_capacity(ids.len());
                for value in ids.iter() {
                    values.push({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    });
                }
                builder.create_vector(&values)
            };
            let offset = g::AgentRequestAckMessage::create(builder, ids);
            g::AgentRequest::create_agent_request_ack_message(builder, offset)
        }
        AgentRequest::InspectOutbox {
            message_id,
            after,
            limit,
        } => {
            let message_id = message_id
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let after = after
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let limit = limit
                .as_ref()
                .map(|value| -> Result<_> {
                    let value = u64::try_from(*value)?;
                    Ok(g::OptionalUsize::create(builder, value))
                })
                .transpose()?;
            let offset = g::AgentRequestInspectOutbox::create(builder, message_id, after, limit);
            g::AgentRequest::create_agent_request_inspect_outbox(builder, offset)
        }
        AgentRequest::ReceiverRegister {
            session_id,
            mode,
            pid,
        } => {
            let session_id = {
                budget.bytes(session_id.len())?;
                builder.create_string(session_id.as_str())
            };
            let mode = write_agent_receiver_mode(*mode);
            let pid = *pid;
            let offset = g::AgentRequestReceiverRegister::create(builder, session_id, mode, pid);
            g::AgentRequest::create_agent_request_receiver_register(builder, offset)
        }
        AgentRequest::ReceiverSubscribe {
            receiver_id,
            wait_ms,
        } => {
            let receiver_id = {
                budget.bytes(receiver_id.len())?;
                builder.create_string(receiver_id.as_str())
            };
            let wait_ms = *wait_ms;
            let offset = g::AgentRequestReceiverSubscribe::create(builder, receiver_id, wait_ms);
            g::AgentRequest::create_agent_request_receiver_subscribe(builder, offset)
        }
        AgentRequest::WakeValidate {
            receiver_id,
            notification_id,
            generation,
        } => {
            let receiver_id = {
                budget.bytes(receiver_id.len())?;
                builder.create_string(receiver_id.as_str())
            };
            let notification_id = {
                budget.bytes(notification_id.len())?;
                builder.create_string(notification_id.as_str())
            };
            let generation = *generation;
            let offset = g::AgentRequestWakeValidate::create(
                builder,
                receiver_id,
                notification_id,
                generation,
            );
            g::AgentRequest::create_agent_request_wake_validate(builder, offset)
        }
        AgentRequest::WakeOutcome {
            receiver_id,
            notification_id,
            generation,
            outcome,
        } => {
            let receiver_id = {
                budget.bytes(receiver_id.len())?;
                builder.create_string(receiver_id.as_str())
            };
            let notification_id = {
                budget.bytes(notification_id.len())?;
                builder.create_string(notification_id.as_str())
            };
            let generation = *generation;
            let outcome = write_agent_wake_outcome(builder, outcome, budget, depth + 1)?;
            let offset = g::AgentRequestWakeOutcome::create(
                builder,
                receiver_id,
                notification_id,
                generation,
                outcome,
            );
            g::AgentRequest::create_agent_request_wake_outcome(builder, offset)
        }
    })
}

pub(super) fn read_agent_request(
    value: g::AgentRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<AgentRequest> {
    budget.node(depth)?;
    Ok(match value {
        g::AgentRequestRef::AgentRequestWhoami(_value) => AgentRequest::Whoami,
        g::AgentRequestRef::AgentRequestControl(value) => AgentRequest::Control {
            request: Box::new(read_operation(value.request()?, budget, depth + 1)?),
            scope: read_agent_scope(value.scope()?),
        },
        g::AgentRequestRef::AgentRequestSetTitle(value) => AgentRequest::SetTitle {
            title: {
                let text = value.title()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::AgentRequestRef::AgentRequestDiscoverAgents(value) => AgentRequest::DiscoverAgents {
            scope: read_agent_scope(value.scope()?),
            detail: value.detail()?,
        },
        g::AgentRequestRef::AgentRequestSendMessage(value) => AgentRequest::SendMessage {
            to: {
                let text = value.to()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            text: {
                let text = value.text()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            idempotency_key: {
                let text = value.idempotency_key()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            scope: read_agent_scope(value.scope()?),
        },
        g::AgentRequestRef::AgentRequestReadInbox(value) => AgentRequest::ReadInbox {
            after: value
                .after()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            limit: value
                .limit()?
                .map(|value| -> Result<_> { Ok(usize::try_from(value.value()?)?) })
                .transpose()?,
        },
        g::AgentRequestRef::AgentRequestAckMessage(value) => AgentRequest::AckMessage {
            ids: {
                let vector = value.ids()?;
                budget.vector(vector.len(), std::mem::size_of::<String>())?;
                let mut values = Vec::with_capacity(vector.len());
                for value in vector {
                    let value = value?;
                    values.push({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    });
                }
                values
            },
        },
        g::AgentRequestRef::AgentRequestInspectOutbox(value) => AgentRequest::InspectOutbox {
            message_id: value
                .message_id()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            after: value
                .after()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            limit: value
                .limit()?
                .map(|value| -> Result<_> { Ok(usize::try_from(value.value()?)?) })
                .transpose()?,
        },
        g::AgentRequestRef::AgentRequestReceiverRegister(value) => AgentRequest::ReceiverRegister {
            session_id: {
                let text = value.session_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            mode: read_agent_receiver_mode(value.mode()?),
            pid: value.pid()?,
        },
        g::AgentRequestRef::AgentRequestReceiverSubscribe(value) => {
            AgentRequest::ReceiverSubscribe {
                receiver_id: {
                    let text = value.receiver_id()?;
                    budget.bytes(text.len())?;
                    text.to_owned()
                },
                wait_ms: value.wait_ms()?,
            }
        }
        g::AgentRequestRef::AgentRequestWakeValidate(value) => AgentRequest::WakeValidate {
            receiver_id: {
                let text = value.receiver_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            notification_id: {
                let text = value.notification_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            generation: value.generation()?,
        },
        g::AgentRequestRef::AgentRequestWakeOutcome(value) => AgentRequest::WakeOutcome {
            receiver_id: {
                let text = value.receiver_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            notification_id: {
                let text = value.notification_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            generation: value.generation()?,
            outcome: read_agent_wake_outcome(value.outcome()?, budget, depth + 1)?,
        },
    })
}

pub(super) fn write_agent_wake_outcome(
    builder: &mut Builder,
    value: &AgentWakeOutcome,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::AgentWakeOutcome>> {
    budget.node(depth)?;
    Ok(match value {
        AgentWakeOutcome::Accepted => {
            let offset = g::AgentWakeOutcomeAccepted::create(builder);
            g::AgentWakeOutcome::create_agent_wake_outcome_accepted(builder, offset)
        }
        AgentWakeOutcome::Failed { reason } => {
            let reason = {
                budget.bytes(reason.len())?;
                builder.create_string(reason.as_str())
            };
            let offset = g::AgentWakeOutcomeFailed::create(builder, reason);
            g::AgentWakeOutcome::create_agent_wake_outcome_failed(builder, offset)
        }
        AgentWakeOutcome::Unknown { reason } => {
            let reason = {
                budget.bytes(reason.len())?;
                builder.create_string(reason.as_str())
            };
            let offset = g::AgentWakeOutcomeUnknown::create(builder, reason);
            g::AgentWakeOutcome::create_agent_wake_outcome_unknown(builder, offset)
        }
    })
}

pub(super) fn read_agent_wake_outcome(
    value: g::AgentWakeOutcomeRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<AgentWakeOutcome> {
    budget.node(depth)?;
    Ok(match value {
        g::AgentWakeOutcomeRef::AgentWakeOutcomeAccepted(_value) => AgentWakeOutcome::Accepted,
        g::AgentWakeOutcomeRef::AgentWakeOutcomeFailed(value) => AgentWakeOutcome::Failed {
            reason: {
                let text = value.reason()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::AgentWakeOutcomeRef::AgentWakeOutcomeUnknown(value) => AgentWakeOutcome::Unknown {
            reason: {
                let text = value.reason()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
    })
}
