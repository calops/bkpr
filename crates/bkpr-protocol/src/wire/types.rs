use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::Result;
use planus::{Builder, Offset};

use super::paths::{path_bytes, read_path};

pub(super) fn write_harness_kind(value: HarnessKind) -> g::HarnessKind {
    match value {
        HarnessKind::Claude => g::HarnessKind::Claude,
        HarnessKind::Codex => g::HarnessKind::Codex,
        HarnessKind::Omp => g::HarnessKind::Omp,
        HarnessKind::Agy => g::HarnessKind::Agy,
        HarnessKind::Opencode => g::HarnessKind::Opencode,
        HarnessKind::Cursor => g::HarnessKind::Cursor,
        HarnessKind::Copilot => g::HarnessKind::Copilot,
        HarnessKind::Aider => g::HarnessKind::Aider,
        HarnessKind::Cline => g::HarnessKind::Cline,
        HarnessKind::Goose => g::HarnessKind::Goose,
    }
}
pub(super) fn read_harness_kind(value: g::HarnessKind) -> HarnessKind {
    match value {
        g::HarnessKind::Claude => HarnessKind::Claude,
        g::HarnessKind::Codex => HarnessKind::Codex,
        g::HarnessKind::Omp => HarnessKind::Omp,
        g::HarnessKind::Agy => HarnessKind::Agy,
        g::HarnessKind::Opencode => HarnessKind::Opencode,
        g::HarnessKind::Cursor => HarnessKind::Cursor,
        g::HarnessKind::Copilot => HarnessKind::Copilot,
        g::HarnessKind::Aider => HarnessKind::Aider,
        g::HarnessKind::Cline => HarnessKind::Cline,
        g::HarnessKind::Goose => HarnessKind::Goose,
    }
}

pub(super) fn write_conversation_source(value: ConversationSource) -> g::ConversationSource {
    match value {
        ConversationSource::ClaudeHook => g::ConversationSource::ClaudeHook,
        ConversationSource::OmpExtension => g::ConversationSource::OmpExtension,
        ConversationSource::CodexStart => g::ConversationSource::CodexStart,
        ConversationSource::CodexResume => g::ConversationSource::CodexResume,
        ConversationSource::CodexFork => g::ConversationSource::CodexFork,
        ConversationSource::AgyMcp => g::ConversationSource::AgyMcp,
    }
}
pub(super) fn read_conversation_source(value: g::ConversationSource) -> ConversationSource {
    match value {
        g::ConversationSource::ClaudeHook => ConversationSource::ClaudeHook,
        g::ConversationSource::OmpExtension => ConversationSource::OmpExtension,
        g::ConversationSource::CodexStart => ConversationSource::CodexStart,
        g::ConversationSource::CodexResume => ConversationSource::CodexResume,
        g::ConversationSource::CodexFork => ConversationSource::CodexFork,
        g::ConversationSource::AgyMcp => ConversationSource::AgyMcp,
    }
}

pub(super) fn write_agent_scope(value: AgentScope) -> g::AgentScope {
    match value {
        AgentScope::Workspace => g::AgentScope::Workspace,
        AgentScope::Session => g::AgentScope::Session,
        AgentScope::Repository => g::AgentScope::Repository,
        AgentScope::All => g::AgentScope::All,
    }
}
pub(super) fn read_agent_scope(value: g::AgentScope) -> AgentScope {
    match value {
        g::AgentScope::Workspace => AgentScope::Workspace,
        g::AgentScope::Session => AgentScope::Session,
        g::AgentScope::Repository => AgentScope::Repository,
        g::AgentScope::All => AgentScope::All,
    }
}

pub(super) fn write_agent_receiver_mode(value: AgentReceiverMode) -> g::AgentReceiverMode {
    match value {
        AgentReceiverMode::ActiveTurn => g::AgentReceiverMode::ActiveTurn,
        AgentReceiverMode::FutureTurn => g::AgentReceiverMode::FutureTurn,
    }
}
pub(super) fn read_agent_receiver_mode(value: g::AgentReceiverMode) -> AgentReceiverMode {
    match value {
        g::AgentReceiverMode::ActiveTurn => AgentReceiverMode::ActiveTurn,
        g::AgentReceiverMode::FutureTurn => AgentReceiverMode::FutureTurn,
    }
}

pub(super) fn write_axis(value: Axis) -> g::Axis {
    match value {
        Axis::Horizontal => g::Axis::Horizontal,
        Axis::Vertical => g::Axis::Vertical,
    }
}
pub(super) fn read_axis(value: g::Axis) -> Axis {
    match value {
        g::Axis::Horizontal => Axis::Horizontal,
        g::Axis::Vertical => Axis::Vertical,
    }
}

pub(super) fn write_clipboard_location(value: ClipboardLocation) -> g::ClipboardLocation {
    match value {
        ClipboardLocation::Standard => g::ClipboardLocation::Standard,
        ClipboardLocation::Selection => g::ClipboardLocation::Selection,
        ClipboardLocation::Primary => g::ClipboardLocation::Primary,
    }
}
pub(super) fn read_clipboard_location(value: g::ClipboardLocation) -> ClipboardLocation {
    match value {
        g::ClipboardLocation::Standard => ClipboardLocation::Standard,
        g::ClipboardLocation::Selection => ClipboardLocation::Selection,
        g::ClipboardLocation::Primary => ClipboardLocation::Primary,
    }
}

pub(super) fn write_clipboard_result(value: ClipboardResult) -> g::ClipboardResult {
    match value {
        ClipboardResult::Success => g::ClipboardResult::Success,
        ClipboardResult::Denied => g::ClipboardResult::Denied,
        ClipboardResult::Unsupported => g::ClipboardResult::Unsupported,
        ClipboardResult::Busy => g::ClipboardResult::Busy,
        ClipboardResult::InvalidData => g::ClipboardResult::InvalidData,
        ClipboardResult::IoError => g::ClipboardResult::IoError,
    }
}
pub(super) fn read_clipboard_result(value: g::ClipboardResult) -> ClipboardResult {
    match value {
        g::ClipboardResult::Success => ClipboardResult::Success,
        g::ClipboardResult::Denied => ClipboardResult::Denied,
        g::ClipboardResult::Unsupported => ClipboardResult::Unsupported,
        g::ClipboardResult::Busy => ClipboardResult::Busy,
        g::ClipboardResult::InvalidData => ClipboardResult::InvalidData,
        g::ClipboardResult::IoError => ClipboardResult::IoError,
    }
}

pub(super) fn write_key_action(value: KeyAction) -> g::KeyAction {
    match value {
        KeyAction::Press => g::KeyAction::Press,
        KeyAction::Release => g::KeyAction::Release,
        KeyAction::Repeat => g::KeyAction::Repeat,
    }
}
pub(super) fn read_key_action(value: g::KeyAction) -> KeyAction {
    match value {
        g::KeyAction::Press => KeyAction::Press,
        g::KeyAction::Release => KeyAction::Release,
        g::KeyAction::Repeat => KeyAction::Repeat,
    }
}

pub(super) fn write_mouse_button(value: MouseButton) -> g::MouseButton {
    match value {
        MouseButton::None => g::MouseButton::None,
        MouseButton::Left => g::MouseButton::Left,
        MouseButton::Middle => g::MouseButton::Middle,
        MouseButton::Right => g::MouseButton::Right,
        MouseButton::WheelUp => g::MouseButton::WheelUp,
        MouseButton::WheelDown => g::MouseButton::WheelDown,
        MouseButton::WheelLeft => g::MouseButton::WheelLeft,
        MouseButton::WheelRight => g::MouseButton::WheelRight,
        MouseButton::Back => g::MouseButton::Back,
        MouseButton::Forward => g::MouseButton::Forward,
    }
}
pub(super) fn read_mouse_button(value: g::MouseButton) -> MouseButton {
    match value {
        g::MouseButton::None => MouseButton::None,
        g::MouseButton::Left => MouseButton::Left,
        g::MouseButton::Middle => MouseButton::Middle,
        g::MouseButton::Right => MouseButton::Right,
        g::MouseButton::WheelUp => MouseButton::WheelUp,
        g::MouseButton::WheelDown => MouseButton::WheelDown,
        g::MouseButton::WheelLeft => MouseButton::WheelLeft,
        g::MouseButton::WheelRight => MouseButton::WheelRight,
        g::MouseButton::Back => MouseButton::Back,
        g::MouseButton::Forward => MouseButton::Forward,
    }
}

pub(super) fn write_mouse_action(value: MouseAction) -> g::MouseAction {
    match value {
        MouseAction::Press => g::MouseAction::Press,
        MouseAction::Release => g::MouseAction::Release,
        MouseAction::Motion => g::MouseAction::Motion,
    }
}
pub(super) fn read_mouse_action(value: g::MouseAction) -> MouseAction {
    match value {
        g::MouseAction::Press => MouseAction::Press,
        g::MouseAction::Release => MouseAction::Release,
        g::MouseAction::Motion => MouseAction::Motion,
    }
}

pub(super) fn write_selection_action(value: SelectionAction) -> g::SelectionAction {
    match value {
        SelectionAction::Press => g::SelectionAction::Press,
        SelectionAction::Drag => g::SelectionAction::Drag,
        SelectionAction::Release => g::SelectionAction::Release,
        SelectionAction::Clear => g::SelectionAction::Clear,
        SelectionAction::Tick => g::SelectionAction::Tick,
    }
}
pub(super) fn read_selection_action(value: g::SelectionAction) -> SelectionAction {
    match value {
        g::SelectionAction::Press => SelectionAction::Press,
        g::SelectionAction::Drag => SelectionAction::Drag,
        g::SelectionAction::Release => SelectionAction::Release,
        g::SelectionAction::Clear => SelectionAction::Clear,
        g::SelectionAction::Tick => SelectionAction::Tick,
    }
}

pub(super) fn write_request_outcome(value: RequestOutcome) -> g::RequestOutcome {
    match value {
        RequestOutcome::NotDispatched => g::RequestOutcome::NotDispatched,
        RequestOutcome::Unknown => g::RequestOutcome::Unknown,
    }
}
pub(super) fn read_request_outcome(value: g::RequestOutcome) -> RequestOutcome {
    match value {
        g::RequestOutcome::NotDispatched => RequestOutcome::NotDispatched,
        g::RequestOutcome::Unknown => RequestOutcome::Unknown,
    }
}

pub(super) fn write_size(
    builder: &mut Builder,
    value: &Size,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::Size>> {
    budget.node(depth)?;
    let cols = value.cols;
    let rows = value.rows;
    let cell_width = value.cell_width;
    let cell_height = value.cell_height;
    Ok(g::Size::create(
        builder,
        cols,
        rows,
        cell_width,
        cell_height,
    ))
}

pub(super) fn read_size(value: g::SizeRef<'_>, budget: &mut Budget, depth: usize) -> Result<Size> {
    budget.node(depth)?;
    Ok(Size {
        cols: value.cols()?,
        rows: value.rows()?,
        cell_width: value.cell_width()?,
        cell_height: value.cell_height()?,
    })
}

pub(super) fn write_image_key(
    builder: &mut Builder,
    value: &ImageKey,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ImageKey>> {
    budget.node(depth)?;
    let id = value.id;
    let generation = value.generation;
    Ok(g::ImageKey::create(builder, id, generation))
}

pub(super) fn read_image_key(
    value: g::ImageKeyRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ImageKey> {
    budget.node(depth)?;
    Ok(ImageKey {
        id: value.id()?,
        generation: value.generation()?,
    })
}

pub(super) fn write_view_spec(
    builder: &mut Builder,
    value: &ViewSpec,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ViewSpec>> {
    budget.node(depth)?;
    let size = write_size(builder, &value.size, budget, depth + 1)?;
    let interactive = value.interactive;
    let visible = value.visible;
    let input = value.input;
    let clipboard = value.clipboard;
    Ok(g::ViewSpec::create(
        builder,
        size,
        interactive,
        visible,
        input,
        clipboard,
    ))
}

pub(super) fn read_view_spec(
    value: g::ViewSpecRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ViewSpec> {
    budget.node(depth)?;
    Ok(ViewSpec {
        size: read_size(value.size()?, budget, depth + 1)?,
        interactive: value.interactive()?,
        visible: value.visible()?,
        input: value.input()?,
        clipboard: value.clipboard()?,
    })
}

pub(super) fn write_client_authority(
    builder: &mut Builder,
    value: &ClientAuthority,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ClientAuthority>> {
    budget.node(depth)?;
    let client_id = {
        budget.bytes(value.client_id.len())?;
        builder.create_string(value.client_id.as_str())
    };
    let token = {
        budget.bytes(value.token.len())?;
        builder.create_string(value.token.as_str())
    };
    Ok(g::ClientAuthority::create(builder, client_id, token))
}

pub(super) fn read_client_authority(
    value: g::ClientAuthorityRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ClientAuthority> {
    budget.node(depth)?;
    Ok(ClientAuthority {
        client_id: {
            let text = value.client_id()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        token: {
            let text = value.token()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
    })
}

pub(super) fn write_pane_spawn_spec(
    builder: &mut Builder,
    value: &PaneSpawnSpec,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::PaneSpawnSpec>> {
    budget.node(depth)?;
    let argv = {
        budget.vector(value.argv.len(), 8)?;
        let mut values = Vec::with_capacity(value.argv.len());
        for value in value.argv.iter() {
            values.push({
                budget.bytes(value.len())?;
                builder.create_string(value.as_str())
            });
        }
        builder.create_vector(&values)
    };
    let cwd = {
        let bytes = path_bytes(&value.cwd)?;
        budget.bytes(bytes.len())?;
        builder.create_vector(bytes.as_ref())
    };
    let home = {
        let bytes = path_bytes(&value.home)?;
        budget.bytes(bytes.len())?;
        builder.create_vector(bytes.as_ref())
    };
    let size = write_size(builder, &value.size, budget, depth + 1)?;
    let title = value
        .title
        .as_ref()
        .map(|value| -> Result<_> {
            Ok({
                budget.bytes(value.len())?;
                builder.create_string(value.as_str())
            })
        })
        .transpose()?;
    let isolated_environment = value.isolated_environment;
    Ok(g::PaneSpawnSpec::create(
        builder,
        argv,
        cwd,
        home,
        size,
        title,
        isolated_environment,
    ))
}

pub(super) fn read_pane_spawn_spec(
    value: g::PaneSpawnSpecRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<PaneSpawnSpec> {
    budget.node(depth)?;
    Ok(PaneSpawnSpec {
        argv: {
            let vector = value.argv()?;
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
        cwd: {
            let bytes = value.cwd()?;
            budget.bytes(bytes.len())?;
            read_path(bytes)?
        },
        home: {
            let bytes = value.home()?;
            budget.bytes(bytes.len())?;
            read_path(bytes)?
        },
        size: read_size(value.size()?, budget, depth + 1)?,
        title: value
            .title()?
            .map(|value| -> Result<_> {
                Ok({
                    let text = value;
                    budget.bytes(text.len())?;
                    text.to_owned()
                })
            })
            .transpose()?,
        isolated_environment: value.isolated_environment()?,
    })
}

pub(super) fn write_harness_recovery(
    builder: &mut Builder,
    value: &HarnessRecovery,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::HarnessRecovery>> {
    budget.node(depth)?;
    let kind = write_harness_kind(value.kind);
    let conversation_id = {
        budget.bytes(value.conversation_id.len())?;
        builder.create_string(value.conversation_id.as_str())
    };
    let cwd = {
        let bytes = path_bytes(&value.cwd)?;
        budget.bytes(bytes.len())?;
        builder.create_vector(bytes.as_ref())
    };
    Ok(g::HarnessRecovery::create(
        builder,
        kind,
        conversation_id,
        cwd,
    ))
}

pub(super) fn read_harness_recovery(
    value: g::HarnessRecoveryRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<HarnessRecovery> {
    budget.node(depth)?;
    Ok(HarnessRecovery {
        kind: read_harness_kind(value.kind()?),
        conversation_id: {
            let text = value.conversation_id()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        cwd: {
            let bytes = value.cwd()?;
            budget.bytes(bytes.len())?;
            read_path(bytes)?
        },
    })
}

pub(super) fn write_clipboard_content(
    builder: &mut Builder,
    value: &ClipboardContent,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ClipboardContent>> {
    budget.node(depth)?;
    let mime = {
        budget.bytes(value.mime.len())?;
        builder.create_string(value.mime.as_str())
    };
    let data = {
        budget.bytes(value.data.len())?;
        builder.create_vector(value.data.as_slice())
    };
    Ok(g::ClipboardContent::create(builder, mime, data))
}

pub(super) fn read_clipboard_content(
    value: g::ClipboardContentRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ClipboardContent> {
    budget.node(depth)?;
    Ok(ClipboardContent {
        mime: {
            let text = value.mime()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        data: {
            let bytes = value.data()?;
            budget.bytes(bytes.len())?;
            bytes.to_vec()
        },
    })
}

pub(super) fn write_clipboard_response(
    builder: &mut Builder,
    value: &ClipboardResponse,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ClipboardResponse>> {
    budget.node(depth)?;
    let result = write_clipboard_result(value.result);
    let contents = {
        budget.vector(value.contents.len(), 8)?;
        let mut values = Vec::with_capacity(value.contents.len());
        for value in value.contents.iter() {
            values.push(write_clipboard_content(builder, value, budget, depth + 1)?);
        }
        builder.create_vector(&values)
    };
    let available = {
        budget.vector(value.available.len(), 8)?;
        let mut values = Vec::with_capacity(value.available.len());
        for value in value.available.iter() {
            values.push({
                budget.bytes(value.len())?;
                builder.create_string(value.as_str())
            });
        }
        builder.create_vector(&values)
    };
    let remember = value.remember;
    Ok(g::ClipboardResponse::create(
        builder, result, contents, available, remember,
    ))
}

pub(super) fn read_clipboard_response(
    value: g::ClipboardResponseRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ClipboardResponse> {
    budget.node(depth)?;
    Ok(ClipboardResponse {
        result: read_clipboard_result(value.result()?),
        contents: {
            let vector = value.contents()?;
            budget.vector(vector.len(), std::mem::size_of::<ClipboardContent>())?;
            let mut values = Vec::with_capacity(vector.len());
            for value in vector {
                let value = value?;
                values.push(read_clipboard_content(value, budget, depth + 1)?);
            }
            values
        },
        available: {
            let vector = value.available()?;
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
        remember: value.remember()?,
    })
}

pub(super) fn write_key_event(
    builder: &mut Builder,
    value: &KeyEvent,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::KeyEvent>> {
    budget.node(depth)?;
    let key = {
        budget.bytes(value.key.len())?;
        builder.create_string(value.key.as_str())
    };
    let modifiers = value.modifiers;
    let text = {
        budget.bytes(value.text.len())?;
        builder.create_string(value.text.as_str())
    };
    let action = write_key_action(value.action);
    let unshifted_codepoint = value.unshifted_codepoint;
    Ok(g::KeyEvent::create(
        builder,
        key,
        modifiers,
        text,
        action,
        unshifted_codepoint,
    ))
}

pub(super) fn read_key_event(
    value: g::KeyEventRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<KeyEvent> {
    budget.node(depth)?;
    Ok(KeyEvent {
        key: {
            let text = value.key()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        modifiers: value.modifiers()?,
        text: {
            let text = value.text()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        action: read_key_action(value.action()?),
        unshifted_codepoint: value.unshifted_codepoint()?,
    })
}

pub(super) fn write_mouse_event(
    builder: &mut Builder,
    value: &MouseEvent,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::MouseEvent>> {
    budget.node(depth)?;
    let x = value.x;
    let y = value.y;
    let modifiers = value.modifiers;
    let button = write_mouse_button(value.button);
    let action = write_mouse_action(value.action);
    Ok(g::MouseEvent::create(
        builder, x, y, modifiers, button, action,
    ))
}

pub(super) fn read_mouse_event(
    value: g::MouseEventRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<MouseEvent> {
    budget.node(depth)?;
    Ok(MouseEvent {
        x: value.x()?,
        y: value.y()?,
        modifiers: value.modifiers()?,
        button: read_mouse_button(value.button()?),
        action: read_mouse_action(value.action()?),
    })
}

pub(super) fn write_selection_event(
    builder: &mut Builder,
    value: &SelectionEvent,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::SelectionEvent>> {
    budget.node(depth)?;
    let action = write_selection_action(value.action);
    let x = value.x;
    let y = value.y;
    let modifiers = value.modifiers;
    let time_ms = value.time_ms;
    Ok(g::SelectionEvent::create(
        builder, action, x, y, modifiers, time_ms,
    ))
}

pub(super) fn read_selection_event(
    value: g::SelectionEventRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<SelectionEvent> {
    budget.node(depth)?;
    Ok(SelectionEvent {
        action: read_selection_action(value.action()?),
        x: value.x()?,
        y: value.y()?,
        modifiers: value.modifiers()?,
        time_ms: value.time_ms()?,
    })
}

pub(super) fn write_client_event(
    builder: &mut Builder,
    value: &ClientEvent,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ClientEvent>> {
    budget.node(depth)?;
    let event = {
        budget.bytes(value.event.len())?;
        builder.create_string(value.event.as_str())
    };
    let reason = {
        budget.bytes(value.reason.len())?;
        builder.create_string(value.reason.as_str())
    };
    Ok(g::ClientEvent::create(builder, event, reason))
}

pub(super) fn read_client_event(
    value: g::ClientEventRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ClientEvent> {
    budget.node(depth)?;
    Ok(ClientEvent {
        event: {
            let text = value.event()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
        reason: {
            let text = value.reason()?;
            budget.bytes(text.len())?;
            text.to_owned()
        },
    })
}
