use crate::ui;
use serde::{Deserialize, Serialize};
use shephrd::ci_contract::CiRefreshTarget;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

pub const UPDATE: &str = "shephrd-tooltip";
pub const READY: &str = "shephrd-tooltip-ready";
pub const DISMISS: &str = "shephrd-tooltip-dismiss";
pub const REFRESH: &str = "shephrd-tooltip-refresh";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ready {
    pub id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissNotice {
    pub instance: String,
    pub key: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub instance: String,
    pub revision: u64,
    pub key: String,
    pub target: CiRefreshTarget,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Tooltip {
    pub instance: String,
    pub revision: u64,
    pub visible: bool,
    pub content: ui::PopupContent,
    pub coordinates: FloatingPaneCoordinates,
    #[serde(default)]
    pub connection: Option<JoinConnection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinConnection {
    pub side: ui::JoinSide,
    /// Inclusive source outline band, relative to the popup's top row.
    pub top: usize,
    pub bottom: usize,
}

pub struct Placement {
    pub coordinates: FloatingPaneCoordinates,
    pub connection: JoinConnection,
}

pub struct Controller {
    pub id: Option<u32>,
    pub pending: Option<Tooltip>,
    revision: u64,
    open_revision: u64,
    launched: bool,
    instance: String,
    refresh_requested: Vec<CiRefreshTarget>,
}

impl Default for Controller {
    fn default() -> Self {
        let instance = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_nanos().to_string())
            .unwrap_or_else(|_| "clock-unavailable".to_owned());
        Self {
            id: None,
            pending: None,
            revision: 0,
            open_revision: 0,
            launched: false,
            instance,
            refresh_requested: Vec::new(),
        }
    }
}

impl Controller {
    pub fn begin_open(&mut self) {
        self.open_revision = self.revision.saturating_add(1);
    }

    pub fn update(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        tooltip: Option<(&ui::PopupContent, Placement)>,
    ) {
        let empty = ui::PopupContent::default();
        let (visible, content, coordinates, connection) = match tooltip {
            Some((content, placement)) => (
                true,
                content,
                placement.coordinates,
                Some(placement.connection),
            ),
            None => (false, &empty, FloatingPaneCoordinates::default(), None),
        };
        if self.pending.as_ref().is_some_and(|old| {
            old.visible == visible
                && &old.content == content
                && old.coordinates == coordinates
                && old.connection == connection
        }) {
            return;
        }
        self.refresh_requested.retain(|target| {
            visible
                && content
                    .rows()
                    .iter()
                    .any(|row| row.refresh.as_ref() == Some(target))
        });
        self.revision = self.revision.saturating_add(1);
        self.pending = Some(Tooltip {
            instance: self.instance.clone(),
            revision: self.revision,
            visible,
            content: content.clone(),
            coordinates,
            connection,
        });
        if self.id.is_some() || visible {
            self.send(owner, configuration);
        }
    }

    pub fn accept_refresh(&mut self, source: u32, request: &RefreshRequest) -> bool {
        if self.id != Some(source)
            || self.refresh_requested.contains(&request.target)
            || !self.pending.as_ref().is_some_and(|state| {
                state.visible
                    && state.instance == request.instance
                    && state.revision == request.revision
                    && state.content.key == request.key
                    && state
                        .content
                        .rows()
                        .iter()
                        .any(|row| row.refresh.as_ref() == Some(&request.target))
            })
        {
            return false;
        }
        self.refresh_requested.push(request.target.clone());
        true
    }

    pub fn accept_dismiss(&self, source: u32, notice: &DismissNotice) -> bool {
        self.id == Some(source)
            && self.pending.as_ref().is_some_and(|state| {
                state.visible
                    && state.instance == notice.instance
                    && state.content.key == notice.key
                    && notice.revision >= self.open_revision
                    && notice.revision <= state.revision
            })
    }

    pub fn ready(
        &mut self,
        ready: &Ready,
        owner: u32,
        configuration: &BTreeMap<String, String>,
    ) -> bool {
        if self.id.is_some_and(|id| id != ready.id) {
            return false;
        }
        let first = self.id.is_none();
        self.id = Some(ready.id);
        self.launched = true;
        if first {
            self.send(owner, configuration);
        }
        true
    }

    fn send(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        let Some(tooltip) = &self.pending else { return };
        let Ok(payload) = serde_json::to_string(tooltip) else {
            return;
        };
        let mut message = MessageToPlugin::new(UPDATE).with_payload(payload);
        if let Some(id) = self.id {
            message.destination_plugin_id = Some(id);
        } else {
            let Some(url) = configuration.get("plugin_url") else {
                return;
            };
            let mut config = configuration.clone();
            config.insert("role".to_owned(), "overlay".to_owned());
            config.insert("owner".to_owned(), owner.to_string());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("shephrd details".to_owned()),
                ..NewPluginArgs::default()
            });
            message.floating_pane_coordinates = Some(tooltip.coordinates.clone());
            // Exact URL/config identity makes repeated sends target the same plugin instance.
            self.launched = true;
        }
        pipe_message_to_plugin(message);
    }

    pub fn forget(&mut self, id: u32) {
        if self.id == Some(id) {
            self.id = None;
            self.launched = false;
            self.pending = None;
        }
    }

    pub fn close(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        if let Some(id) = self.id.take() {
            close_plugin_pane(id);
        } else if self.launched {
            // An in-flight load is still sent the hidden state; it also watches owner lifetime.
            self.update(owner, configuration, None);
        }
        self.launched = false;
    }
}

pub fn geometry(
    sidebar: &PaneInfo,
    rendered_columns: usize,
    tab: &TabInfo,
    band: std::ops::RangeInclusive<usize>,
    content: &ui::PopupContent,
) -> Option<Placement> {
    // Bound the join to the painted content edge, not the pane's outer separator/frame.
    let left = if sidebar.pane_content_columns > 0 {
        sidebar.pane_content_x
    } else {
        sidebar.pane_x
    };
    let columns = match (rendered_columns, sidebar.pane_content_columns) {
        (0, reported) => reported,
        (rendered, 0) => rendered,
        (rendered, reported) => rendered.min(reported),
    };
    let right = if columns > 0 {
        left.saturating_add(columns)
    } else {
        sidebar.pane_x.saturating_add(sidebar.pane_columns)
    };
    let right_space = tab.display_area_columns.saturating_sub(right);
    let left_space = left;
    let (x, available, side) = if right_space >= 12 {
        (right, right_space, ui::JoinSide::Right)
    } else if left_space >= 12 {
        (0, left_space, ui::JoinSide::Left)
    } else {
        return None;
    };
    let source_top = sidebar.pane_content_y.saturating_add(*band.start());
    if source_top >= tab.display_area_rows || band.is_empty() {
        return None;
    }
    let source_bottom = sidebar
        .pane_content_y
        .saturating_add(*band.end())
        .min(tab.display_area_rows.saturating_sub(1));
    if source_bottom < source_top {
        return None;
    }
    let source_rows = content.rows();
    let width = source_rows
        .iter()
        .map(|row| {
            let right: usize = row
                .right
                .iter()
                .map(|span| ui::width(&ui::clean(&span.text)))
                .sum();
            ui::width(&row.plain()) + right + usize::from(right != 0)
        })
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .clamp(12, 64)
        .min(available);
    let height = wrapped_rows(&source_rows, width.saturating_sub(2))
        .len()
        .saturating_add(3)
        .max(source_bottom - source_top + 1)
        .min(tab.display_area_rows)
        .max(1);
    let x = if side == ui::JoinSide::Right {
        x
    } else {
        left_space.saturating_sub(width)
    };
    let y = source_top.min(tab.display_area_rows.saturating_sub(height));
    Some(Placement {
        connection: JoinConnection {
            side,
            top: source_top - y,
            bottom: source_bottom - y,
        },
        coordinates: FloatingPaneCoordinates {
            pinned: Some(true),
            borderless: Some(true),
            ..FloatingPaneCoordinates::default()
                .with_x_fixed(x)
                .with_y_fixed(y)
                .with_width_fixed(width)
                .with_height_fixed(height)
        },
    })
}

#[derive(Default)]
struct PopupLayout {
    columns: usize,
    lines: Vec<PopupLine>,
    scroll: usize,
}

struct PopupLine {
    row: ui::PopupRow,
}

fn wrapped_rows(rows: &[ui::PopupRow], columns: usize) -> Vec<PopupLine> {
    let mut wrapped = Vec::new();
    for row in rows {
        wrap_row(row, columns, &mut wrapped);
    }
    wrapped
}

fn trim_line_end(line: &mut ui::PopupRow) {
    while let Some(span) = line.spans.last_mut() {
        let length = span.text.trim_end_matches(char::is_whitespace).len();
        span.text.truncate(length);
        if !span.text.is_empty() {
            break;
        }
        line.spans.pop();
    }
}

fn wrap_row(row: &ui::PopupRow, columns: usize, output: &mut Vec<PopupLine>) {
    let right: usize = row
        .right
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let button = row
        .right
        .iter()
        .any(|span| span.text == ui::refresh_button_text());
    let right_columns = if button && right > columns {
        0
    } else {
        right.min(if row.suffix.is_empty() {
            columns
        } else {
            columns / 2
        })
    };
    let body_columns = columns
        .saturating_sub(right_columns + usize::from(right_columns != 0 && right_columns < columns));
    let suffix_width: usize = row
        .suffix
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let suffix = if suffix_width < body_columns {
        row.suffix.clone()
    } else {
        Vec::new()
    };
    let suffix_width: usize = suffix
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let body_columns = body_columns.saturating_sub(suffix_width);
    let clipped = row.truncate
        && row
            .spans
            .iter()
            .map(|span| ui::width(&ui::clean(&span.text)))
            .sum::<usize>()
            > body_columns;
    let budget = body_columns.saturating_sub(usize::from(clipped));
    let mut right = if button && right_columns == 0 {
        Vec::new()
    } else {
        row.right.clone()
    };
    let mut remaining = right_columns;
    for span in &mut right {
        span.text = ui::truncate(&span.text, remaining);
        remaining = remaining.saturating_sub(ui::width(&span.text));
    }
    let mut line = ui::PopupRow {
        suffix,
        right,
        link: row.link.clone(),
        refresh: row
            .refresh
            .clone()
            .filter(|_| right_columns >= ui::refresh_button_width()),
        ..ui::PopupRow::default()
    };
    let mut used = 0;
    if body_columns == 0 {
        output.push(PopupLine { row: line });
        return;
    }
    'spans: for span in &row.spans {
        let text = ui::clean(&span.text);
        let mut part = String::new();
        for token in text.split_inclusive(char::is_whitespace) {
            let word = token.trim_end_matches(char::is_whitespace);
            let word_width = ui::width(word);
            if !row.truncate && word_width <= budget && used > 0 && used + word_width > budget {
                if !part.is_empty() {
                    line.spans.push(ui::PopupSpan {
                        text: std::mem::take(&mut part),
                        color: span.color.clone(),
                        bold: span.bold,
                    });
                }
                trim_line_end(&mut line);
                output.push(PopupLine { row: line });
                line = ui::PopupRow {
                    link: row.link.clone(),
                    ..ui::PopupRow::default()
                };
                used = 0;
            }
            if !row.truncate && word_width <= budget {
                if used == 0 && word.is_empty() {
                    continue;
                }
                part.push_str(word);
                used += word_width;
                for character in token[word.len()..].chars() {
                    let mut bytes = [0; 4];
                    let cells = ui::width(character.encode_utf8(&mut bytes));
                    if used > 0 && used + cells <= budget {
                        part.push(character);
                        used += cells;
                    }
                }
                continue;
            }
            // Only tokens wider than a complete line need character-level fallback.
            for character in token.chars() {
                let mut bytes = [0; 4];
                let cells = ui::width(character.encode_utf8(&mut bytes));
                if used + cells > budget {
                    if character.is_whitespace() && !row.truncate {
                        continue;
                    }
                    if !part.is_empty() {
                        line.spans.push(ui::PopupSpan {
                            text: std::mem::take(&mut part),
                            color: span.color.clone(),
                            bold: span.bold,
                        });
                    }
                    if row.truncate {
                        break 'spans;
                    }
                    trim_line_end(&mut line);
                    output.push(PopupLine { row: line });
                    line = ui::PopupRow {
                        link: row.link.clone(),
                        ..ui::PopupRow::default()
                    };
                    used = 0;
                }
                if cells <= budget {
                    part.push(character);
                    used += cells;
                }
            }
        }
        if !part.is_empty() {
            line.spans.push(ui::PopupSpan {
                text: part,
                color: span.color.clone(),
                bold: span.bold,
            });
        }
    }
    if !row.truncate {
        trim_line_end(&mut line);
    }
    if clipped {
        line.spans.push(ui::PopupSpan {
            text: "…".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
        });
    }
    output.push(PopupLine { row: line });
}

const OSC8_CLOSE: &str = "\u{1b}]8;;\u{1b}\\";

#[cfg(test)]
fn render_line(row: &ui::PopupRow, columns: usize) -> String {
    render_action_line(row, columns, false)
}

fn render_action_line(row: &ui::PopupRow, columns: usize, hovered: bool) -> String {
    let right: usize = row
        .right
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let button = row
        .right
        .iter()
        .any(|span| span.text == ui::refresh_button_text());
    let right_columns = if button && right > columns {
        0
    } else {
        right.min(if row.suffix.is_empty() {
            columns
        } else {
            columns / 2
        })
    };
    let body_columns = columns
        .saturating_sub(right_columns + usize::from(right_columns != 0 && right_columns < columns));
    let suffix_width: usize = row
        .suffix
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let suffix = if suffix_width < body_columns {
        row.suffix.as_slice()
    } else {
        &[]
    };
    let suffix_width: usize = suffix
        .iter()
        .map(|span| ui::width(&ui::clean(&span.text)))
        .sum();
    let mut rendered = String::from(OSC8_CLOSE);
    if let Some(link) = row.link.as_deref().and_then(ui::safe_link) {
        rendered.push_str("\u{1b}]8;;");
        rendered.push_str(link);
        rendered.push_str("\u{1b}\\");
    }
    let mut used = 0;
    for span in &row.spans {
        let text = ui::truncate(&span.text, body_columns.saturating_sub(suffix_width + used));
        used += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        rendered.push_str(span.color.ansi());
        if span.bold {
            rendered.push_str("\u{1b}[1m");
        }
        rendered.push_str(&text);
    }
    for span in suffix {
        let text = ui::clean(&span.text);
        used += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        rendered.push_str(span.color.ansi());
        rendered.push_str(&text);
    }
    rendered.push_str(OSC8_CLOSE);
    rendered.push_str(ui::MUTED);
    rendered.push_str(&" ".repeat(columns.saturating_sub(used + right_columns)));
    let mut used_right = 0;
    for span in &row.right {
        if hovered && row.refresh.is_some() {
            rendered.push_str("\u{1b}[1;38;2;30;30;46;48;2;137;180;250m");
        }
        let text = ui::truncate(&span.text, right_columns.saturating_sub(used_right));
        used_right += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        if !(hovered && row.refresh.is_some()) {
            rendered.push_str(span.color.ansi());
        }
        if span.bold || (hovered && row.refresh.is_some()) {
            rendered.push_str("\u{1b}[1m");
        }
        rendered.push_str(&text);
    }
    rendered.push_str(ui::RESET);
    rendered.push_str(ui::BASE);
    rendered
}

fn outline_edges(
    row: usize,
    rows: usize,
    connection: Option<&JoinConnection>,
) -> (&'static str, &'static str) {
    let top = row == 0;
    let bottom = row + 1 == rows;
    let mut left = if top {
        "╭"
    } else if bottom {
        "╰"
    } else {
        "│"
    };
    let mut right = if top {
        "╮"
    } else if bottom {
        "╯"
    } else {
        "│"
    };
    if let Some(connection) = connection
        .filter(|connection| connection.top <= connection.bottom && connection.bottom < rows)
    {
        let edge = if connection.top == connection.bottom && row == connection.top {
            if top || bottom {
                "─"
            } else if connection.side == ui::JoinSide::Right {
                "┤"
            } else {
                "├"
            }
        } else if top && connection.top == 0 || bottom && connection.bottom + 1 == rows {
            "─"
        } else if row == connection.top {
            if connection.side == ui::JoinSide::Right {
                "╯"
            } else {
                "╰"
            }
        } else if row == connection.bottom {
            if connection.side == ui::JoinSide::Right {
                "╮"
            } else {
                "╭"
            }
        } else if (connection.top..=connection.bottom).contains(&row) {
            " "
        } else if top {
            if connection.side == ui::JoinSide::Right {
                "╭"
            } else {
                "╮"
            }
        } else if bottom {
            if connection.side == ui::JoinSide::Right {
                "╰"
            } else {
                "╯"
            }
        } else {
            "│"
        };
        match connection.side {
            ui::JoinSide::Right => left = edge,
            ui::JoinSide::Left => right = edge,
        }
    }
    (left, right)
}

fn outlined_line(
    text: String,
    row: usize,
    rows: usize,
    columns: usize,
    connection: Option<&JoinConnection>,
) -> String {
    let (left, right) = outline_edges(row, rows, connection);
    let center = if row == 0 || row + 1 == rows {
        format!("{}{}", ui::BRIGHT, "─".repeat(columns.saturating_sub(2)))
    } else {
        text
    };
    if columns >= 2 {
        format!(
            "{OSC8_CLOSE}{}{left}{center}{OSC8_CLOSE}{}{right}{OSC8_CLOSE}",
            ui::BRIGHT,
            ui::BRIGHT
        )
    } else if columns == 1 {
        format!("{OSC8_CLOSE}{}{left}{OSC8_CLOSE}", ui::BRIGHT)
    } else {
        OSC8_CLOSE.to_owned()
    }
}

#[derive(Default)]
pub struct Overlay {
    pub owner: Option<u32>,
    pub id: u32,
    granted: bool,
    owner_seen: bool,
    refresh_hover: Option<CiRefreshTarget>,
    state: Option<Tooltip>,
    error: Option<String>,
    layout: RefCell<PopupLayout>,
    viewport: Cell<usize>,
    columns: Cell<usize>,
    rows: Cell<usize>,
    refresh_requested: Vec<CiRefreshTarget>,
}

impl Overlay {
    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32) {
        self.id = id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::BeforeClose,
            EventType::Mouse,
        ]);
        request_permission(super::PERMISSIONS);
    }

    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                set_selectable(false);
                if self
                    .owner
                    .is_some_and(|owner| get_pane_info(PaneId::Plugin(owner)).is_some())
                {
                    self.owner_seen = true;
                    self.apply();
                    self.announce();
                } else {
                    close_self();
                }
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Tooltip permissions denied".to_owned());
            }
            Event::PaneUpdate(manifest) => {
                let found = manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| pane.is_plugin && Some(pane.id) == self.owner);
                if found {
                    self.owner_seen = true;
                } else if self.owner_seen && self.granted {
                    close_self();
                }
            }
            Event::Mouse(Mouse::LeftClick(-1, column) | Mouse::RightClick(-1, column))
                if self.granted && column == u16::MAX as usize =>
            {
                if let (Some(owner), Some(state)) = (self.owner, &self.state)
                    && state.visible
                {
                    let notice = DismissNotice {
                        instance: state.instance.clone(),
                        key: state.content.key.clone(),
                        revision: state.revision,
                    };
                    if let Ok(payload) = serde_json::to_string(&notice) {
                        pipe_message_to_plugin(
                            MessageToPlugin::new(DISMISS)
                                .with_destination_plugin_id(owner)
                                .with_payload(payload),
                        );
                    }
                }
            }
            Event::Mouse(Mouse::Hover(line, column)) if self.granted => {
                self.refresh_hover = self.refresh_at(line, column).map(|request| request.target);
            }
            Event::Mouse(Mouse::LeftClick(line, column)) if self.granted => {
                if let (Some(owner), Some(request)) = (self.owner, self.refresh_at(line, column))
                    && let Ok(payload) = serde_json::to_string(&request)
                {
                    self.refresh_requested.push(request.target);
                    pipe_message_to_plugin(
                        MessageToPlugin::new(REFRESH)
                            .with_destination_plugin_id(owner)
                            .with_payload(payload),
                    );
                }
            }
            Event::Mouse(Mouse::ScrollUp(lines)) if self.granted => {
                self.refresh_hover = None;
                let mut layout = self.layout.borrow_mut();
                layout.scroll = layout.scroll.saturating_sub(lines);
            }
            Event::Mouse(Mouse::ScrollDown(lines)) if self.granted => {
                self.refresh_hover = None;
                let mut layout = self.layout.borrow_mut();
                layout.scroll = layout
                    .scroll
                    .saturating_add(lines)
                    .min(layout.lines.len().saturating_sub(self.viewport.get()));
            }
            Event::PaneClosed(PaneId::Plugin(id)) if Some(id) == self.owner && self.granted => {
                close_self()
            }
            _ => {}
        }
        true
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name != UPDATE
            || !matches!(message.source, PipeSource::Plugin(id) if Some(id) == self.owner)
        {
            return false;
        }
        let Some(payload) = message.payload else {
            return false;
        };
        if payload.len() > 32 * 1024 {
            return false;
        }
        let Ok(tooltip) = serde_json::from_str::<Tooltip>(&payload) else {
            return false;
        };
        if self.state.as_ref().is_some_and(|current| {
            tooltip.instance == current.instance && tooltip.revision <= current.revision
        }) {
            return false;
        }
        let mut layout = self.layout.borrow_mut();
        self.refresh_requested.retain(|target| {
            tooltip.visible
                && tooltip
                    .content
                    .rows()
                    .iter()
                    .any(|row| row.refresh.as_ref() == Some(target))
        });
        if self.state.as_ref().map(|state| &state.content.key) != Some(&tooltip.content.key)
            || !tooltip.visible
        {
            layout.scroll = 0;
            self.refresh_hover = None;
        }
        layout.columns = usize::MAX;
        drop(layout);
        self.state = Some(tooltip);
        self.apply();
        if self.granted {
            self.announce();
        }
        true
    }

    fn refresh_at(&self, line: isize, column: usize) -> Option<RefreshRequest> {
        let state = self.state.as_ref().filter(|state| state.visible)?;
        if self.error.is_some() || line < 1 || line as usize > self.viewport.get() {
            return None;
        }
        let layout = self.layout.borrow();
        // An update invalidates layout until the corresponding frame is actually painted.
        if layout.columns != self.columns.get().saturating_sub(2) {
            return None;
        }
        let row = &layout.lines.get(line as usize - 1 + layout.scroll)?.row;
        let target = row.refresh.as_ref()?;
        let icon_width = ui::refresh_button_width();
        let end = self.columns.get().checked_sub(1)?;
        if icon_width > layout.columns
            || column < end.saturating_sub(icon_width)
            || column >= end
            || self.refresh_requested.contains(target)
        {
            return None;
        }
        Some(RefreshRequest {
            instance: state.instance.clone(),
            revision: state.revision,
            key: state.content.key.clone(),
            target: target.clone(),
        })
    }

    fn apply(&self) {
        if !self.granted {
            return;
        }
        let pane = PaneId::Plugin(self.id);
        match &self.state {
            Some(state) if state.visible => {
                change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
                // Unlike show_self(), this does not transfer focus from the real terminal.
                show_pane_with_id(pane, true, false);
            }
            _ => hide_self(),
        }
    }

    fn announce(&self) {
        let Some(owner) = self.owner else { return };
        let ready = Ready { id: self.id };
        if let Ok(payload) = serde_json::to_string(&ready) {
            pipe_message_to_plugin(
                MessageToPlugin::new(READY)
                    .with_destination_plugin_id(owner)
                    .with_payload(payload),
            );
        }
    }

    pub fn render(&self, rows: usize, columns: usize) {
        self.rows.set(rows);
        self.viewport.set(rows.saturating_sub(3));
        self.columns.set(columns);
        let mut layout = self.layout.borrow_mut();
        let content_columns = columns.saturating_sub(2);
        if layout.columns != content_columns {
            layout.lines = self
                .state
                .as_ref()
                .map(|state| wrapped_rows(&state.content.rows(), content_columns))
                .unwrap_or_default();
            layout.columns = content_columns;
        }
        layout.scroll = layout
            .scroll
            .min(layout.lines.len().saturating_sub(self.viewport.get()));
        let footer = if layout.lines.len() > self.viewport.get() {
            format!("↕ Scroll {}/{}", layout.scroll + 1, layout.lines.len())
        } else {
            String::new()
        };
        print!("{OSC8_CLOSE}");
        for row in 0..rows {
            let text = if let Some(error) = &self.error {
                if row == 1 {
                    format!(
                        "{}{}",
                        ui::status_color(shephrd::Status::Blocked),
                        ui::fit(error, content_columns)
                    )
                } else {
                    " ".repeat(content_columns)
                }
            } else if row > 0 && row + 2 == rows {
                format!("{}{}", ui::MUTED, ui::fit(&footer, content_columns))
            } else {
                row.checked_sub(1)
                    .and_then(|i| layout.lines.get(i + layout.scroll))
                    .map(|line| {
                        render_action_line(
                            &line.row,
                            content_columns,
                            line.row.refresh.as_ref().is_some_and(|target| {
                                self.refresh_hover.as_ref() == Some(target)
                                    && !self.refresh_requested.contains(target)
                            }),
                        )
                    })
                    .unwrap_or_else(|| " ".repeat(content_columns))
            };
            let connection = self
                .state
                .as_ref()
                .and_then(|state| state.connection.as_ref());
            let line = outlined_line(text, row, rows, columns, connection);
            print!(
                "\u{1b}[{};1H{}{}{line}{}",
                row + 1,
                ui::RESET,
                ui::BASE,
                ui::RESET
            );
        }
        print!("{OSC8_CLOSE}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, color: ui::PopupColor) -> ui::PopupRow {
        ui::PopupRow {
            spans: vec![ui::PopupSpan {
                text: text.to_owned(),
                color,
                bold: false,
            }],
            ..ui::PopupRow::default()
        }
    }

    #[derive(Default)]
    struct TerminalConsumer {
        link: Option<String>,
        glyphs: Vec<(char, Option<String>)>,
    }

    impl TerminalConsumer {
        fn consume(&mut self, rendered: &str) {
            let mut characters = rendered.chars();
            while let Some(character) = characters.next() {
                if character != '\u{1b}' {
                    assert!(!character.is_control());
                    self.glyphs.push((character, self.link.clone()));
                    continue;
                }
                match characters.next().expect("escape introducer") {
                    '[' => {
                        let final_byte = characters
                            .find(|character| ('@'..='~').contains(character))
                            .expect("CSI terminator");
                        assert_eq!(final_byte, 'm');
                    }
                    ']' => {
                        let mut command = String::new();
                        loop {
                            match characters.next().expect("OSC terminator") {
                                '\u{7}' => break,
                                '\u{1b}' => {
                                    assert_eq!(characters.next(), Some('\\'));
                                    break;
                                }
                                character => command.push(character),
                            }
                        }
                        let destination = command.strip_prefix("8;;").expect("OSC8 command");
                        self.link = (!destination.is_empty()).then(|| destination.to_owned());
                    }
                    other => panic!("unexpected escape {other:?}"),
                }
            }
        }

        fn text(&self) -> String {
            self.glyphs.iter().map(|(character, _)| character).collect()
        }

        fn cell_links(&self) -> Vec<Option<&str>> {
            self.glyphs
                .iter()
                .flat_map(|(character, link)| {
                    let mut bytes = [0; 4];
                    let width = ui::width(character.encode_utf8(&mut bytes));
                    std::iter::repeat_n(link.as_deref(), width)
                })
                .collect()
        }
    }

    fn visible(rendered: &str) -> String {
        let mut terminal = TerminalConsumer::default();
        terminal.consume(rendered);
        terminal.text()
    }

    fn refresh_header() -> ui::PopupRow {
        let mut header = row(
            "日本語 CI summary with long wrapping context",
            ui::PopupColor::Text,
        );
        header.right = vec![
            row(ui::refresh_button_text(), ui::PopupColor::Text)
                .spans
                .remove(0),
        ];
        header.link = Some("https://example.com/checks".to_owned());
        header.refresh = Some(CiRefreshTarget {
            repository: shephrd::ci_contract::ForgeRepository {
                provider: shephrd::ci_contract::ForgeProvider::Github,
                host: "forge.example.com".to_owned(),
                path: "owner/repository".to_owned(),
                web_url: "https://forge.example.com/owner/repository".to_owned(),
            },
            head_sha: "0123456789abcdef".to_owned(),
        });
        header
    }

    #[test]
    fn refresh_hit_is_only_the_rendered_unlinked_icon_not_wrapping_or_chrome() {
        let header = refresh_header();
        for columns in 0_usize..=100 {
            let mut overlay = Overlay {
                granted: true,
                owner: Some(7),
                state: Some(Tooltip {
                    instance: "instance".to_owned(),
                    revision: 42,
                    visible: true,
                    content: ui::PopupContent {
                        key: "pane:1".to_owned(),
                        intro: vec![
                            header.clone(),
                            row("check with duration", ui::PopupColor::Text),
                        ],
                        ..ui::PopupContent::default()
                    },
                    coordinates: FloatingPaneCoordinates::default(),
                    connection: None,
                }),
                ..Overlay::default()
            };
            overlay.render(9, columns);
            for line in -1..=9 {
                for column in 0..=columns {
                    let hit = overlay.refresh_at(line, column);
                    let expected = columns >= ui::refresh_button_width() + 2
                        && line == 3
                        && column >= columns - 1 - ui::refresh_button_width()
                        && column < columns - 1;
                    assert_eq!(hit.is_some(), expected, "{columns}: ({line}, {column})");
                    if let Some(hit) = hit {
                        assert_eq!(hit.target, header.refresh.clone().unwrap());
                        assert_eq!(hit.instance, "instance");
                        assert_eq!(hit.revision, 42);
                        assert_eq!(hit.key, "pane:1");
                    }
                }
            }
            if columns >= ui::refresh_button_width() + 2 {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&render_line(
                    &overlay.layout.borrow().lines[2].row,
                    columns - 2,
                ));
                assert_eq!(terminal.glyphs.last(), Some(&(']', None)));
                overlay.event(Event::Mouse(Mouse::LeftClick(3, columns - 2)));
                assert!(overlay.refresh_at(3, columns - 2).is_none());
                assert!(overlay.state.as_ref().unwrap().visible);
            }
        }
    }

    #[test]
    fn refresh_button_hover_loading_and_artwork_cells_are_bounded_and_unlinked() {
        let header = refresh_header();
        assert_eq!(
            ui::width(ui::refresh_button_text()),
            ui::refresh_button_width()
        );
        for columns in 0..=100 {
            for enabled in [true, false] {
                let mut source = header.clone();
                source.truncate = true;
                if !enabled {
                    source.refresh = None;
                    source.right[0].color = ui::PopupColor::Muted;
                }
                let lines = wrapped_rows(&[source], columns);
                assert_eq!(lines.len(), 1);
                let normal = render_action_line(&lines[0].row, columns, false);
                let hover = render_action_line(&lines[0].row, columns, true);
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&hover);
                assert_eq!(ui::width(&terminal.text()), columns);
                assert_eq!(
                    normal != hover,
                    enabled && columns >= ui::refresh_button_width()
                );
                if columns >= ui::refresh_button_width() {
                    assert!(terminal.text().ends_with(ui::refresh_button_text()));
                    let start = columns - ui::refresh_button_width();
                    assert!(terminal.cell_links()[start..].iter().all(Option::is_none));
                    assert!(terminal.text().ends_with("\u{f021}  ]"));
                } else {
                    assert!(!terminal.text().contains('\u{f021}'));
                }
            }
        }
    }

    #[test]
    fn compact_ci_reason_never_wraps_past_single_summary_row() {
        let mut summary = refresh_header();
        summary.spans = row(
            &format!(
                "CI · Unavailable · stale · {}",
                "provider failure 日本語 e\u{301} ".repeat(40)
            ),
            ui::PopupColor::Ci(shephrd::ci_contract::CiStatus::Unknown),
        )
        .spans;
        summary.truncate = true;
        for columns in 0..=100 {
            let lines = wrapped_rows(&[summary.clone()], columns);
            assert_eq!(lines.len(), 1);
            let mut terminal = TerminalConsumer::default();
            terminal.consume(&render_line(&lines[0].row, columns));
            assert_eq!(ui::width(&terminal.text()), columns);
            if columns >= ui::refresh_button_width() {
                assert!(terminal.text().ends_with(ui::refresh_button_text()));
            }
        }
    }

    #[test]
    fn request_titles_wrap_whole_words_at_the_normal_left_edge() {
        for prefix in ["icon #7 ", "icon #123456789 "] {
            let title = "日本語 title cafe\u{301} with enough words to wrap repeatedly";
            let mut source = row(title, ui::PopupColor::Text);
            source.spans.insert(
                0,
                ui::PopupSpan {
                    text: prefix.to_owned(),
                    color: ui::PopupColor::Native(shephrd::Status::Idle),
                    bold: true,
                },
            );
            source.link = Some("https://forge.example.com/repository/pull/7".to_owned());
            let original = source.plain();
            let widest_word = original.split_whitespace().map(ui::width).max().unwrap();
            for columns in 0..=100 {
                let lines = wrapped_rows(&[source.clone()], columns);
                for line in &lines {
                    let mut terminal = TerminalConsumer::default();
                    terminal.consume(&render_line(&line.row, columns));
                    assert_eq!(ui::width(&terminal.text()), columns);
                    if !line.row.spans.is_empty() {
                        assert!(!terminal.text().starts_with(' '));
                    }
                    for (glyph, link) in &terminal.glyphs {
                        if !glyph.is_whitespace() {
                            assert_eq!(link.as_deref(), source.link.as_deref());
                        }
                    }
                    assert!(terminal.link.is_none());
                }
                if columns >= widest_word {
                    let wrapped = lines
                        .iter()
                        .map(|line| line.row.plain())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        wrapped
                            .iter()
                            .flat_map(|line| line.split_whitespace())
                            .collect::<Vec<_>>(),
                        original.split_whitespace().collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    #[test]
    fn overlong_tokens_fit_without_losing_text_at_word_wrap_boundaries() {
        let source = row(
            "before abcdefghijklmnopqrstuvwxyz after",
            ui::PopupColor::Text,
        );
        let lines = wrapped_rows(&[source], 8);
        assert!(lines.iter().all(|line| ui::width(&line.row.plain()) <= 8));
        let joined = lines
            .iter()
            .map(|line| line.row.plain())
            .collect::<String>();
        let text = joined
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert_eq!(text, "beforeabcdefghijklmnopqrstuvwxyzafter");
    }

    #[test]
    fn refresh_hits_follow_scroll_and_reject_unpainted_or_loading_updates() {
        let header = refresh_header();
        let mut overlay = Overlay {
            granted: true,
            state: Some(Tooltip {
                instance: "instance".to_owned(),
                revision: 2,
                visible: true,
                content: ui::PopupContent {
                    key: "workspace:1".to_owned(),
                    intro: vec![header.clone(), row("check", ui::PopupColor::Text)],
                    ..ui::PopupContent::default()
                },
                coordinates: FloatingPaneCoordinates::default(),
                connection: None,
            }),
            ..Overlay::default()
        };
        overlay.render(5, 16);
        assert!(overlay.refresh_at(3, 14).is_none()); // Footer, not the hidden header.
        overlay.event(Event::Mouse(Mouse::ScrollDown(2)));
        overlay.render(5, 16);
        assert_eq!(
            overlay.refresh_at(1, 14).unwrap().target,
            header.refresh.unwrap()
        );
        assert!(overlay.refresh_at(2, 14).is_none()); // Wrapped continuation has no action.
        overlay.layout.borrow_mut().columns = usize::MAX;
        assert!(overlay.refresh_at(1, 14).is_none());
        overlay.state.as_mut().unwrap().content.intro[0].refresh = None;
        overlay.render(5, 16);
        assert!(overlay.refresh_at(1, 14).is_none());
        overlay.state.as_mut().unwrap().visible = false;
        assert!(overlay.refresh_at(1, 14).is_none());
    }

    #[test]
    fn popup_wraps_unicode_and_preserves_palette_boundaries_without_controls() {
        let rows = vec![ui::PopupRow {
            spans: vec![
                ui::PopupSpan {
                    text: "\u{f05e1} ".to_owned(),
                    color: ui::PopupColor::Ci(shephrd::ci_contract::CiStatus::Passed),
                    bold: false,
                },
                ui::PopupSpan {
                    text: "日本語\u{1b}\n checks".to_owned(),
                    color: ui::PopupColor::Text,
                    bold: true,
                },
            ],
            ..ui::PopupRow::default()
        }];
        for columns in 2..=64 {
            let lines = wrapped_rows(&rows, columns);
            assert!(
                lines
                    .iter()
                    .all(|line| ui::width(&line.row.plain()) <= columns)
            );
            for line in &lines {
                let rendered = render_line(&line.row, columns);
                assert_eq!(ui::width(&visible(&rendered)), columns);
                for span in &line.row.spans {
                    assert!(rendered.contains(span.color.ansi()));
                    assert!(!span.text.chars().any(char::is_control));
                }
            }
        }
    }

    #[test]
    fn check_rows_stay_single_line_at_all_widths() {
        let mut check = row(
            "\u{f05e1} 日本語 integration tests with a long name",
            ui::PopupColor::Text,
        );
        check.truncate = true;
        check.right = vec![ui::PopupSpan {
            text: "2:03".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
        }];
        check.link = Some("https://forge.test/owner/project/checks/12".to_owned());
        for columns in 0..=80 {
            let lines = wrapped_rows(&[check.clone()], columns);
            assert_eq!(lines.len(), 1);
            assert!(ui::width(&lines[0].row.plain()) <= columns);
            assert_eq!(
                ui::width(&visible(&render_line(&lines[0].row, columns))),
                columns
            );
            if columns >= 4 {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&render_line(&lines[0].row, columns));
                assert!(terminal.text().ends_with("2:03"));
                assert!(
                    terminal.cell_links()[columns - 4..]
                        .iter()
                        .all(Option::is_none)
                );
                if columns > 4 {
                    assert!(terminal.cell_links()[columns - 5].is_none());
                }
            }
        }
    }

    #[test]
    fn branch_totals_survive_unicode_truncation_without_linking_comparison_metadata() {
        let mut branch = row(
            "\u{f062c} feature/日本語/長い名前/a-very-long-branch",
            ui::PopupColor::Text,
        );
        branch.truncate = true;
        branch.suffix = vec![
            ui::PopupSpan {
                text: " +1234".to_owned(),
                color: ui::PopupColor::Added,
                bold: false,
            },
            ui::PopupSpan {
                text: " -567".to_owned(),
                color: ui::PopupColor::Deleted,
                bold: false,
            },
        ];
        branch.right = vec![ui::PopupSpan {
            text: "\u{f419} origin/日本語/a-long-comparison-base".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
        }];
        branch.link = Some("https://forge.test/owner/project".to_owned());
        let mut check = row("unit tests", ui::PopupColor::Text);
        check.truncate = true;
        check.right = vec![ui::PopupSpan {
            text: "2:03".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
        }];
        for columns in 0..=100 {
            let lines = wrapped_rows(&[branch.clone(), check.clone()], columns);
            assert_eq!(lines.len(), 2);
            for line in &lines {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&render_line(&line.row, columns));
                assert_eq!(ui::width(&terminal.text()), columns);
                assert_eq!(terminal.cell_links().len(), columns);
                let right = line
                    .row
                    .right
                    .iter()
                    .map(|span| ui::width(&ui::clean(&span.text)))
                    .sum::<usize>()
                    .min(columns);
                assert!(
                    terminal.cell_links()[columns - right..]
                        .iter()
                        .all(Option::is_none)
                );
                assert!(terminal.link.is_none());
            }
            let rendered = visible(&render_line(&lines[0].row, columns));
            assert_eq!(rendered.contains("+1234"), rendered.contains("-567"));
            if columns >= 40 {
                assert!(rendered.contains("+1234 -567"));
                if columns <= 60 {
                    assert!(rendered.contains('…'));
                }
                assert!(rendered.contains('\u{f419}'));
            }
            if columns >= 4 {
                assert!(visible(&render_line(&lines[1].row, columns)).ends_with("2:03"));
            }
        }
    }

    #[test]
    fn wrapped_request_titles_retain_identity_and_target_without_linking_padding() {
        let target = "https://forge.test/owner/project/requests/123";
        let mut request = row("\u{f407}  #123 ", ui::PopupColor::Text);
        request.spans.push(ui::PopupSpan {
            text: "日本語 improve Unicode names and paths".to_owned(),
            color: ui::PopupColor::Text,
            bold: false,
        });
        request.link = Some(target.to_owned());
        for columns in 8..=48 {
            let lines = wrapped_rows(&[request.clone()], columns);
            assert!(lines[0].row.plain().contains("#123"));
            for line in lines {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&render_line(&line.row, columns));
                let cells = terminal.cell_links();
                let body = ui::width(&line.row.plain());
                assert_eq!(cells.len(), columns);
                assert!(cells[..body].iter().all(|link| *link == Some(target)));
                assert!(cells[body..].iter().all(Option::is_none));
            }
        }
    }

    #[test]
    fn right_metadata_sanitizes_user_controls_and_stays_unlinked() {
        let mut check = row("safe check", ui::PopupColor::Text);
        check.truncate = true;
        check.link = Some("https://forge.test/check/1".to_owned());
        check.right = vec![ui::PopupSpan {
            text: "9\u{1b}]8;;https://attacker.test\u{7}s".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
        }];
        for columns in 0..=48 {
            let lines = wrapped_rows(&[check.clone()], columns);
            let mut terminal = TerminalConsumer::default();
            terminal.consume(&render_line(&lines[0].row, columns));
            assert_eq!(terminal.cell_links().len(), columns);
            assert!(
                terminal
                    .cell_links()
                    .iter()
                    .all(|link| link.is_none() || *link == check.link.as_deref())
            );
            assert!(terminal.link.is_none());
        }
    }

    #[test]
    fn linked_wrapped_rows_scope_targets_to_body_cells_only() {
        let target = "https://github.com/owner/repo/pull/12";
        let mut linked = row("日本語 branch and PR context", ui::PopupColor::Text);
        linked.link = Some(target.to_owned());
        linked.spans.push(ui::PopupSpan {
            text: " · checks".to_owned(),
            color: ui::PopupColor::Muted,
            bold: true,
        });
        for columns in 4..=32 {
            let lines = wrapped_rows(&[linked.clone()], columns);
            for line in lines {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&outlined_line(
                    render_line(&line.row, columns),
                    1,
                    5,
                    columns + 2,
                    None,
                ));
                let cells = terminal.cell_links();
                let text_width = ui::width(&line.row.plain());
                assert_eq!(cells.len(), columns + 2);
                assert!(cells[0].is_none());
                assert!(
                    cells[1..=text_width]
                        .iter()
                        .all(|link| *link == Some(target))
                );
                assert!(cells[text_width + 1..].iter().all(Option::is_none));
                assert!(terminal.link.is_none());
            }
        }
    }

    #[test]
    fn unsafe_targets_and_metadata_controls_never_create_links() {
        for target in [
            "http://github.com/owner/repo",
            "https://github.com/owner/repo\u{1b}\\",
            "https://github.com/owner/repo\u{7}",
            "https://user@github.com/owner/repo",
            "https://github.com/has space",
        ] {
            let mut unsafe_row = row(
                "check \u{1b}]8;;https://attacker.test\u{1b}\\raw\u{7}",
                ui::PopupColor::Text,
            );
            unsafe_row.link = Some(target.to_owned());
            for line in wrapped_rows(&[unsafe_row], 12) {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&render_line(&line.row, 12));
                assert!(terminal.cell_links().iter().all(Option::is_none));
                assert!(terminal.link.is_none());
            }
        }
    }

    #[test]
    fn borders_footer_and_unlinked_frames_clear_previous_links() {
        let target = "https://github.com/owner/repo/actions/runs/12";
        let mut check = row("check", ui::PopupColor::Text);
        check.link = Some(target.to_owned());
        let mut terminal = TerminalConsumer {
            link: Some("https://previous.test".to_owned()),
            ..TerminalConsumer::default()
        };
        for (index, text) in [
            render_line(&check, 16),
            render_line(&check, 16),
            render_line(&row("context", ui::PopupColor::Muted), 16),
            ui::fit("↕ Scroll", 16),
            render_line(&check, 16),
        ]
        .into_iter()
        .enumerate()
        {
            terminal.glyphs.clear();
            terminal.consume(&outlined_line(text, index, 5, 18, None));
            let links = terminal.cell_links();
            if index == 1 {
                assert!(links[1..=5].iter().all(|link| *link == Some(target)));
                assert!(links[..1].iter().chain(&links[6..]).all(Option::is_none));
            } else {
                assert!(links.iter().all(Option::is_none));
            }
            assert!(terminal.link.is_none());
        }
        terminal.link = Some(target.to_owned());
        terminal.consume(&outlined_line(String::new(), 0, 0, 0, None));
        assert!(terminal.link.is_none());
    }

    #[test]
    fn terminal_links_follow_scrolled_rows_without_click_expansion() {
        let mut overlay = Overlay {
            granted: true,
            ..Overlay::default()
        };
        overlay.viewport.set(3);
        overlay.columns.set(18);
        let checks: Vec<_> = (0..10)
            .map(|index| {
                let mut check = row(&format!("check {index}"), ui::PopupColor::Text);
                check.link = Some(format!(
                    "https://github.com/owner/repo/actions/runs/{index}"
                ));
                check
            })
            .collect();
        overlay.layout.get_mut().lines = wrapped_rows(&checks, 16);
        overlay.event(Event::Mouse(Mouse::ScrollDown(100)));
        assert_eq!(overlay.layout.borrow().scroll, 7);
        overlay.event(Event::Mouse(Mouse::LeftClick(1, 4)));
        assert_eq!(overlay.layout.borrow().scroll, 7);
        assert_eq!(overlay.layout.borrow().lines.len(), checks.len());
        for offset in 0..3 {
            let layout = overlay.layout.borrow();
            let line = &layout.lines[layout.scroll + offset];
            let mut terminal = TerminalConsumer::default();
            terminal.consume(&outlined_line(
                render_line(&line.row, 16),
                offset + 1,
                6,
                18,
                None,
            ));
            let expected = checks[7 + offset].link.as_deref();
            let links = terminal.cell_links();
            let text_width = ui::width(&line.row.plain());
            assert!(links[1..=text_width].iter().all(|link| *link == expected));
            assert!(links[text_width + 1..].iter().all(Option::is_none));
            assert!(terminal.link.is_none());
        }
        overlay.event(Event::Mouse(Mouse::ScrollUp(100)));
        assert_eq!(overlay.layout.borrow().scroll, 0);
    }

    #[test]
    fn geometry_accounts_for_wrapped_context_and_screen_edges() {
        let content = ui::PopupContent {
            key: "pane:1".to_owned(),
            title: row("Agent", ui::PopupColor::Text),
            subtitle: row("working", ui::PopupColor::Native(shephrd::Status::Working)),
            intro: Vec::new(),
            sections: vec![ui::PopupSection {
                heading: None,
                rows: vec![row(
                    "日本語 project branch context with a long path",
                    ui::PopupColor::Muted,
                )],
            }],
        };
        let sidebar = PaneInfo {
            pane_columns: 20,
            pane_content_y: 2,
            ..PaneInfo::default()
        };
        let tab = TabInfo {
            display_area_columns: 36,
            display_area_rows: 12,
            ..TabInfo::default()
        };
        let placement = geometry(&sidebar, 0, &tab, 8..=9, &content).unwrap();
        let height = wrapped_rows(&content.rows(), 14).len() + 3;
        assert!(height > content.rows().len() + 3);
        let expected = FloatingPaneCoordinates {
            pinned: Some(true),
            borderless: Some(true),
            ..FloatingPaneCoordinates::default()
                .with_x_fixed(20)
                .with_y_fixed(12 - height)
                .with_width_fixed(16)
                .with_height_fixed(height)
        };
        assert_eq!(placement.coordinates, expected);
        assert!(placement.connection.bottom < height);
        let tiny_tab = TabInfo {
            display_area_columns: 31,
            ..tab
        };
        assert!(geometry(&sidebar, 0, &tiny_tab, 0..=1, &content).is_none());
    }

    #[test]
    fn compact_source_bands_join_only_their_actual_rows() {
        let content = ui::PopupContent {
            title: row("Workspace", ui::PopupColor::Text),
            ..ui::PopupContent::default()
        };
        let tab = TabInfo {
            display_area_columns: 100,
            display_area_rows: 20,
            ..TabInfo::default()
        };
        for (content_x, side) in [(0, ui::JoinSide::Right), (85, ui::JoinSide::Left)] {
            let sidebar = PaneInfo {
                pane_content_x: content_x,
                pane_content_y: 1,
                pane_content_columns: 15,
                ..PaneInfo::default()
            };
            for band in [0..=0, 7..=7, 7..=8, 18..=18] {
                let placement = geometry(&sidebar, 15, &tab, band.clone(), &content).unwrap();
                assert_eq!(placement.connection.side, side);
                assert_eq!(
                    placement.connection.bottom - placement.connection.top,
                    band.end() - band.start()
                );
            }
            for (top, bottom) in [(0, 0), (2, 2), (7, 7), (2, 3)] {
                let connection = JoinConnection { side, top, bottom };
                for index in 0..8 {
                    let (left, right) = outline_edges(index, 8, Some(&connection));
                    let edge = if side == ui::JoinSide::Right {
                        left
                    } else {
                        right
                    };
                    if top == bottom && index == top {
                        let expected = if index == 0 || index == 7 {
                            "─"
                        } else if side == ui::JoinSide::Right {
                            "┤"
                        } else {
                            "├"
                        };
                        assert_eq!(edge, expected);
                    } else if index > 0 && index < 7 && !(top..=bottom).contains(&index) {
                        assert_eq!(edge, "│");
                    } else if (top..=bottom).contains(&index) {
                        assert_ne!(edge, "│");
                    }
                    assert_eq!(
                        ui::width(&visible(&outlined_line(
                            ui::fit("context", 10),
                            index,
                            8,
                            12,
                            Some(&connection),
                        ))),
                        12
                    );
                }
            }
        }
    }

    #[test]
    fn joined_outline_opens_only_source_band_and_preserves_every_width() {
        for side in [ui::JoinSide::Left, ui::JoinSide::Right] {
            let connection = JoinConnection {
                side,
                top: 2,
                bottom: 5,
            };
            let edge = |row| {
                let (left, right) = outline_edges(row, 8, Some(&connection));
                if side == ui::JoinSide::Right {
                    left
                } else {
                    right
                }
            };
            assert_eq!(edge(1), "│");
            assert_ne!(edge(2), "│");
            assert_eq!(edge(3), " ");
            assert_eq!(edge(4), " ");
            assert_ne!(edge(5), "│");
            assert_eq!(edge(6), "│");
            for columns in 0_usize..=80 {
                for row in 0..8 {
                    let text = ui::fit("日本語 +1234 -567", columns.saturating_sub(2));
                    assert_eq!(
                        ui::width(&visible(&outlined_line(
                            text,
                            row,
                            8,
                            columns,
                            Some(&connection)
                        ))),
                        columns
                    );
                }
            }
            let aligned = JoinConnection {
                side,
                top: 0,
                bottom: 7,
            };
            let (left, right) = outline_edges(0, 8, Some(&aligned));
            assert_eq!(
                if side == ui::JoinSide::Right {
                    left
                } else {
                    right
                },
                "─"
            );
        }
    }

    #[test]
    fn geometry_joins_content_edge_on_either_side_and_rejects_offscreen_bands() {
        let content = ui::PopupContent {
            key: "pane:1".to_owned(),
            title: row("Agent", ui::PopupColor::Text),
            ..ui::PopupContent::default()
        };
        let tab = TabInfo {
            display_area_columns: 100,
            display_area_rows: 20,
            ..TabInfo::default()
        };
        for (content_x, side) in [(0, ui::JoinSide::Right), (85, ui::JoinSide::Left)] {
            let sidebar = PaneInfo {
                pane_x: content_x,
                pane_columns: 16,
                pane_content_x: content_x,
                pane_content_columns: 15,
                pane_content_y: 0,
                ..PaneInfo::default()
            };
            let placement = geometry(&sidebar, 14, &tab, 16..=19, &content).unwrap();
            assert_eq!(placement.connection.side, side);
            assert_eq!(placement.connection.bottom - placement.connection.top, 3);
            let width = 12;
            let x = if side == ui::JoinSide::Right {
                14
            } else {
                85 - width
            };
            assert_eq!(
                placement.coordinates,
                FloatingPaneCoordinates {
                    pinned: Some(true),
                    borderless: Some(true),
                    ..FloatingPaneCoordinates::default()
                        .with_x_fixed(x)
                        .with_y_fixed(15)
                        .with_width_fixed(width)
                        .with_height_fixed(5)
                }
            );
            let clipped = geometry(&sidebar, 16, &tab, 16..=19, &content).unwrap();
            let x = if side == ui::JoinSide::Right {
                15
            } else {
                85 - width
            };
            assert_eq!(
                clipped.coordinates,
                FloatingPaneCoordinates {
                    pinned: Some(true),
                    borderless: Some(true),
                    ..FloatingPaneCoordinates::default()
                        .with_x_fixed(x)
                        .with_y_fixed(15)
                        .with_width_fixed(width)
                        .with_height_fixed(5)
                }
            );
            assert!(geometry(&sidebar, 14, &tab, 20..=22, &content).is_none());
            assert!(geometry(&sidebar, 14, &tab, 19..=22, &content).is_some());
        }
    }

    #[test]
    fn popup_wheel_reaches_all_checks_without_focus_commands() {
        let mut overlay = Overlay {
            granted: true,
            ..Overlay::default()
        };
        overlay.viewport.set(4);
        overlay.layout.get_mut().lines = (0..20)
            .map(|index| PopupLine {
                row: row(&format!("check {index}"), ui::PopupColor::Text),
            })
            .collect();
        assert!(overlay.event(Event::Mouse(Mouse::ScrollDown(100))));
        assert_eq!(overlay.layout.borrow().scroll, 16);
        assert!(overlay.event(Event::Mouse(Mouse::ScrollUp(3))));
        assert_eq!(overlay.layout.borrow().scroll, 13);
        assert!(overlay.event(Event::Mouse(Mouse::ScrollUp(100))));
        assert_eq!(overlay.layout.borrow().scroll, 0);
    }
}
