use super::{Budget, generated::native_wire as g};
use crate::*;
use anyhow::Result;
use planus::{Builder, UnionOffset};

use super::types::*;

pub(super) fn write_pane_request(
    builder: &mut Builder,
    value: &PaneRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::PaneRequest>> {
    budget.node(depth)?;
    Ok(match value {
        PaneRequest::Inspect => {
            let offset = g::PaneRequestInspect::create(builder);
            g::PaneRequest::create_pane_request_inspect(builder, offset)
        }
        PaneRequest::Attach { spec } => {
            let spec = write_view_spec(builder, spec, budget, depth + 1)?;
            let offset = g::PaneRequestAttach::create(builder, spec);
            g::PaneRequest::create_pane_request_attach(builder, offset)
        }
        PaneRequest::Update { view_id, spec } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let spec = write_view_spec(builder, spec, budget, depth + 1)?;
            let offset = g::PaneRequestUpdate::create(builder, view_id, spec);
            g::PaneRequest::create_pane_request_update(builder, offset)
        }
        PaneRequest::Detach { view_id } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let offset = g::PaneRequestDetach::create(builder, view_id);
            g::PaneRequest::create_pane_request_detach(builder, offset)
        }
        PaneRequest::Poll {
            view_id,
            after,
            frame_after,
            resources,
            snapshot_after,
            wait_ms,
            row_deltas,
            scene_encoding,
        } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let after = *after;
            let frame_after = *frame_after;
            let resources = {
                budget.vector(resources.len(), 8)?;
                let mut values = Vec::with_capacity(resources.len());
                for value in resources.iter() {
                    values.push(write_image_key(builder, value, budget, depth + 1)?);
                }
                builder.create_vector(&values)
            };
            let snapshot_after = *snapshot_after;
            let wait_ms = *wait_ms;
            let row_deltas = *row_deltas;
            let scene_encoding = scene_encoding
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok({
                        budget.bytes(value.len())?;
                        builder.create_string(value.as_str())
                    })
                })
                .transpose()?;
            let offset = g::PaneRequestPoll::create(
                builder,
                view_id,
                after,
                frame_after,
                resources,
                snapshot_after,
                wait_ms,
                row_deltas,
                scene_encoding,
            );
            g::PaneRequest::create_pane_request_poll(builder, offset)
        }
        PaneRequest::Pin { view_id, size } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let size = size
                .as_ref()
                .map(|value| write_size(builder, value, budget, depth + 1))
                .transpose()?;
            let offset = g::PaneRequestPin::create(builder, view_id, size);
            g::PaneRequest::create_pane_request_pin(builder, offset)
        }
        PaneRequest::Input { view_id, data } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let data = {
                budget.bytes(data.len())?;
                builder.create_vector(data.as_slice())
            };
            let offset = g::PaneRequestInput::create(builder, view_id, data);
            g::PaneRequest::create_pane_request_input(builder, offset)
        }
        PaneRequest::Paste { view_id, data } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let data = {
                budget.bytes(data.len())?;
                builder.create_vector(data.as_slice())
            };
            let offset = g::PaneRequestPaste::create(builder, view_id, data);
            g::PaneRequest::create_pane_request_paste(builder, offset)
        }
        PaneRequest::PasteContents {
            view_id,
            contents,
            location,
            allow_unsafe,
        } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let contents = {
                budget.vector(contents.len(), 8)?;
                let mut values = Vec::with_capacity(contents.len());
                for value in contents.iter() {
                    values.push(write_clipboard_content(builder, value, budget, depth + 1)?);
                }
                builder.create_vector(&values)
            };
            let location = write_clipboard_location(*location);
            let allow_unsafe = *allow_unsafe;
            let offset = g::PaneRequestPasteContents::create(
                builder,
                view_id,
                contents,
                location,
                allow_unsafe,
            );
            g::PaneRequest::create_pane_request_paste_contents(builder, offset)
        }
        PaneRequest::Key { view_id, event } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let event = write_key_event(builder, event, budget, depth + 1)?;
            let offset = g::PaneRequestKey::create(builder, view_id, event);
            g::PaneRequest::create_pane_request_key(builder, offset)
        }
        PaneRequest::Mouse { view_id, event } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let event = write_mouse_event(builder, event, budget, depth + 1)?;
            let offset = g::PaneRequestMouse::create(builder, view_id, event);
            g::PaneRequest::create_pane_request_mouse(builder, offset)
        }
        PaneRequest::Focus { view_id, focused } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let focused = *focused;
            let offset = g::PaneRequestFocus::create(builder, view_id, focused);
            g::PaneRequest::create_pane_request_focus(builder, offset)
        }
        PaneRequest::Scroll { view_id, delta } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let delta = *delta;
            let offset = g::PaneRequestScroll::create(builder, view_id, delta);
            g::PaneRequest::create_pane_request_scroll(builder, offset)
        }
        PaneRequest::Select { view_id, event } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let event = write_selection_event(builder, event, budget, depth + 1)?;
            let offset = g::PaneRequestSelect::create(builder, view_id, event);
            g::PaneRequest::create_pane_request_select(builder, offset)
        }
        PaneRequest::CopySelection { view_id } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let offset = g::PaneRequestCopySelection::create(builder, view_id);
            g::PaneRequest::create_pane_request_copy_selection(builder, offset)
        }
        PaneRequest::Search { view_id, action } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let action = write_search_action(builder, action, budget, depth + 1)?;
            let offset = g::PaneRequestSearch::create(builder, view_id, action);
            g::PaneRequest::create_pane_request_search(builder, offset)
        }
        PaneRequest::Clipboard {
            view_id,
            request_id,
            response,
        } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let request_id = {
                budget.bytes(request_id.len())?;
                builder.create_string(request_id.as_str())
            };
            let response = write_clipboard_response(builder, response, budget, depth + 1)?;
            let offset = g::PaneRequestClipboard::create(builder, view_id, request_id, response);
            g::PaneRequest::create_pane_request_clipboard(builder, offset)
        }
        PaneRequest::Colors {
            view_id,
            foreground,
            background,
            cursor,
            palette,
        } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let foreground = g::Rgb {
                r: foreground[0],
                g: foreground[1],
                b: foreground[2],
            };
            let background = g::Rgb {
                r: background[0],
                g: background[1],
                b: background[2],
            };
            let cursor = cursor
                .as_ref()
                .map(|value| -> Result<_> {
                    Ok(g::Rgb {
                        r: value[0],
                        g: value[1],
                        b: value[2],
                    })
                })
                .transpose()?;
            let palette = palette
                .as_ref()
                .map(|value| -> Result<_> {
                    let value = {
                        budget.vector(value.len(), 8)?;
                        let mut values = Vec::with_capacity(value.len());
                        for value in value.iter() {
                            values.push(g::Rgb {
                                r: value[0],
                                g: value[1],
                                b: value[2],
                            });
                        }
                        builder.create_vector(&values)
                    };
                    Ok(g::OptionalPalette::create(builder, value))
                })
                .transpose()?;
            let offset = g::PaneRequestColors::create(
                builder, view_id, foreground, background, cursor, palette,
            );
            g::PaneRequest::create_pane_request_colors(builder, offset)
        }
        PaneRequest::Terminate { view_id } => {
            let view_id = {
                budget.bytes(view_id.len())?;
                builder.create_string(view_id.as_str())
            };
            let offset = g::PaneRequestTerminate::create(builder, view_id);
            g::PaneRequest::create_pane_request_terminate(builder, offset)
        }
        PaneRequest::Shutdown => {
            let offset = g::PaneRequestShutdown::create(builder);
            g::PaneRequest::create_pane_request_shutdown(builder, offset)
        }
    })
}

pub(super) fn read_pane_request(
    value: g::PaneRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<PaneRequest> {
    budget.node(depth)?;
    Ok(match value {
        g::PaneRequestRef::PaneRequestInspect(_value) => PaneRequest::Inspect,
        g::PaneRequestRef::PaneRequestAttach(value) => PaneRequest::Attach {
            spec: read_view_spec(value.spec()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestUpdate(value) => PaneRequest::Update {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            spec: read_view_spec(value.spec()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestDetach(value) => PaneRequest::Detach {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::PaneRequestRef::PaneRequestPoll(value) => PaneRequest::Poll {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            after: value.after()?,
            frame_after: value.frame_after()?,
            resources: {
                let vector = value.resources()?;
                budget.vector(vector.len(), std::mem::size_of::<ImageKey>())?;
                let mut values = Vec::with_capacity(vector.len());
                for value in vector {
                    let value = value?;
                    values.push(read_image_key(value, budget, depth + 1)?);
                }
                values
            },
            snapshot_after: value.snapshot_after()?,
            wait_ms: value.wait_ms()?,
            row_deltas: value.row_deltas()?,
            scene_encoding: value
                .scene_encoding()?
                .map(|value| -> Result<_> {
                    Ok({
                        let text = value;
                        budget.bytes(text.len())?;
                        text.to_owned()
                    })
                })
                .transpose()?,
        },
        g::PaneRequestRef::PaneRequestPin(value) => PaneRequest::Pin {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            size: value
                .size()?
                .map(|value| read_size(value, budget, depth + 1))
                .transpose()?,
        },
        g::PaneRequestRef::PaneRequestInput(value) => PaneRequest::Input {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            data: {
                let bytes = value.data()?;
                budget.bytes(bytes.len())?;
                bytes.to_vec()
            },
        },
        g::PaneRequestRef::PaneRequestPaste(value) => PaneRequest::Paste {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            data: {
                let bytes = value.data()?;
                budget.bytes(bytes.len())?;
                bytes.to_vec()
            },
        },
        g::PaneRequestRef::PaneRequestPasteContents(value) => PaneRequest::PasteContents {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
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
            location: read_clipboard_location(value.location()?),
            allow_unsafe: value.allow_unsafe()?,
        },
        g::PaneRequestRef::PaneRequestKey(value) => PaneRequest::Key {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            event: read_key_event(value.event()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestMouse(value) => PaneRequest::Mouse {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            event: read_mouse_event(value.event()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestFocus(value) => PaneRequest::Focus {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            focused: value.focused()?,
        },
        g::PaneRequestRef::PaneRequestScroll(value) => PaneRequest::Scroll {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            delta: value.delta()?,
        },
        g::PaneRequestRef::PaneRequestSelect(value) => PaneRequest::Select {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            event: read_selection_event(value.event()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestCopySelection(value) => PaneRequest::CopySelection {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::PaneRequestRef::PaneRequestSearch(value) => PaneRequest::Search {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            action: read_search_action(value.action()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestClipboard(value) => PaneRequest::Clipboard {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            request_id: {
                let text = value.request_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            response: read_clipboard_response(value.response()?, budget, depth + 1)?,
        },
        g::PaneRequestRef::PaneRequestColors(value) => PaneRequest::Colors {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
            foreground: {
                let rgb = value.foreground()?;
                [rgb.r(), rgb.g(), rgb.b()]
            },
            background: {
                let rgb = value.background()?;
                [rgb.r(), rgb.g(), rgb.b()]
            },
            cursor: value
                .cursor()?
                .map(|value| -> Result<_> {
                    Ok({
                        let rgb = value;
                        [rgb.r(), rgb.g(), rgb.b()]
                    })
                })
                .transpose()?,
            palette: value
                .palette()?
                .map(|value| -> Result<_> {
                    let vector = value.value()?;
                    budget.vector(vector.len(), std::mem::size_of::<[u8; 3]>())?;
                    Ok(vector
                        .iter()
                        .map(|rgb| [rgb.r(), rgb.g(), rgb.b()])
                        .collect())
                })
                .transpose()?,
        },
        g::PaneRequestRef::PaneRequestTerminate(value) => PaneRequest::Terminate {
            view_id: {
                let text = value.view_id()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::PaneRequestRef::PaneRequestShutdown(_value) => PaneRequest::Shutdown,
    })
}

pub(super) fn write_search_action(
    builder: &mut Builder,
    value: &SearchAction,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::SearchAction>> {
    budget.node(depth)?;
    Ok(match value {
        SearchAction::Query { text } => {
            let text = {
                budget.bytes(text.len())?;
                builder.create_string(text.as_str())
            };
            let offset = g::SearchActionQuery::create(builder, text);
            g::SearchAction::create_search_action_query(builder, offset)
        }
        SearchAction::Next => {
            let offset = g::SearchActionNext::create(builder);
            g::SearchAction::create_search_action_next(builder, offset)
        }
        SearchAction::Previous => {
            let offset = g::SearchActionPrevious::create(builder);
            g::SearchAction::create_search_action_previous(builder, offset)
        }
        SearchAction::Clear => {
            let offset = g::SearchActionClear::create(builder);
            g::SearchAction::create_search_action_clear(builder, offset)
        }
    })
}

pub(super) fn read_search_action(
    value: g::SearchActionRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<SearchAction> {
    budget.node(depth)?;
    Ok(match value {
        g::SearchActionRef::SearchActionQuery(value) => SearchAction::Query {
            text: {
                let text = value.text()?;
                budget.bytes(text.len())?;
                text.to_owned()
            },
        },
        g::SearchActionRef::SearchActionNext(_value) => SearchAction::Next,
        g::SearchActionRef::SearchActionPrevious(_value) => SearchAction::Previous,
        g::SearchActionRef::SearchActionClear(_value) => SearchAction::Clear,
    })
}
