use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::Result;
use planus::{Builder, UnionOffset};

use super::agents::{read_agent_request, write_agent_request};
use super::pane::{read_pane_request, write_pane_request};
use super::paths::{path_bytes, read_path};
use super::types::*;

pub(super) fn write_operation(
    builder: &mut Builder,
    value: &Operation,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::Operation>> {
    budget.node(depth)?;
    Ok(match value {
        Operation::SessionInfo => {
            let offset = g::OperationSessionInfo::create(builder);
            g::Operation::create_operation_session_info(builder, offset)
        }
        Operation::ClientRegister {
            session_id,
            daemon_id,
        } => {
            let session_id = {
                budget.bytes(session_id.len())?;
                builder.create_string(session_id.as_str())
            };
            let daemon_id = {
                budget.bytes(daemon_id.len())?;
                builder.create_string(daemon_id.as_str())
            };
            let offset = g::OperationClientRegister::create(builder, session_id, daemon_id);
            g::Operation::create_operation_client_register(builder, offset)
        }
        Operation::Client { authority, request } => {
            let authority = write_client_authority(builder, authority, budget, depth + 1)?;
            let request = write_operation(builder, request, budget, depth + 1)?;
            let offset = g::OperationClient::create(builder, authority, request);
            g::Operation::create_operation_client(builder, offset)
        }
        Operation::ClientActivate {
            detach_others,
            agent_socket,
        } => {
            let detach_others = *detach_others;
            let agent_socket = agent_socket
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        let bytes = path_bytes(value)?;
                        budget.bytes(bytes.len())?;
                        builder.create_vector(bytes.as_ref())
                    })
                })
                .transpose()?;
            let offset = g::OperationClientActivate::create(builder, detach_others, agent_socket);
            g::Operation::create_operation_client_activate(builder, offset)
        }
        Operation::ClientDetach => {
            let offset = g::OperationClientDetach::create(builder);
            g::Operation::create_operation_client_detach(builder, offset)
        }
        Operation::Inspect => {
            let offset = g::OperationInspect::create(builder);
            g::Operation::create_operation_inspect(builder, offset)
        }
        Operation::Context { pane_id } => {
            let pane_id = pane_id
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let offset = g::OperationContext::create(builder, pane_id);
            g::Operation::create_operation_context(builder, offset)
        }
        Operation::RecoveryList => {
            let offset = g::OperationRecoveryList::create(builder);
            g::Operation::create_operation_recovery_list(builder, offset)
        }
        Operation::PaneResume { pane_id, expected } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let expected = write_harness_recovery(builder, expected, budget, depth + 1)?;
            let offset = g::OperationPaneResume::create(builder, pane_id, expected);
            g::Operation::create_operation_pane_resume(builder, offset)
        }
        Operation::HarnessBegin { kind } => {
            let kind = write_harness_kind(*kind);
            let offset = g::OperationHarnessBegin::create(builder, kind);
            g::Operation::create_operation_harness_begin(builder, offset)
        }
        Operation::HarnessEvent {
            launch_id,
            token,
            event,
        } => {
            let launch_id = {
                budget.bytes(launch_id.len())?;
                builder.create_string(launch_id.as_str())
            };
            let token = {
                budget.bytes(token.len())?;
                builder.create_string(token.as_str())
            };
            let event = write_capture_event(builder, event, budget, depth + 1)?;
            let offset = g::OperationHarnessEvent::create(builder, launch_id, token, event);
            g::Operation::create_operation_harness_event(builder, offset)
        }
        Operation::HarnessContext { launch_id, token } => {
            let launch_id = {
                budget.bytes(launch_id.len())?;
                builder.create_string(launch_id.as_str())
            };
            let token = {
                budget.bytes(token.len())?;
                builder.create_string(token.as_str())
            };
            let offset = g::OperationHarnessContext::create(builder, launch_id, token);
            g::Operation::create_operation_harness_context(builder, offset)
        }
        Operation::Agent {
            launch_id,
            token,
            request,
        } => {
            let launch_id = {
                budget.bytes(launch_id.len())?;
                builder.create_string(launch_id.as_str())
            };
            let token = {
                budget.bytes(token.len())?;
                builder.create_string(token.as_str())
            };
            let request = write_agent_request(builder, request, budget, depth + 1)?;
            let offset = g::OperationAgent::create(builder, launch_id, token, request);
            g::Operation::create_operation_agent(builder, offset)
        }
        Operation::WorkspaceCreate { name, initial } => {
            let name = name
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let initial = initial
                .as_ref()
                .map(|value| write_pane_spawn_spec(builder, value, budget, depth + 1))
                .transpose()?;
            let offset = g::OperationWorkspaceCreate::create(builder, name, initial);
            g::Operation::create_operation_workspace_create(builder, offset)
        }
        Operation::WorkspaceRename {
            workspace_id,
            expected_revision,
            name,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let expected_revision = *expected_revision;
            let name = {
                budget.bytes(name.len())?;
                builder.create_string(name.as_str())
            };
            let offset =
                g::OperationWorkspaceRename::create(builder, workspace_id, expected_revision, name);
            g::Operation::create_operation_workspace_rename(builder, offset)
        }
        Operation::WorkspaceClose {
            workspace_id,
            expected_revision,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let expected_revision = *expected_revision;
            let offset =
                g::OperationWorkspaceClose::create(builder, workspace_id, expected_revision);
            g::Operation::create_operation_workspace_close(builder, offset)
        }
        Operation::WindowCreate {
            workspace_id,
            expected_revision,
            name,
            spec,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let expected_revision = *expected_revision;
            let name = name
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let spec = write_pane_spawn_spec(builder, spec, budget, depth + 1)?;
            let offset = g::OperationWindowCreate::create(
                builder,
                workspace_id,
                expected_revision,
                name,
                spec,
            );
            g::Operation::create_operation_window_create(builder, offset)
        }
        Operation::WindowRename {
            workspace_id,
            window_id,
            expected_revision,
            name,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let window_id = {
                budget.bytes(window_id.len())?;
                builder.create_string(window_id.as_str())
            };
            let expected_revision = *expected_revision;
            let name = {
                budget.bytes(name.len())?;
                builder.create_string(name.as_str())
            };
            let offset = g::OperationWindowRename::create(
                builder,
                workspace_id,
                window_id,
                expected_revision,
                name,
            );
            g::Operation::create_operation_window_rename(builder, offset)
        }
        Operation::WindowClose {
            workspace_id,
            window_id,
            expected_revision,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let window_id = {
                budget.bytes(window_id.len())?;
                builder.create_string(window_id.as_str())
            };
            let expected_revision = *expected_revision;
            let offset = g::OperationWindowClose::create(
                builder,
                workspace_id,
                window_id,
                expected_revision,
            );
            g::Operation::create_operation_window_close(builder, offset)
        }
        Operation::PaneSpawn {
            workspace_id,
            window_id,
            expected_revision,
            spec,
        } => {
            let workspace_id = workspace_id
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let window_id = window_id
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let expected_revision = expected_revision
                .as_ref()
                .map(|value| -> Result<_> {
                    let value = *value;
                    Ok(g::OptionalU64::create(builder, value))
                })
                .transpose()?;
            let spec = write_pane_spawn_spec(builder, spec, budget, depth + 1)?;
            let offset = g::OperationPaneSpawn::create(
                builder,
                workspace_id,
                window_id,
                expected_revision,
                spec,
            );
            g::Operation::create_operation_pane_spawn(builder, offset)
        }
        Operation::PaneReopen {
            workspace_id,
            window_id,
            expected_revision,
            pane_id,
            spec,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let window_id = {
                budget.bytes(window_id.len())?;
                builder.create_string(window_id.as_str())
            };
            let expected_revision = *expected_revision;
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let spec = write_pane_spawn_spec(builder, spec, budget, depth + 1)?;
            let offset = g::OperationPaneReopen::create(
                builder,
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
                spec,
            );
            g::Operation::create_operation_pane_reopen(builder, offset)
        }
        Operation::PaneTerminate { pane_id } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let offset = g::OperationPaneTerminate::create(builder, pane_id);
            g::Operation::create_operation_pane_terminate(builder, offset)
        }
        Operation::PaneForget { pane_id } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let offset = g::OperationPaneForget::create(builder, pane_id);
            g::Operation::create_operation_pane_forget(builder, offset)
        }
        Operation::PaneRename { pane_id, title } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let title = {
                budget.bytes(title.len())?;
                builder.create_string(title.as_str())
            };
            let offset = g::OperationPaneRename::create(builder, pane_id, title);
            g::Operation::create_operation_pane_rename(builder, offset)
        }
        Operation::LayoutUpdate {
            workspace_id,
            window_id,
            expected_revision,
            layout,
        } => {
            let workspace_id = {
                budget.bytes(workspace_id.len())?;
                builder.create_string(workspace_id.as_str())
            };
            let window_id = {
                budget.bytes(window_id.len())?;
                builder.create_string(window_id.as_str())
            };
            let expected_revision = *expected_revision;
            let layout = layout
                .as_ref()
                .map(|value| -> Result<_> {
                    budget.node(depth + 1)?;
                    let value = write_layout_node(builder, value, budget, depth + 2)?;
                    Ok(g::OptionalLayout::create(builder, value))
                })
                .transpose()?;
            let offset = g::OperationLayoutUpdate::create(
                builder,
                workspace_id,
                window_id,
                expected_revision,
                layout,
            );
            g::Operation::create_operation_layout_update(builder, offset)
        }
        Operation::Pane { pane_id, request } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let request = write_pane_request(builder, request, budget, depth + 1)?;
            let offset = g::OperationPane::create(builder, pane_id, request);
            g::Operation::create_operation_pane(builder, offset)
        }
        Operation::Wait { after, wait_ms } => {
            let after = *after;
            let wait_ms = *wait_ms;
            let offset = g::OperationWait::create(builder, after, wait_ms);
            g::Operation::create_operation_wait(builder, offset)
        }
        Operation::Shutdown => {
            let offset = g::OperationShutdown::create(builder);
            g::Operation::create_operation_shutdown(builder, offset)
        }
    })
}

pub(super) fn read_operation(
    value: g::OperationRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Operation> {
    budget.node(depth)?;
    Ok(match value {
        g::OperationRef::OperationSessionInfo(_value) => Operation::SessionInfo,
        g::OperationRef::OperationClientRegister(value) => Operation::ClientRegister {
            session_id: {
                let text = value.session_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            daemon_id: {
                let text = value.daemon_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationClient(value) => Operation::Client {
            authority: read_client_authority(value.authority()?, budget, depth + 1)?,
            request: Box::new(read_operation(value.request()?, budget, depth + 1)?),
        },
        g::OperationRef::OperationClientActivate(value) => Operation::ClientActivate {
            detach_others: value.detach_others()?,
            agent_socket: value
                .agent_socket()?
                .map(|value| -> Result<_> {
                    Ok({
                        let bytes = value;
                        budget.bytes(bytes.len())?;
                        read_path(bytes)?
                    })
                })
                .transpose()?,
        },
        g::OperationRef::OperationClientDetach(_value) => Operation::ClientDetach,
        g::OperationRef::OperationInspect(_value) => Operation::Inspect,
        g::OperationRef::OperationContext(value) => Operation::Context {
            pane_id: value
                .pane_id()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
        },
        g::OperationRef::OperationRecoveryList(_value) => Operation::RecoveryList,
        g::OperationRef::OperationPaneResume(value) => Operation::PaneResume {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected: read_harness_recovery(value.expected()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationHarnessBegin(value) => Operation::HarnessBegin {
            kind: read_harness_kind(value.kind()?),
        },
        g::OperationRef::OperationHarnessEvent(value) => Operation::HarnessEvent {
            launch_id: {
                let text = value.launch_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            token: {
                let text = value.token()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            event: read_capture_event(value.event()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationHarnessContext(value) => Operation::HarnessContext {
            launch_id: {
                let text = value.launch_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            token: {
                let text = value.token()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationAgent(value) => Operation::Agent {
            launch_id: {
                let text = value.launch_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            token: {
                let text = value.token()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            request: read_agent_request(value.request()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationWorkspaceCreate(value) => Operation::WorkspaceCreate {
            name: value
                .name()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            initial: value
                .initial()?
                .map(|value| read_pane_spawn_spec(value, budget, depth + 1))
                .transpose()?,
        },
        g::OperationRef::OperationWorkspaceRename(value) => Operation::WorkspaceRename {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
            name: {
                let text = value.name()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationWorkspaceClose(value) => Operation::WorkspaceClose {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
        },
        g::OperationRef::OperationWindowCreate(value) => Operation::WindowCreate {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
            name: value
                .name()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            spec: read_pane_spawn_spec(value.spec()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationWindowRename(value) => Operation::WindowRename {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            window_id: {
                let text = value.window_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
            name: {
                let text = value.name()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationWindowClose(value) => Operation::WindowClose {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            window_id: {
                let text = value.window_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
        },
        g::OperationRef::OperationPaneSpawn(value) => Operation::PaneSpawn {
            workspace_id: value
                .workspace_id()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            window_id: value
                .window_id()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
            expected_revision: value
                .expected_revision()?
                .map(|value| -> Result<_> { Ok(value.value()?) })
                .transpose()?,
            spec: read_pane_spawn_spec(value.spec()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationPaneReopen(value) => Operation::PaneReopen {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            window_id: {
                let text = value.window_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            spec: read_pane_spawn_spec(value.spec()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationPaneTerminate(value) => Operation::PaneTerminate {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationPaneForget(value) => Operation::PaneForget {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationPaneRename(value) => Operation::PaneRename {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            title: {
                let text = value.title()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::OperationRef::OperationLayoutUpdate(value) => Operation::LayoutUpdate {
            workspace_id: {
                let text = value.workspace_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            window_id: {
                let text = value.window_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            expected_revision: value.expected_revision()?,
            layout: value
                .layout()?
                .map(|value| {
                    budget.node(depth + 1)?;
                    read_layout_node(value.value()?, budget, depth + 2)
                })
                .transpose()?,
        },
        g::OperationRef::OperationPane(value) => Operation::Pane {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            request: read_pane_request(value.request()?, budget, depth + 1)?,
        },
        g::OperationRef::OperationWait(value) => Operation::Wait {
            after: value.after()?,
            wait_ms: value.wait_ms()?,
        },
        g::OperationRef::OperationShutdown(_value) => Operation::Shutdown,
    })
}

pub(super) fn write_layout_node(
    builder: &mut Builder,
    value: &LayoutNode,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::LayoutNode>> {
    budget.node(depth)?;
    Ok(match value {
        LayoutNode::Leaf { pane_id } => {
            let pane_id = {
                budget.bytes(pane_id.len())?;
                builder.create_string(pane_id.as_str())
            };
            let offset = g::LayoutNodeLeaf::create(builder, pane_id);
            g::LayoutNode::create_layout_node_leaf(builder, offset)
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let axis = write_axis(*axis);
            let ratio = *ratio;
            let first = write_layout_node(builder, first, budget, depth + 1)?;
            let second = write_layout_node(builder, second, budget, depth + 1)?;
            let offset = g::LayoutNodeSplit::create(builder, axis, ratio, first, second);
            g::LayoutNode::create_layout_node_split(builder, offset)
        }
    })
}

pub(super) fn read_layout_node(
    value: g::LayoutNodeRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<LayoutNode> {
    budget.node(depth)?;
    Ok(match value {
        g::LayoutNodeRef::LayoutNodeLeaf(value) => LayoutNode::Leaf {
            pane_id: {
                let text = value.pane_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::LayoutNodeRef::LayoutNodeSplit(value) => LayoutNode::Split {
            axis: read_axis(value.axis()?),
            ratio: value.ratio()?,
            first: Box::new(read_layout_node(value.first()?, budget, depth + 1)?),
            second: Box::new(read_layout_node(value.second()?, budget, depth + 1)?),
        },
    })
}

pub(super) fn write_capture_event(
    builder: &mut Builder,
    value: &CaptureEvent,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::CaptureEvent>> {
    budget.node(depth)?;
    Ok(match value {
        CaptureEvent::Bind {
            conversation_id,
            source,
        } => {
            let conversation_id = {
                budget.bytes(conversation_id.len())?;
                builder.create_string(conversation_id.as_str())
            };
            let source = write_conversation_source(*source);
            let offset = g::CaptureEventBind::create(builder, conversation_id, source);
            g::CaptureEvent::create_capture_event_bind(builder, offset)
        }
        CaptureEvent::Error { message } => {
            let message = {
                budget.bytes(message.len())?;
                builder.create_string(message.as_str())
            };
            let offset = g::CaptureEventError::create(builder, message);
            g::CaptureEvent::create_capture_event_error(builder, offset)
        }
        CaptureEvent::End => {
            let offset = g::CaptureEventEnd::create(builder);
            g::CaptureEvent::create_capture_event_end(builder, offset)
        }
    })
}

pub(super) fn read_capture_event(
    value: g::CaptureEventRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<CaptureEvent> {
    budget.node(depth)?;
    Ok(match value {
        g::CaptureEventRef::CaptureEventBind(value) => CaptureEvent::Bind {
            conversation_id: {
                let text = value.conversation_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            source: read_conversation_source(value.source()?),
        },
        g::CaptureEventRef::CaptureEventError(value) => CaptureEvent::Error {
            message: {
                let text = value.message()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::CaptureEventRef::CaptureEventEnd(_value) => CaptureEvent::End,
    })
}

pub(super) fn write_request_failure(
    builder: &mut Builder,
    value: &RequestFailure,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::RequestFailure>> {
    budget.node(depth)?;
    Ok(match value {
        RequestFailure::SessionClosing { outcome } => {
            let outcome = write_request_outcome(*outcome);
            let offset = g::RequestFailureSessionClosing::create(builder, outcome);
            g::RequestFailure::create_request_failure_session_closing(builder, offset)
        }
    })
}

pub(super) fn read_request_failure(
    value: g::RequestFailureRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<RequestFailure> {
    budget.node(depth)?;
    Ok(match value {
        g::RequestFailureRef::RequestFailureSessionClosing(value) => {
            RequestFailure::SessionClosing {
                outcome: read_request_outcome(value.outcome()?),
            }
        }
    })
}
