use crate::{animation::WorkingClock, ui};
use bkpr::ci_contract::CiRefreshTarget;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Instant;
use zellij_tile::prelude::*;

pub const UPDATE: &str = "bkpr-tooltip";
pub const READY: &str = "bkpr-tooltip-ready";
pub const POINTER: &str = "bkpr-tooltip-pointer";
pub const POINTER_QUERY: &str = "bkpr-tooltip-pointer-query";
pub const REFRESH: &str = "bkpr-tooltip-refresh";
pub const ERROR: &str = "bkpr-tooltip-error";
const PRESENTED: &str = "bkpr-tooltip-presented";
const CLEARED: &str = "bkpr-tooltip-cleared";

pub const GUIDE_UPDATE: &str = "bkpr-guide";
pub const GUIDE_READY: &str = "bkpr-guide-ready";
const MAX_GUIDE_PAYLOAD: usize = 128 * 1024;
const MAX_GUIDE_ROWS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuideReady {
    pub id: u32,
    pub instance: String,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct GuideFrame {
    instance: String,
    generation: u64,
    revision: u64,
    visible: bool,
    closed: bool,
    lines: Vec<String>,
    coordinates: FloatingPaneCoordinates,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ready {
    pub id: u32,
    pub client_id: ClientId,
    pub instance: String,
    pub key: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupIdentity {
    pub client_id: ClientId,
    pub instance: String,
    pub key: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerNotice {
    pub client_id: ClientId,
    pub instance: String,
    pub key: String,
    pub revision: u64,
    pub inside: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultNotice {
    pub id: u32,
    pub client_id: ClientId,
    pub instance: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub client_id: ClientId,
    pub instance: String,
    pub revision: u64,
    pub key: String,
    pub target: CiRefreshTarget,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Tooltip {
    pub client_id: ClientId,
    pub instance: String,
    pub revision: u64,
    pub visible: bool,
    pub content: ui::PopupContent,
    pub coordinates: FloatingPaneCoordinates,
}

impl Tooltip {
    pub fn identity(&self) -> PopupIdentity {
        PopupIdentity {
            client_id: self.client_id,
            instance: self.instance.clone(),
            key: self.content.key.clone(),
            revision: self.revision,
        }
    }
}

pub struct Placement {
    pub coordinates: FloatingPaneCoordinates,
}

pub struct Controller {
    pub id: Option<u32>,
    pub client_id: ClientId,
    pub pending: Option<Tooltip>,
    revision: u64,
    launched: bool,
    close_pending: bool,
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
            client_id: 0,
            pending: None,
            revision: 0,
            launched: false,
            close_pending: false,
            instance,
            refresh_requested: Vec::new(),
        }
    }
}

impl Controller {
    pub fn update(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        tooltip: Option<(&ui::PopupContent, Placement)>,
    ) {
        let empty = ui::PopupContent::default();
        let (visible, content, coordinates) = match tooltip {
            Some((content, placement)) => (true, content, placement.coordinates),
            None => (false, &empty, FloatingPaneCoordinates::default()),
        };
        if visible {
            self.close_pending = false;
        }
        if self.pending.as_ref().is_some_and(|old| {
            old.visible == visible
                && old.client_id == self.client_id
                && &old.content == content
                && old.coordinates == coordinates
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
            client_id: self.client_id,
            instance: self.instance.clone(),
            revision: self.revision,
            visible,
            content: content.clone(),
            coordinates,
        });
        if self.id.is_some() || visible {
            self.send(owner, configuration);
        }
    }

    pub fn accept_refresh(&mut self, source: u32, request: &RefreshRequest) -> bool {
        if self.id != Some(source)
            || request.client_id != self.client_id
            || self.refresh_requested.contains(&request.target)
            || !self.pending.as_ref().is_some_and(|state| {
                state.visible
                    && state.client_id == request.client_id
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

    pub fn accept_pointer(&self, source: u32, notice: &PointerNotice) -> bool {
        self.id == Some(source)
            && notice.client_id == self.client_id
            && self.pending.as_ref().is_some_and(|state| {
                state.visible
                    && state.client_id == notice.client_id
                    && state.instance == notice.instance
                    && state.content.key == notice.key
                    && state.revision == notice.revision
            })
    }

    pub fn probe_pointer(&self, _owner: u32) -> bool {
        let (Some(id), Some(state)) = (self.id, self.pending.as_ref()) else {
            return false;
        };
        if !state.visible || state.client_id != self.client_id {
            return false;
        }
        let Ok(payload) = serde_json::to_string(&state.identity()) else {
            return false;
        };
        if payload.len() > 4096 {
            return false;
        }
        pipe_message_to_plugin(
            MessageToPlugin::new(POINTER_QUERY)
                .with_destination_plugin_id(id)
                .with_payload(payload),
        );
        true
    }

    pub fn ready(
        &mut self,
        ready: &Ready,
        owner: u32,
        configuration: &BTreeMap<String, String>,
    ) -> bool {
        if ready.client_id != self.client_id
            || ready.instance != self.instance
            || ready.revision > self.revision
            || self.id.is_some_and(|id| id != ready.id)
            || self.pending.is_none() && !self.close_pending
        {
            return false;
        }
        let first = self.id.is_none();
        self.id = Some(ready.id);
        self.launched = true;
        if self.close_pending {
            self.close_helper(ready.id);
            return false;
        }
        let current = self.pending.as_ref().is_some_and(|state| {
            state.client_id == ready.client_id
                && state.instance == ready.instance
                && state.content.key == ready.key
                && state.revision == ready.revision
        });
        if first && !current {
            self.send(owner, configuration);
        }
        current && self.pending.as_ref().is_some_and(|state| state.visible)
    }

    pub fn accept_fault(&mut self, source: u32, fault: &FaultNotice) -> bool {
        if source != fault.id
            || self.id.is_some_and(|id| id != source)
            || fault.client_id != self.client_id
            || fault.instance != self.instance
            || fault.message.is_empty()
            || fault.message.len() > 1024
            || self.pending.is_none() && !self.close_pending
        {
            return false;
        }
        self.id = Some(source);
        self.launched = true;
        if self.close_pending {
            self.close_helper(source);
            return false;
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
            config.insert("owner_client_id".to_owned(), self.client_id.to_string());
            config.insert("popup_instance".to_owned(), self.instance.clone());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("bkpr details".to_owned()),
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
            self.close_pending = false;
            self.pending = None;
        }
    }

    pub fn close(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        if let Some(id) = self.id {
            self.close_helper(id);
        } else if self.launched {
            self.close_pending = true;
            // Even a cold load acknowledges its hidden clear-frame, then gets retired.
            self.update(owner, configuration, None);
        }
    }

    fn close_helper(&mut self, id: u32) {
        close_plugin_pane(id);
        self.id = None;
        self.pending = None;
        self.launched = false;
        self.close_pending = false;
        self.refresh_requested.clear();
    }
}

pub struct GuideController {
    pub id: Option<u32>,
    instance: String,
    generation: u64,
    revision: u64,
    visible_revision: u64,
    ready_revision: u64,
    launched: bool,
    pending: Option<GuideFrame>,
}

impl Default for GuideController {
    fn default() -> Self {
        Self {
            id: None,
            instance: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_nanos().to_string())
                .unwrap_or_else(|_| "clock-unavailable".to_owned()),
            generation: 1,
            revision: 0,
            visible_revision: 0,
            ready_revision: 0,
            launched: false,
            pending: None,
        }
    }
}

impl GuideController {
    pub fn update(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        frame: Option<(&[String], FloatingPaneCoordinates)>,
    ) {
        let frame = frame.filter(|(lines, _)| !lines.is_empty() && lines.len() <= MAX_GUIDE_ROWS);
        let (visible, lines, mut coordinates) = match frame {
            Some((lines, coordinates)) => (true, lines, coordinates),
            None => (
                false,
                &[][..],
                self.pending
                    .as_ref()
                    .map(|state| state.coordinates.clone())
                    .unwrap_or_default(),
            ),
        };
        coordinates.pinned = Some(true);
        coordinates.borderless = Some(true);
        if self.pending.as_ref().is_some_and(|old| {
            old.visible == visible && old.lines == lines && old.coordinates == coordinates
        }) {
            return;
        }
        self.revision = self.revision.saturating_add(1);
        if visible && !self.pending.as_ref().is_some_and(|state| state.visible) {
            self.visible_revision = self.revision;
        }
        self.pending = Some(GuideFrame {
            instance: self.instance.clone(),
            generation: self.generation,
            revision: self.revision,
            visible,
            closed: false,
            lines: lines.to_vec(),
            coordinates,
        });
        if self.launched || visible {
            self.send(owner, configuration);
        }
    }

    pub fn ready(
        &mut self,
        ready: &GuideReady,
        owner: u32,
        configuration: &BTreeMap<String, String>,
    ) -> bool {
        if !self.launched
            || self.id.is_some_and(|id| id != ready.id)
            || !self.pending.as_ref().is_some_and(|state| {
                ready.instance == state.instance
                    && ready.generation == state.generation
                    && ready.revision > 0
                    && ready.revision <= state.revision
            })
        {
            return false;
        }
        let first = self.id.is_none();
        self.id = Some(ready.id);
        self.ready_revision = self.ready_revision.max(ready.revision);
        if first {
            self.send(owner, configuration);
        }
        true
    }

    pub fn ready_for_display(&self) -> bool {
        self.id.is_some()
            && self.ready_revision >= self.visible_revision
            && self.pending.as_ref().is_some_and(|state| state.visible)
    }

    fn send(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        let Some(frame) = &mut self.pending else {
            return;
        };
        let Ok(mut payload) = serde_json::to_string(frame) else {
            return;
        };
        if payload.len() > MAX_GUIDE_PAYLOAD {
            // Keep the inline fallback rather than exposing a partially delivered guide.
            frame.visible = false;
            frame.lines.clear();
            let Ok(hidden) = serde_json::to_string(frame) else {
                return;
            };
            payload = hidden;
            if !self.launched {
                return;
            }
        }
        let mut message = MessageToPlugin::new(GUIDE_UPDATE).with_payload(payload);
        if let Some(id) = self.id {
            message.destination_plugin_id = Some(id);
        } else {
            let Some(url) = configuration.get("plugin_url") else {
                return;
            };
            let mut config = configuration.clone();
            config.insert("role".to_owned(), "guide-overlay".to_owned());
            config.insert("owner".to_owned(), owner.to_string());
            config.insert("guide_instance".to_owned(), self.instance.clone());
            config.insert("guide_generation".to_owned(), self.generation.to_string());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("bkpr shortcuts".to_owned()),
                ..NewPluginArgs::default()
            });
            message.floating_pane_coordinates = Some(frame.coordinates.clone());
        }
        // A stable URL/config identity also coalesces updates while the first load is pending.
        self.launched = true;
        pipe_message_to_plugin(message);
    }

    fn reset(&mut self) {
        self.id = None;
        self.launched = false;
        self.pending = None;
        self.ready_revision = 0;
        self.visible_revision = 0;
        self.generation = self.generation.saturating_add(1);
    }

    pub fn forget(&mut self, id: u32) {
        if self.id == Some(id) {
            self.reset();
        }
    }

    pub fn close(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        if let Some(id) = self.id {
            close_plugin_pane(id);
        } else if self.launched {
            // Retire an in-flight instance too; a later launch has a different config identity.
            if let Some(frame) = &mut self.pending {
                self.revision = self.revision.saturating_add(1);
                frame.revision = self.revision;
                frame.visible = false;
                frame.closed = true;
                frame.lines.clear();
            }
            self.send(owner, configuration);
        }
        self.reset();
    }
}

pub fn geometry(
    sidebar: &PaneInfo,
    rendered_columns: usize,
    tab: &TabInfo,
    band: std::ops::RangeInclusive<usize>,
    content: &ui::PopupContent,
) -> Option<Placement> {
    // The outline may occupy the separator, but never a painted sidebar item cell.
    let left = if sidebar.pane_content_columns > 0 {
        sidebar.pane_content_x
    } else {
        sidebar.pane_x
    };
    if left >= tab.display_area_columns {
        return None;
    }
    let columns = match (rendered_columns, sidebar.pane_content_columns) {
        (0, reported) => reported,
        (rendered, 0) => rendered,
        (rendered, reported) => rendered.min(reported),
    };
    let painted_right = if columns > 0 {
        left.saturating_add(columns)
    } else {
        sidebar.pane_x.saturating_add(sidebar.pane_columns)
    };
    let (left, right) = if sidebar.pane_columns > 0 {
        (
            left.min(sidebar.pane_x.saturating_add(1)),
            painted_right.max(
                sidebar
                    .pane_x
                    .saturating_add(sidebar.pane_columns)
                    .saturating_sub(1),
            ),
        )
    } else {
        (left, painted_right)
    };
    let right_space = tab.display_area_columns.saturating_sub(right);
    let left_space = left;
    let (x, available, on_right) = if right_space >= 12 {
        (right, right_space, true)
    } else if left_space >= 12 {
        (0, left_space, false)
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
            let body: usize = row
                .spans
                .iter()
                .map(|span| ui::width(&ui::clean(&span.text)))
                .sum();
            let suffix: usize = row
                .suffix
                .iter()
                .map(|span| ui::width(&ui::clean(&span.text)))
                .sum();
            let right: usize = row
                .right
                .iter()
                .map(|span| ui::width(&ui::clean(&span.text)))
                .sum();
            // Rendering reserves a gap before right-aligned metadata and, when a
            // suffix is present, limits that metadata to half the body viewport.
            let body = body.max(usize::from(suffix != 0)).saturating_add(suffix);
            let width = body
                .saturating_add(right)
                .saturating_add(usize::from(right != 0));
            if suffix != 0 {
                width.max(right.saturating_mul(2))
            } else {
                width
            }
        })
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .clamp(48, 80)
        .min(available);
    let height = wrapped_rows(&source_rows, width.saturating_sub(2))
        .len()
        .saturating_add(3)
        .max(source_bottom - source_top + 1)
        .min(tab.display_area_rows)
        .max(1);
    let x = if on_right {
        x
    } else {
        left_space.saturating_sub(width)
    };
    let y = source_top.min(tab.display_area_rows.saturating_sub(height));
    Some(Placement {
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

pub(crate) struct PopupLine {
    pub(crate) row: ui::PopupRow,
}

pub(crate) fn wrapped_rows(rows: &[ui::PopupRow], columns: usize) -> Vec<PopupLine> {
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
                        working_clock: span.working_clock,
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
                            working_clock: span.working_clock,
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
                working_clock: span.working_clock,
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
            working_clock: false,
        });
    }
    output.push(PopupLine { row: line });
}

pub(crate) const OSC8_CLOSE: &str = "\u{1b}]8;;\u{1b}\\";

pub(crate) fn working_clock_visible(row: &ui::PopupRow, columns: usize) -> bool {
    let right: usize = row.right.iter().map(|span| ui::width(&span.text)).sum();
    let right_columns = if right > columns
        && row
            .right
            .iter()
            .any(|span| span.text == ui::refresh_button_text())
    {
        0
    } else {
        right.min(if row.suffix.is_empty() {
            columns
        } else {
            columns / 2
        })
    };
    let body = columns
        .saturating_sub(right_columns + usize::from(right_columns != 0 && right_columns < columns));
    let suffix: usize = row.suffix.iter().map(|span| ui::width(&span.text)).sum();
    let mut remaining = body.saturating_sub(if suffix < body { suffix } else { 0 });
    for span in &row.spans {
        let width = ui::width(&span.text).min(remaining);
        if span.working_clock && width > 0 {
            return true;
        }
        remaining = remaining.saturating_sub(width);
    }
    false
}

#[cfg(test)]
fn render_line(row: &ui::PopupRow, columns: usize) -> String {
    render_action_line(row, columns, false, &ui::Theme::default(), 0)
}

pub(crate) fn render_action_line(
    row: &ui::PopupRow,
    columns: usize,
    hovered: bool,
    theme: &ui::Theme,
    clock_frame: usize,
) -> String {
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
    rendered.push_str(&theme.base);
    if let Some(link) = row.link.as_deref().and_then(ui::safe_link) {
        rendered.push_str("\u{1b}]8;;");
        rendered.push_str(link);
        rendered.push_str("\u{1b}\\");
    }
    let mut used = 0;
    for span in &row.spans {
        let available = body_columns.saturating_sub(suffix_width + used);
        let text = if span.working_clock {
            ui::truncate(
                ui::working_clock_icon(clock_frame),
                available.min(ui::width(&span.text)),
            )
        } else {
            ui::truncate(&span.text, available)
        };
        used += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        rendered.push_str(span.color.ansi(theme));
        if span.bold {
            rendered.push_str("\u{1b}[1m");
        }
        rendered.push_str(&text);
    }
    for span in suffix {
        let text = ui::clean(&span.text);
        used += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        rendered.push_str(span.color.ansi(theme));
        rendered.push_str(&text);
    }
    rendered.push_str(OSC8_CLOSE);
    rendered.push_str(&theme.muted);
    rendered.push_str(&" ".repeat(columns.saturating_sub(used + right_columns)));
    let mut used_right = 0;
    for span in &row.right {
        if hovered && row.refresh.is_some() {
            rendered.push_str(&theme.base);
            rendered.push_str(&theme.hovered);
        }
        let text = ui::truncate(&span.text, right_columns.saturating_sub(used_right));
        used_right += ui::width(&text);
        rendered.push_str("\u{1b}[22m");
        if !(hovered && row.refresh.is_some()) {
            rendered.push_str(span.color.ansi(theme));
        }
        if span.bold || (hovered && row.refresh.is_some()) {
            rendered.push_str("\u{1b}[1m");
        }
        rendered.push_str(&text);
    }
    rendered.push_str(ui::RESET);
    rendered.push_str(&theme.base);
    rendered
}

fn outline_edges(row: usize, rows: usize) -> (&'static str, &'static str) {
    if row == 0 {
        ("╭", "╮")
    } else if row + 1 == rows {
        ("╰", "╯")
    } else {
        ("│", "│")
    }
}

pub(crate) fn outlined_line(
    text: String,
    row: usize,
    rows: usize,
    columns: usize,
    theme: &ui::Theme,
) -> String {
    let (left, right) = outline_edges(row, rows);
    let center = if row == 0 || row + 1 == rows {
        format!("{}{}", theme.bright, "─".repeat(columns.saturating_sub(2)))
    } else {
        text
    };
    if columns >= 2 {
        format!(
            "{OSC8_CLOSE}{}{}{left}{center}{OSC8_CLOSE}{}{}{right}{OSC8_CLOSE}",
            theme.base, theme.bright, theme.base, theme.bright
        )
    } else if columns == 1 {
        format!(
            "{OSC8_CLOSE}{}{}{left}{OSC8_CLOSE}",
            theme.base, theme.bright
        )
    } else {
        OSC8_CLOSE.to_owned()
    }
}

fn fixed_rectangle(coordinates: &FloatingPaneCoordinates) -> Option<(usize, usize, usize, usize)> {
    // Controller placements are fixed; never guess positions for unresolved percent coordinates.
    let (x, y, columns, rows) = (
        coordinates.x?.to_fixed(0),
        coordinates.y?.to_fixed(0),
        coordinates.width?.to_fixed(0),
        coordinates.height?.to_fixed(0),
    );
    let fixed = FloatingPaneCoordinates::default()
        .with_x_fixed(x)
        .with_y_fixed(y)
        .with_width_fixed(columns)
        .with_height_fixed(rows);
    (coordinates.x == fixed.x
        && coordinates.y == fixed.y
        && coordinates.width == fixed.width
        && coordinates.height == fixed.height)
        .then_some((x, y, columns, rows))
}

#[derive(Default)]
pub struct Overlay {
    theme: ui::Theme,
    working_clock: RefCell<WorkingClock>,
    pub owner: Option<u32>,
    pub id: u32,
    client_id: ClientId,
    foreign_client: bool,
    instance: String,
    shown: Option<bool>,
    coordinates: Option<FloatingPaneCoordinates>,
    clear_required: bool,
    cleared_revision: Cell<Option<u64>>,
    rendered_revision: Cell<Option<u64>>,
    presented_revision: Option<u64>,
    granted: bool,
    owner_seen: bool,
    refresh_hover: Option<CiRefreshTarget>,
    state: Option<Tooltip>,
    fault: Option<String>,
    fault_reported: bool,
    layout: RefCell<PopupLayout>,
    viewport: Cell<usize>,
    columns: Cell<usize>,
    rows: Cell<usize>,
    pointer_inside: Cell<bool>,
    pointer_position: Cell<Option<(isize, usize)>>,
    refresh_requested: Vec<CiRefreshTarget>,
}

impl Overlay {
    pub fn set_terminal_colors(&mut self, colors: &crate::palette::TerminalColors) -> bool {
        self.theme.update(colors)
    }

    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32, client_id: ClientId) {
        self.id = id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        self.client_id = client_id;
        self.instance = configuration
            .get("popup_instance")
            .cloned()
            .unwrap_or_default();
        self.foreign_client = configuration
            .get("owner_client_id")
            .and_then(|owner| owner.parse::<ClientId>().ok())
            != Some(client_id);
        // A plugin pane has replicas on other clients; those must not touch its shared pane.
        if self.foreign_client {
            return;
        }
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::CustomMessage,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::BeforeClose,
            EventType::Mouse,
            EventType::Timer,
        ]);
        request_permission(super::PERMISSIONS);
    }

    pub fn event(&mut self, event: Event) -> bool {
        if self.foreign_client {
            return false;
        }
        match event {
            Event::Timer(_) => {
                return self
                    .working_clock
                    .borrow_mut()
                    .tick(Instant::now(), self.working_clock_active());
            }
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                set_selectable(false);
                if self
                    .owner
                    .is_some_and(|owner| get_pane_info(PaneId::Plugin(owner)).is_some())
                {
                    self.owner_seen = true;
                    self.apply();
                } else {
                    close_self();
                }
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.granted = false;
                self.working_clock
                    .borrow_mut()
                    .schedule(Instant::now(), false);
                self.presented_revision = None;
                self.rendered_revision.set(None);
                self.pointer_inside.set(false);
                self.pointer_position.set(None);
                self.record_fault("Tooltip permissions denied");
            }
            Event::PaneUpdate(manifest) => {
                let found = manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| pane.is_plugin && Some(pane.id) == self.owner);
                if found {
                    self.owner_seen = true;
                } else if self.owner_seen {
                    close_self();
                }
                return false;
            }
            Event::Mouse(Mouse::Hover(line, column)) if self.granted => {
                if let Some(notice) = self.pointer_hover(line, column) {
                    self.notify_pointer(&notice);
                }
                let hover = self.refresh_at(line, column).map(|request| request.target);
                let previous = self
                    .refresh_hover
                    .as_ref()
                    .filter(|target| !self.refresh_requested.contains(target));
                let changed = previous != hover.as_ref();
                self.refresh_hover = hover;
                return changed;
            }
            Event::Mouse(Mouse::LeftClick(line, column)) if self.granted => {
                if let (Some(owner), Some(request)) = (self.owner, self.refresh_at(line, column))
                    && let Ok(payload) = serde_json::to_string(&request)
                {
                    let changed = self.refresh_hover.as_ref() == Some(&request.target);
                    self.refresh_requested.push(request.target);
                    pipe_message_to_plugin(
                        MessageToPlugin::new(REFRESH)
                            .with_destination_plugin_id(owner)
                            .with_payload(payload),
                    );
                    return changed;
                }
                return false;
            }
            Event::Mouse(Mouse::ScrollUp(lines)) if self.granted => {
                let hover_changed = self
                    .refresh_hover
                    .take()
                    .is_some_and(|target| !self.refresh_requested.contains(&target));
                let mut layout = self.layout.borrow_mut();
                let scroll = layout.scroll.saturating_sub(lines);
                let changed = hover_changed || scroll != layout.scroll;
                layout.scroll = scroll;
                return changed && self.state.as_ref().is_some_and(|state| state.visible);
            }
            Event::Mouse(Mouse::ScrollDown(lines)) if self.granted => {
                let hover_changed = self
                    .refresh_hover
                    .take()
                    .is_some_and(|target| !self.refresh_requested.contains(&target));
                let mut layout = self.layout.borrow_mut();
                let scroll = layout
                    .scroll
                    .saturating_add(lines)
                    .min(layout.lines.len().saturating_sub(self.viewport.get()));
                let changed = hover_changed || scroll != layout.scroll;
                layout.scroll = scroll;
                return changed && self.state.as_ref().is_some_and(|state| state.visible);
            }
            Event::PaneClosed(PaneId::Plugin(id)) if Some(id) == self.owner => {
                close_self();
                return false;
            }
            _ => return false,
        }
        true
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if self.foreign_client {
            return false;
        }
        let PipeSource::Plugin(source) = message.source else {
            return false;
        };
        let Some(payload) = message.payload.filter(|payload| payload.len() <= 32 * 1024) else {
            return false;
        };
        if message.name == CLEARED {
            let Ok(identity) = serde_json::from_str::<PopupIdentity>(&payload) else {
                return false;
            };
            if !self.accept_cleared(source, &identity) {
                return false;
            }
            let visible = self.state.as_ref().is_some_and(|state| state.visible);
            self.apply();
            if !visible {
                self.announce();
            }
            return visible;
        }
        if message.name == PRESENTED {
            let Ok(identity) = serde_json::from_str::<PopupIdentity>(&payload) else {
                return false;
            };
            if self.accept_presented(source, &identity) {
                self.announce();
                if let Some(notice) = self.pointer_notice() {
                    self.notify_pointer(&notice);
                }
            }
            return false;
        }
        if Some(source) != self.owner {
            return false;
        }
        match message.name.as_str() {
            POINTER_QUERY if payload.len() <= 4096 => {
                let Ok(identity) = serde_json::from_str::<PopupIdentity>(&payload) else {
                    return false;
                };
                let Some(notice) = self.pointer_query(source, &identity) else {
                    return false;
                };
                self.notify_pointer(&notice);
                false
            }
            UPDATE => {
                let Ok(tooltip) = serde_json::from_str::<Tooltip>(&payload) else {
                    // An undecodable frame cannot prove its client/launch scope.
                    return false;
                };
                if tooltip.client_id != self.client_id || tooltip.instance != self.instance {
                    return false;
                }
                if self
                    .state
                    .as_ref()
                    .is_some_and(|current| tooltip.revision <= current.revision)
                {
                    return false;
                }
                if tooltip.revision == 0
                    || tooltip.visible
                        && (tooltip.content.key.is_empty()
                            || !fixed_rectangle(&tooltip.coordinates)
                                .is_some_and(|(_, _, columns, rows)| columns > 0 && rows > 0))
                {
                    self.record_fault("Invalid tooltip frame geometry or content identity");
                    return false;
                }
                if !self.accept(source, tooltip) {
                    return false;
                }
                self.apply();
                true
            }
            _ => false,
        }
    }

    fn accept(&mut self, source: u32, tooltip: Tooltip) -> bool {
        if Some(source) != self.owner
            || self.foreign_client
            || tooltip.client_id != self.client_id
            || tooltip.instance != self.instance
            || tooltip.instance.is_empty()
            || tooltip.revision == 0
            || tooltip.visible && tooltip.content.key.is_empty()
            || tooltip.visible
                && !fixed_rectangle(&tooltip.coordinates)
                    .is_some_and(|(_, _, columns, rows)| columns > 0 && rows > 0)
            || self.state.as_ref().is_some_and(|current| {
                tooltip.instance == current.instance && tooltip.revision <= current.revision
            })
        {
            return false;
        }
        let same_item = self.state.as_ref().is_some_and(|current| {
            current.visible
                && tooltip.visible
                && current.instance == tooltip.instance
                && current.content.key == tooltip.content.key
        });
        let moved = self
            .state
            .as_ref()
            .is_none_or(|current| current.coordinates != tooltip.coordinates);
        if !tooltip.visible || moved && self.state.is_some() || self.shown == Some(false) {
            self.clear_required = true;
        }
        self.cleared_revision.set(None);
        if !same_item {
            self.pointer_position.set(None);
            self.pointer_inside.set(false);
        } else if moved {
            let position = self.reproject_pointer(&tooltip);
            self.pointer_position.set(position);
            self.pointer_inside.set(position.is_some());
            if let Some((_, _, columns, rows)) = fixed_rectangle(&tooltip.coordinates) {
                self.columns.set(columns);
                self.rows.set(rows);
            }
        }
        if !same_item || moved {
            self.refresh_hover = None;
        }
        let layout = self.layout.get_mut();
        let source_rows = tooltip.content.rows();
        self.refresh_requested.retain(|target| {
            tooltip.visible
                && source_rows
                    .iter()
                    .any(|row| row.refresh.as_ref() == Some(target))
        });
        if self.state.as_ref().map(|state| &state.content.key) != Some(&tooltip.content.key)
            || !tooltip.visible
        {
            layout.scroll = 0;
        }
        if tooltip.visible {
            let (_, _, columns, rows) = fixed_rectangle(&tooltip.coordinates).unwrap();
            layout.columns = columns.saturating_sub(2);
            layout.lines = wrapped_rows(&source_rows, layout.columns);
            layout.scroll = layout
                .scroll
                .min(layout.lines.len().saturating_sub(rows.saturating_sub(3)));
        } else {
            layout.lines.clear();
            layout.columns = 0;
        }
        self.rendered_revision.set(None);
        self.presented_revision = None;
        self.state = Some(tooltip);
        if !self.working_clock_active() {
            self.working_clock
                .borrow_mut()
                .schedule(Instant::now(), false);
        }
        true
    }

    fn reproject_pointer(&self, next: &Tooltip) -> Option<(isize, usize)> {
        if !self.pointer_inside.get() {
            return None;
        }
        let current = self.state.as_ref()?;
        Self::project_position(
            self.pointer_position.get()?,
            &current.coordinates,
            &next.coordinates,
        )
    }

    fn project_position(
        position: (isize, usize),
        previous: &FloatingPaneCoordinates,
        next: &FloatingPaneCoordinates,
    ) -> Option<(isize, usize)> {
        let (old_x, old_y, old_columns, old_rows) = fixed_rectangle(previous)?;
        let (new_x, new_y, columns, rows) = fixed_rectangle(next)?;
        let (line, column) = position;
        let line = usize::try_from(line).ok()?;
        if line >= old_rows || column >= old_columns || column == u16::MAX as usize {
            return None;
        }
        let column = old_x.checked_add(column)?.checked_sub(new_x)?;
        let line = old_y.checked_add(line)?.checked_sub(new_y)?;
        if line >= rows || column >= columns {
            return None;
        }
        Some((isize::try_from(line).ok()?, column))
    }

    fn pointer_notice(&self) -> Option<PointerNotice> {
        let state = self.state.as_ref().filter(|state| {
            state.visible && self.granted && self.presented_revision == Some(state.revision)
        })?;
        Some(PointerNotice {
            client_id: self.client_id,
            instance: state.instance.clone(),
            key: state.content.key.clone(),
            revision: state.revision,
            inside: self.pointer_inside.get(),
        })
    }

    fn pointer_query(&self, source: u32, identity: &PopupIdentity) -> Option<PointerNotice> {
        let state = self.state.as_ref()?;
        if Some(source) != self.owner
            || self.foreign_client
            || identity.client_id != self.client_id
            || !state.visible
            || state.instance != identity.instance
            || state.content.key != identity.key
            || state.revision != identity.revision
        {
            return None;
        }
        self.pointer_notice()
    }

    fn pointer_hover(&self, line: isize, column: usize) -> Option<PointerNotice> {
        let position = match (&self.coordinates, &self.state) {
            (Some(actual), Some(desired)) if line >= 0 && column != u16::MAX as usize => {
                Self::project_position((line, column), actual, &desired.coordinates)
            }
            _ => Some((line, column)),
        };
        self.pointer_position.set(position);
        self.reconcile_pointer()
    }

    fn reconcile_pointer(&self) -> Option<PointerNotice> {
        let inside = self.granted
            && self.state.as_ref().is_some_and(|state| state.visible)
            && self.pointer_position.get().is_some_and(|(line, column)| {
                line >= 0
                    && (line as usize) < self.rows.get()
                    && column < self.columns.get()
                    && column != u16::MAX as usize
            });
        if self.pointer_inside.replace(inside) == inside {
            return None;
        }
        self.pointer_notice()
    }

    fn notify_pointer(&self, notice: &PointerNotice) {
        let Some(owner) = self.owner else { return };
        if let Ok(payload) = serde_json::to_string(notice)
            && payload.len() <= 4096
        {
            pipe_message_to_plugin(
                MessageToPlugin::new(POINTER)
                    .with_destination_plugin_id(owner)
                    .with_payload(payload),
            );
        }
    }

    fn refresh_at(&self, line: isize, column: usize) -> Option<RefreshRequest> {
        let state = self
            .state
            .as_ref()
            .filter(|state| state.visible && self.presented_revision == Some(state.revision))?;
        if line < 1 || line as usize > self.viewport.get() {
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
            client_id: self.client_id,
            instance: state.instance.clone(),
            revision: state.revision,
            key: state.content.key.clone(),
            target: target.clone(),
        })
    }

    fn apply(&mut self) {
        if !self.granted {
            return;
        }
        self.report_fault();
        let pane = PaneId::Plugin(self.id);
        match &self.state {
            Some(state) if state.visible && !self.clear_required => {
                let restored = self.shown != Some(true);
                if restored {
                    // Unsuppress first: it restores native defaults, including old geometry.
                    show_pane_with_id(pane, true, false);
                    self.shown = Some(true);
                }
                if restored || self.coordinates.as_ref() != Some(&state.coordinates) {
                    change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
                    self.coordinates = Some(state.coordinates.clone());
                }
            }
            Some(state) if state.visible && self.shown.is_some() => {}
            _ if self.shown != Some(false) => {
                hide_self();
                self.shown = Some(false);
                self.coordinates = None;
            }
            _ => {}
        }
    }

    fn complete_dimensions(&self, rows: usize, columns: usize) -> bool {
        !self.foreign_client
            && self.granted
            && !self.clear_required
            && self.shown == Some(true)
            && self.state.as_ref().is_some_and(|state| {
                state.visible
                    && self.coordinates.as_ref() == Some(&state.coordinates)
                    && fixed_rectangle(&state.coordinates).is_some_and(|(_, _, width, height)| {
                        width == columns && height == rows && width > 0 && height > 0
                    })
            })
    }

    fn accept_presented(&mut self, source: u32, identity: &PopupIdentity) -> bool {
        if source != self.id
            || self.foreign_client
            || !self.granted
            || self.clear_required
            || self.shown.is_none()
            || self.presented_revision == Some(identity.revision)
            || self.rendered_revision.get() != Some(identity.revision)
            || !self.state.as_ref().is_some_and(|state| {
                self.shown == Some(state.visible)
                    && state.client_id == identity.client_id
                    && state.instance == identity.instance
                    && state.content.key == identity.key
                    && state.revision == identity.revision
            })
        {
            return false;
        }
        self.presented_revision = Some(identity.revision);
        true
    }

    fn stage_render(&self, rows: usize, columns: usize) -> bool {
        let Some(state) = self.state.as_ref() else {
            return false;
        };
        if self.foreign_client
            || !self.granted
            || self.clear_required
            || if state.visible {
                !self.complete_dimensions(rows, columns)
            } else {
                self.shown != Some(false)
            }
        {
            return false;
        }
        self.rendered_revision.set(Some(state.revision));
        true
    }

    fn accept_cleared(&mut self, source: u32, identity: &PopupIdentity) -> bool {
        if source != self.id
            || self.foreign_client
            || !self.granted
            || !self.clear_required
            || self.cleared_revision.get() != Some(identity.revision)
            || !self.state.as_ref().is_some_and(|state| {
                state.client_id == identity.client_id
                    && (state.visible || self.shown == Some(false))
                    && state.instance == identity.instance
                    && state.content.key == identity.key
                    && state.revision == identity.revision
            })
        {
            return false;
        }
        self.clear_required = false;
        self.cleared_revision.set(None);
        if self.state.as_ref().is_some_and(|state| !state.visible) {
            self.presented_revision = Some(identity.revision);
        }
        true
    }

    fn announce(&self) {
        let (Some(owner), Some(state)) = (self.owner, &self.state) else {
            return;
        };
        let ready = Ready {
            id: self.id,
            client_id: self.client_id,
            instance: state.instance.clone(),
            key: state.content.key.clone(),
            revision: state.revision,
        };
        if let Ok(payload) = serde_json::to_string(&ready) {
            pipe_message_to_plugin(
                MessageToPlugin::new(READY)
                    .with_destination_plugin_id(owner)
                    .with_payload(payload),
            );
        }
    }

    fn record_fault(&mut self, message: &str) {
        if self.fault.as_deref() != Some(message) {
            self.fault = Some(message.to_owned());
            self.fault_reported = false;
        }
        self.report_fault();
    }

    fn report_fault(&mut self) {
        if !self.granted || self.fault_reported {
            return;
        }
        let (Some(owner), Some(message)) = (self.owner, &self.fault) else {
            return;
        };
        let fault = FaultNotice {
            id: self.id,
            client_id: self.client_id,
            instance: self.instance.clone(),
            message: message.clone(),
        };
        if let Ok(payload) = serde_json::to_string(&fault) {
            pipe_message_to_plugin(
                MessageToPlugin::new(ERROR)
                    .with_destination_plugin_id(owner)
                    .with_payload(payload),
            );
            self.fault_reported = true;
        }
    }

    fn working_clock_active(&self) -> bool {
        if !self.complete_dimensions(self.rows.get(), self.columns.get()) {
            return false;
        }
        let layout = self.layout.borrow();
        layout
            .lines
            .iter()
            .skip(layout.scroll)
            .take(self.viewport.get())
            .any(|line| working_clock_visible(&line.row, self.columns.get().saturating_sub(2)))
    }

    pub fn render(&self, rows: usize, columns: usize) {
        if self.foreign_client || !self.granted {
            return;
        }
        if self.clear_required {
            if let Some(state) = &self.state
                && let Ok(payload) = serde_json::to_string(&state.identity())
            {
                self.cleared_revision.set(Some(state.revision));
                pipe_message_to_plugin(
                    MessageToPlugin::new(CLEARED)
                        .with_destination_plugin_id(self.id)
                        .with_payload(payload),
                );
            }
            print!("{OSC8_CLOSE}\u{1b}[2J");
            return;
        }
        if !self.stage_render(rows, columns) {
            // Do not restore a retained old card while waiting for native resize/show.
            print!("{OSC8_CLOSE}\u{1b}[2J");
            return;
        }
        let state = self.state.as_ref().unwrap();
        if self.presented_revision != Some(state.revision)
            && let Ok(payload) = serde_json::to_string(&state.identity())
        {
            // Native delivers this only after applying these complete, correctly sized bytes.
            pipe_message_to_plugin(
                MessageToPlugin::new(PRESENTED)
                    .with_destination_plugin_id(self.id)
                    .with_payload(payload),
            );
        }
        if !state.visible {
            print!("{OSC8_CLOSE}\u{1b}[2J");
            return;
        }
        self.rows.set(rows);
        self.viewport.set(rows.saturating_sub(3));
        self.columns.set(columns);
        if let Some(notice) = self.reconcile_pointer() {
            self.notify_pointer(&notice);
        }
        let mut layout = self.layout.borrow_mut();
        let content_columns = columns.saturating_sub(2);
        debug_assert_eq!(layout.columns, content_columns);
        layout.scroll = layout
            .scroll
            .min(layout.lines.len().saturating_sub(self.viewport.get()));
        drop(layout);
        let active = self.working_clock_active();
        self.working_clock
            .borrow_mut()
            .schedule(Instant::now(), active);
        let clock_frame = self.working_clock.borrow().frame();
        let layout = self.layout.borrow();
        let footer = if layout.lines.len() > self.viewport.get() {
            format!("↕ Scroll {}/{}", layout.scroll + 1, layout.lines.len())
        } else {
            String::new()
        };
        print!("{OSC8_CLOSE}");
        for row in 0..rows {
            let text = if row > 0 && row + 2 == rows {
                format!("{}{}", self.theme.muted, ui::fit(&footer, content_columns))
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
                            &self.theme,
                            clock_frame,
                        )
                    })
                    .unwrap_or_else(|| " ".repeat(content_columns))
            };
            let line = outlined_line(text, row, rows, columns, &self.theme);
            print!(
                "\u{1b}[{};1H{}{}{line}{}",
                row + 1,
                ui::RESET,
                self.theme.base,
                ui::RESET
            );
        }
        print!("{OSC8_CLOSE}");
    }
}

#[derive(Default)]
pub struct GuideOverlay {
    theme: ui::Theme,
    owner: Option<u32>,
    id: u32,
    instance: String,
    generation: u64,
    granted: bool,
    owner_seen: bool,
    closed: bool,
    shown: Option<bool>,
    coordinates: Option<FloatingPaneCoordinates>,
    state: Option<GuideFrame>,
}

impl GuideOverlay {
    pub fn set_terminal_colors(&mut self, colors: &crate::palette::TerminalColors) -> bool {
        self.theme.update(colors)
    }

    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32) {
        self.id = id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        self.instance = configuration
            .get("guide_instance")
            .cloned()
            .unwrap_or_default();
        self.generation = configuration
            .get("guide_generation")
            .and_then(|generation| generation.parse().ok())
            .unwrap_or_default();
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::CustomMessage,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::BeforeClose,
        ]);
        request_permission(super::PERMISSIONS);
    }

    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                set_selectable(false);
                if !self.instance.is_empty()
                    && self.generation > 0
                    && self
                        .owner
                        .is_some_and(|owner| get_pane_info(PaneId::Plugin(owner)).is_some())
                {
                    self.owner_seen = true;
                    self.apply();
                    return !self.closed;
                }
                self.retire();
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.closed = true;
                self.state = None;
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
                    self.retire();
                }
            }
            Event::PaneClosed(PaneId::Plugin(id)) if Some(id) == self.owner && self.granted => {
                self.retire();
            }
            Event::BeforeClose => {
                self.closed = true;
                self.state = None;
            }
            _ => {}
        }
        false
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name != GUIDE_UPDATE {
            return false;
        }
        let PipeSource::Plugin(source) = message.source else {
            return false;
        };
        if Some(source) != self.owner {
            return false;
        }
        let Some(payload) = message.payload else {
            return false;
        };
        if payload.len() > MAX_GUIDE_PAYLOAD {
            return false;
        }
        let Ok(frame) = serde_json::from_str::<GuideFrame>(&payload) else {
            return false;
        };
        if !self.accept(source, frame) {
            return false;
        }
        self.apply();
        !self.closed
    }

    fn accept(&mut self, source: u32, mut frame: GuideFrame) -> bool {
        if self.closed
            || Some(source) != self.owner
            || self.instance.is_empty()
            || frame.instance != self.instance
            || frame.generation != self.generation
            || frame.revision == 0
            || frame.lines.len() > MAX_GUIDE_ROWS
            || frame.visible && (frame.closed || frame.lines.is_empty())
            || self
                .state
                .as_ref()
                .is_some_and(|current| current.closed || frame.revision <= current.revision)
        {
            return false;
        }
        frame.coordinates.pinned = Some(true);
        frame.coordinates.borderless = Some(true);
        self.state = Some(frame);
        true
    }

    fn retire(&mut self) {
        if !self.closed {
            self.closed = true;
            self.state = None;
            if self.granted {
                close_self();
            }
        }
    }

    fn apply(&mut self) {
        if !self.granted || self.closed {
            return;
        }
        if self.state.as_ref().is_some_and(|state| state.closed) {
            self.retire();
            return;
        }
        match &self.state {
            Some(state) if state.visible => {
                let pane = PaneId::Plugin(self.id);
                let restored = self.shown != Some(true);
                if restored {
                    // Unsuppressing restores native defaults before our explicit placement.
                    show_pane_with_id(pane, true, false);
                    self.shown = Some(true);
                }
                if restored || self.coordinates.as_ref() != Some(&state.coordinates) {
                    change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
                    self.coordinates = Some(state.coordinates.clone());
                }
            }
            _ => {
                if self.shown != Some(false) {
                    hide_self();
                    self.shown = Some(false);
                    // Unsuppressing restores native defaults; always reapply our next geometry.
                    self.coordinates = None;
                }
            }
        }
    }

    pub fn render(&self, rows: usize, columns: usize) {
        let Some(state) = self
            .state
            .as_ref()
            .filter(|state| state.visible && !state.closed)
        else {
            return;
        };
        if !self.granted || self.closed || self.shown != Some(true) || rows == 0 || columns == 0 {
            return;
        }
        let complete = state.lines.len() <= rows
            && state.coordinates.width
                == FloatingPaneCoordinates::default()
                    .with_width_fixed(columns)
                    .width
            && state.coordinates.height
                == FloatingPaneCoordinates::default()
                    .with_height_fixed(rows)
                    .height;
        if complete {
            let ready = GuideReady {
                id: self.id,
                instance: state.instance.clone(),
                generation: state.generation,
                revision: state.revision,
            };
            if let (Some(owner), Ok(payload)) = (self.owner, serde_json::to_string(&ready)) {
                // Render-origin messages are delivered only after this complete frame is applied.
                // Host commands still precede ANSI because both share the plugin's stdout.
                pipe_message_to_plugin(
                    MessageToPlugin::new(GUIDE_READY)
                        .with_destination_plugin_id(owner)
                        .with_payload(payload),
                );
            }
        }
        for row in 0..rows {
            let (line, cells) = state
                .lines
                .get(row)
                .map(|line| guide_line_prefix(line, columns))
                .unwrap_or(("", 0));
            print!(
                "\u{1b}[{};1H{OSC8_CLOSE}{}{}{line}{OSC8_CLOSE}{}{}{:padding$}{}",
                row + 1,
                ui::RESET,
                self.theme.base,
                ui::RESET,
                self.theme.base,
                "",
                ui::RESET,
                padding = columns.saturating_sub(cells),
            );
        }
    }
}

fn guide_line_prefix(line: &str, columns: usize) -> (&str, usize) {
    let bytes = line.as_bytes();
    let mut offset = 0;
    let mut cells = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'\x1b' {
            let start = offset;
            offset += 1;
            match bytes.get(offset) {
                Some(b'[') => {
                    offset += 1;
                    while offset < bytes.len() && !(0x40..=0x7e).contains(&bytes[offset]) {
                        offset += 1;
                    }
                    if offset == bytes.len() {
                        return (&line[..start], cells);
                    }
                    offset += 1;
                }
                Some(b']' | b'P' | b'_' | b'^') => {
                    offset += 1;
                    loop {
                        match bytes.get(offset) {
                            Some(b'\x07') => {
                                offset += 1;
                                break;
                            }
                            Some(b'\x1b') if bytes.get(offset + 1) == Some(&b'\\') => {
                                offset += 2;
                                break;
                            }
                            Some(_) => offset += 1,
                            None => return (&line[..start], cells),
                        }
                    }
                }
                Some(_) => offset += 1,
                None => return (&line[..start], cells),
            }
            continue;
        }
        let character = line[offset..].chars().next().unwrap();
        if character.is_control() {
            break;
        }
        let end = offset + character.len_utf8();
        let width = ui::width(&line[offset..end]);
        if cells + width > columns {
            break;
        }
        cells += width;
        offset = end;
    }
    (&line[..offset], cells)
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
                working_clock: false,
            }],
            ..ui::PopupRow::default()
        }
    }

    fn popup_frame(revision: u64) -> Tooltip {
        Tooltip {
            client_id: 0,
            instance: "popup:test".to_owned(),
            revision,
            visible: true,
            content: ui::PopupContent {
                key: "pane:1".to_owned(),
                title: row("Terminal", ui::PopupColor::Text),
                ..ui::PopupContent::default()
            },
            coordinates: FloatingPaneCoordinates::default()
                .with_x_fixed(40)
                .with_y_fixed(2)
                .with_width_fixed(48)
                .with_height_fixed(8),
        }
    }

    fn popup_overlay() -> Overlay {
        let mut overlay = Overlay {
            owner: Some(3),
            id: 9,
            granted: true,
            instance: "popup:test".to_owned(),
            ..Overlay::default()
        };
        assert!(overlay.accept(3, popup_frame(1)));
        present_popup(&mut overlay);
        overlay
    }

    fn present_popup(overlay: &mut Overlay) {
        let state = overlay.state.as_ref().unwrap();
        let identity = state.identity();
        let visible = state.visible;
        let (columns, rows) = if visible {
            let (_, _, columns, rows) = fixed_rectangle(&state.coordinates).unwrap();
            overlay.coordinates = Some(state.coordinates.clone());
            (columns, rows)
        } else {
            overlay.coordinates = None;
            (overlay.columns.get(), overlay.rows.get())
        };
        overlay.shown = Some(visible);
        overlay.rows.set(rows);
        overlay.columns.set(columns);
        overlay.viewport.set(rows.saturating_sub(3));
        if overlay.clear_required {
            overlay.cleared_revision.set(Some(identity.revision));
            assert!(overlay.accept_cleared(overlay.id, &identity));
        }
        assert!(overlay.stage_render(rows, columns));
        if overlay.presented_revision != Some(identity.revision) {
            assert!(overlay.accept_presented(overlay.id, &identity));
        }
    }

    #[test]
    fn working_clock_runs_only_for_painted_authorized_popup_rows() {
        let mut overlay = popup_overlay();
        let mut working = row(
            ui::working_clock_icon(0),
            ui::PopupColor::Native(bkpr::Status::Working),
        );
        working.spans[0].working_clock = true;
        working
            .spans
            .extend(row(" Working · Codex", ui::PopupColor::Text).spans);
        overlay.layout.get_mut().lines = wrapped_rows(&[working], 46);
        assert!(overlay.working_clock_active());
        overlay.layout.get_mut().scroll = 1;
        assert!(!overlay.working_clock_active());
        overlay.layout.get_mut().scroll = 0;
        overlay.viewport.set(0);
        assert!(!overlay.working_clock_active());
        overlay.viewport.set(5);
        overlay.clear_required = true;
        assert!(!overlay.working_clock_active());
        overlay.clear_required = false;
        overlay.granted = false;
        assert!(!overlay.working_clock_active());
        overlay.granted = true;
        overlay.state.as_mut().unwrap().visible = false;
        assert!(!overlay.working_clock_active());
        assert!(!overlay.event(Event::Timer(0.0)));
    }

    #[test]
    fn animated_popup_glyph_preserves_labels_links_and_truncated_widths() {
        let mut source = row(
            ui::working_clock_icon(0),
            ui::PopupColor::Native(bkpr::Status::Working),
        );
        source.spans[0].working_clock = true;
        source
            .spans
            .extend(row(" Working · Codex 日本語", ui::PopupColor::Text).spans);
        source.link = Some("https://forge.test/owner/project".to_owned());
        for columns in 0..=40 {
            let lines = wrapped_rows(&[source.clone()], columns);
            for line in lines {
                for frame in 0..12 {
                    let mut terminal = TerminalConsumer::default();
                    terminal.consume(&render_action_line(
                        &line.row,
                        columns,
                        false,
                        &ui::Theme::default(),
                        frame,
                    ));
                    let mut baseline = TerminalConsumer::default();
                    baseline.consume(&render_line(&line.row, columns));
                    assert_eq!(
                        terminal.text().replace(
                            ui::working_clock_icon(frame).trim_end(),
                            ui::working_clock_icon(0).trim_end()
                        ),
                        baseline.text(),
                    );
                    assert_eq!(ui::width(&terminal.text()), columns);
                    assert_eq!(terminal.cell_links(), baseline.cell_links());
                }
            }
        }
        let mut unflagged = source.clone();
        unflagged.spans[0].working_clock = false;
        assert!(!working_clock_visible(&unflagged, 40));
        assert_eq!(
            render_action_line(&unflagged, 40, false, &ui::Theme::default(), 0),
            render_action_line(&unflagged, 40, false, &ui::Theme::default(), 6),
        );
    }

    #[test]
    fn popup_hover_handoff_does_not_require_a_frame_without_style_changes() {
        let mut overlay = popup_overlay();
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(0, 0))));
        assert!(overlay.pointer_inside.get());
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(4, 20))));
        assert_eq!(overlay.pointer_position.get(), Some((4, 20)));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(-1, u16::MAX as usize))));
        assert!(!overlay.pointer_inside.get());
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(-1, u16::MAX as usize))));
    }

    #[test]
    fn popup_refresh_hover_only_repaints_styling_transitions() {
        let mut overlay = popup_overlay();
        let mut header = refresh_header();
        header.spans = row("CI", ui::PopupColor::Text).spans;
        let target = header.refresh.clone().unwrap();
        let mut frame = popup_frame(2);
        frame.content.title = header;
        assert!(overlay.accept(3, frame));
        present_popup(&mut overlay);

        assert!(overlay.event(Event::Mouse(Mouse::Hover(1, 46))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(1, 46))));
        assert!(overlay.event(Event::Mouse(Mouse::Hover(1, 1))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(1, 2))));
        assert!(overlay.event(Event::Mouse(Mouse::Hover(1, 46))));
        assert!(overlay.event(Event::Mouse(Mouse::LeftClick(1, 46))));
        assert_eq!(overlay.refresh_requested, vec![target]);
        assert!(!overlay.event(Event::Mouse(Mouse::LeftClick(1, 46))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(1, 46))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(1, 1))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(1, 46))));
        assert!(!overlay.event(Event::Mouse(Mouse::Hover(-1, u16::MAX as usize))));
        assert!(!overlay.pointer_inside.get());
    }

    #[test]
    fn popup_noop_events_and_scroll_limits_do_not_request_frames() {
        let mut overlay = popup_overlay();
        let manifest = PaneManifest {
            panes: std::collections::HashMap::from([(
                0,
                vec![PaneInfo {
                    id: 3,
                    is_plugin: true,
                    ..PaneInfo::default()
                }],
            )]),
        };
        assert!(!overlay.event(Event::PaneUpdate(manifest.clone())));
        assert!(overlay.owner_seen);
        assert!(!overlay.event(Event::PaneUpdate(manifest)));
        assert!(!overlay.event(Event::Mouse(Mouse::RightClick(1, 1))));
        assert!(!overlay.event(Event::Mouse(Mouse::LeftClick(1, 1))));
        assert!(!overlay.event(Event::Mouse(Mouse::ScrollUp(1))));
        assert!(!overlay.event(Event::Mouse(Mouse::ScrollDown(1))));

        let mut frame = popup_frame(2);
        frame.content.intro = (0..8)
            .map(|index| row(&format!("Row {index}"), ui::PopupColor::Text))
            .collect();
        assert!(overlay.accept(3, frame));
        present_popup(&mut overlay);
        assert!(overlay.event(Event::Mouse(Mouse::ScrollDown(usize::MAX))));
        assert!(!overlay.event(Event::Mouse(Mouse::ScrollDown(1))));
        assert!(overlay.event(Event::Mouse(Mouse::ScrollUp(usize::MAX))));
        assert!(!overlay.event(Event::Mouse(Mouse::ScrollUp(1))));
    }

    fn ready_for(frame: &Tooltip, id: u32) -> Ready {
        Ready {
            id,
            client_id: frame.client_id,
            instance: frame.instance.clone(),
            key: frame.content.key.clone(),
            revision: frame.revision,
        }
    }

    #[test]
    fn complete_frame_ack_is_required_after_resize_retarget_and_rapid_reopen() {
        let mut overlay = popup_overlay();
        let old = popup_frame(1).identity();
        let mut hidden = popup_frame(2);
        hidden.visible = false;
        hidden.content = ui::PopupContent::default();
        assert!(overlay.accept(3, hidden.clone()));
        overlay.shown = Some(false);
        assert!(!overlay.stage_render(8, 48));
        overlay.cleared_revision.set(Some(2));
        assert!(!overlay.accept_cleared(9, &old));
        assert!(overlay.accept_cleared(9, &hidden.identity()));
        assert!(overlay.pointer_notice().is_none());

        let reopened = popup_frame(3);
        assert!(overlay.accept(3, reopened.clone()));
        overlay.shown = Some(true);
        assert!(!overlay.stage_render(8, 48));
        assert!(!overlay.accept_presented(9, &hidden.identity()));
        let mut target = popup_frame(4);
        target.content.key = "pane:2".to_owned();
        target.coordinates = target
            .coordinates
            .with_width_fixed(60)
            .with_height_fixed(12);
        assert!(overlay.accept(3, target.clone()));
        assert!(overlay.pointer_query(3, &target.identity()).is_none());
        assert!(!overlay.stage_render(8, 48));
        assert_eq!(overlay.rendered_revision.get(), None);
        assert!(!overlay.accept_presented(9, &reopened.identity()));
        assert!(!overlay.accept_presented(9, &target.identity()));
        overlay.cleared_revision.set(Some(4));
        assert!(!overlay.accept_cleared(9, &reopened.identity()));
        assert!(overlay.accept_cleared(9, &target.identity()));
        assert!(!overlay.stage_render(12, 60));
        overlay.coordinates = Some(target.coordinates.clone());
        assert!(overlay.stage_render(12, 60));
        assert!(!overlay.accept_presented(8, &target.identity()));
        assert!(overlay.accept_presented(9, &target.identity()));
        assert!(!overlay.accept_presented(9, &target.identity()));
        assert!(overlay.pointer_query(3, &target.identity()).is_some());
        assert!(!overlay.accept(3, hidden));
        assert_eq!(overlay.state.as_ref().unwrap().revision, 4);
        assert!(overlay.fault.is_none());
    }

    #[test]
    fn staged_geometry_does_not_publish_pointer_or_refresh_from_the_previous_frame() {
        let mut overlay = popup_overlay();
        assert!(overlay.pointer_hover(6, 30).unwrap().inside);
        let mut resized = popup_frame(2);
        resized.coordinates = resized.coordinates.with_height_fixed(12);
        assert!(overlay.accept(3, resized.clone()));
        assert!(overlay.pointer_inside.get());
        assert!(overlay.pointer_query(3, &resized.identity()).is_none());
        assert!(!overlay.stage_render(8, 48));
        assert!(overlay.pointer_inside.get());
        assert!(overlay.refresh_at(1, 46).is_none());
        overlay.cleared_revision.set(Some(2));
        assert!(overlay.accept_cleared(9, &resized.identity()));
        overlay.coordinates = Some(resized.coordinates.clone());
        assert!(overlay.stage_render(12, 48));
        assert!(overlay.accept_presented(9, &resized.identity()));
        assert!(
            overlay
                .pointer_query(3, &resized.identity())
                .unwrap()
                .inside
        );
    }

    #[test]
    fn hover_during_staging_uses_the_still_applied_native_rectangle() {
        let mut overlay = popup_overlay();
        assert!(overlay.pointer_hover(2, 2).unwrap().inside);
        let mut moved = popup_frame(2);
        moved.coordinates = moved
            .coordinates
            .with_x_fixed(39)
            .with_y_fixed(0)
            .with_width_fixed(60)
            .with_height_fixed(12);
        assert!(overlay.accept(3, moved.clone()));
        assert_eq!(overlay.pointer_position.get(), Some((4, 3)));
        assert!(overlay.pointer_hover(2, 2).is_none());
        assert_eq!(overlay.pointer_position.get(), Some((4, 3)));
        assert!(overlay.pointer_query(3, &moved.identity()).is_none());
        present_popup(&mut overlay);
        assert!(overlay.pointer_query(3, &moved.identity()).unwrap().inside);
    }

    #[test]
    fn stale_ready_binds_helper_without_confirming_the_latest_frame_and_cold_close_retires_it() {
        let first = popup_frame(1);
        let mut current = popup_frame(2);
        current.content.key = "pane:2".to_owned();
        let mut controller = Controller {
            instance: current.instance.clone(),
            revision: 2,
            pending: Some(current.clone()),
            launched: true,
            ..Controller::default()
        };
        assert!(!controller.ready(&ready_for(&first, 9), 3, &BTreeMap::new()));
        assert_eq!(controller.id, Some(9));
        assert_eq!(controller.pending.as_ref().unwrap().revision, 2);
        assert!(controller.ready(&ready_for(&current, 9), 3, &BTreeMap::new()));

        let mut cold = Controller {
            instance: first.instance.clone(),
            revision: 1,
            pending: Some(first.clone()),
            launched: true,
            ..Controller::default()
        };
        cold.close(3, &BTreeMap::new());
        assert!(cold.close_pending);
        let hidden = cold.pending.as_ref().unwrap().clone();
        assert!(!hidden.visible);
        assert!(!cold.ready(&ready_for(&hidden, 9), 3, &BTreeMap::new()));
        assert!(cold.id.is_none());
        assert!(cold.pending.is_none());
        assert!(!cold.launched && !cold.close_pending);
        assert!(!cold.ready(&ready_for(&first, 9), 3, &BTreeMap::new()));
        assert!(cold.id.is_none());
    }

    #[test]
    fn retired_foreign_and_malformed_unscoped_frames_leave_the_presented_card_untouched() {
        let mut overlay = popup_overlay();
        for payload in [
            "not json".to_owned(),
            serde_json::to_string(&Tooltip {
                client_id: 1,
                ..popup_frame(2)
            })
            .unwrap(),
            serde_json::to_string(&Tooltip {
                instance: "retired".to_owned(),
                ..popup_frame(2)
            })
            .unwrap(),
            serde_json::to_string(&Tooltip {
                coordinates: FloatingPaneCoordinates::default(),
                ..popup_frame(1)
            })
            .unwrap(),
        ] {
            let message =
                PipeMessage::new(PipeSource::Plugin(3), UPDATE, &Some(payload), &None, true);
            assert!(!overlay.pipe(message));
            assert_eq!(overlay.state.as_ref().unwrap().revision, 1);
            assert_eq!(overlay.presented_revision, Some(1));
            assert!(overlay.fault.is_none());
        }
        let bad = Tooltip {
            coordinates: FloatingPaneCoordinates::default(),
            ..popup_frame(2)
        };
        let message = PipeMessage::new(
            PipeSource::Plugin(3),
            UPDATE,
            &Some(serde_json::to_string(&bad).unwrap()),
            &None,
            true,
        );
        assert!(!overlay.pipe(message));
        assert!(overlay.fault.is_some());
        assert_eq!(overlay.state.as_ref().unwrap().revision, 1);
        assert_eq!(overlay.presented_revision, Some(1));
    }

    #[test]
    fn genuine_permission_and_protocol_faults_remain_authenticated_owner_diagnostics() {
        let mut overlay = popup_overlay();
        overlay.event(Event::PermissionRequestResult(PermissionStatus::Denied));
        assert!(!overlay.granted);
        assert!(overlay.fault.is_some());
        assert!(!overlay.fault_reported);
        assert!(!overlay.complete_dimensions(8, 48));
        assert!(overlay.pointer_notice().is_none());

        let frame = popup_frame(1);
        let mut controller = Controller {
            instance: frame.instance.clone(),
            pending: Some(frame),
            ..Controller::default()
        };
        let notice = FaultNotice {
            id: 9,
            client_id: 0,
            instance: "popup:test".to_owned(),
            message: "Tooltip permissions denied".to_owned(),
        };
        assert!(!controller.accept_fault(8, &notice));
        assert!(!controller.accept_fault(
            9,
            &FaultNotice {
                client_id: 1,
                ..notice.clone()
            }
        ));
        assert!(!controller.accept_fault(
            9,
            &FaultNotice {
                instance: "retired".to_owned(),
                ..notice.clone()
            }
        ));
        assert!(controller.id.is_none());
        assert!(controller.accept_fault(9, &notice));
        assert_eq!(controller.id, Some(9));
    }

    #[test]
    fn pointer_notices_require_current_helper_frame_and_visible_content() {
        let frame = popup_frame(2);
        let mut controller = Controller {
            id: Some(9),
            pending: Some(frame.clone()),
            ..Controller::default()
        };
        let mut notice = PointerNotice {
            client_id: frame.client_id,
            instance: frame.instance.clone(),
            key: frame.content.key.clone(),
            revision: frame.revision,
            inside: true,
        };
        assert!(controller.accept_pointer(9, &notice));
        notice.inside = false;
        assert!(controller.accept_pointer(9, &notice));
        assert!(!controller.accept_pointer(8, &notice));
        notice.client_id = 1;
        assert!(!controller.accept_pointer(9, &notice));
        notice.client_id = 0;
        for revision in [0, 1, 3] {
            notice.revision = revision;
            assert!(!controller.accept_pointer(9, &notice));
        }
        notice.revision = 2;
        notice.key = "pane:2".to_owned();
        assert!(!controller.accept_pointer(9, &notice));
        notice.key = frame.content.key.clone();
        notice.instance = "retired-popup".to_owned();
        assert!(!controller.accept_pointer(9, &notice));
        notice.instance = frame.instance.clone();
        controller.pending.as_mut().unwrap().revision = 3;
        assert!(!controller.accept_pointer(9, &notice));
        notice.revision = 3;
        controller.pending.as_mut().unwrap().visible = false;
        assert!(!controller.accept_pointer(9, &notice));
        assert!(!controller.probe_pointer(3));
        controller.pending.as_mut().unwrap().visible = true;
        controller.id = None;
        assert!(!controller.accept_pointer(9, &notice));
        assert!(!controller.probe_pointer(3));
    }

    #[test]
    fn pointer_probes_reject_foreign_and_stale_identity_but_always_answer_current_state() {
        let mut overlay = popup_overlay();
        let identity = popup_frame(1).identity();
        let outside = overlay.pointer_query(3, &identity).unwrap();
        assert!(!outside.inside);
        assert_eq!(overlay.pointer_query(3, &identity), Some(outside.clone()));
        assert!(overlay.pointer_query(4, &identity).is_none());
        for foreign in [
            PopupIdentity {
                client_id: 1,
                ..identity.clone()
            },
            PopupIdentity {
                instance: "other".to_owned(),
                ..identity.clone()
            },
            PopupIdentity {
                key: "pane:2".to_owned(),
                ..identity.clone()
            },
            PopupIdentity {
                revision: 0,
                ..identity.clone()
            },
            PopupIdentity {
                revision: 2,
                ..identity.clone()
            },
        ] {
            assert!(overlay.pointer_query(3, &foreign).is_none());
        }
        let entered = overlay.pointer_hover(0, 0).unwrap();
        assert!(entered.inside);
        assert!(overlay.pointer_hover(0, 0).is_none());
        // Enter delivery precedes sidebar leave; its explicit probe still gets a reply.
        assert_eq!(overlay.pointer_query(3, &identity), Some(entered));
        assert!(overlay.accept(3, popup_frame(2)));
        assert!(overlay.pointer_query(3, &identity).is_none());
        assert!(
            overlay
                .pointer_query(3, &popup_frame(2).identity())
                .is_none()
        );
        present_popup(&mut overlay);
        assert!(
            overlay
                .pointer_query(3, &popup_frame(2).identity())
                .unwrap()
                .inside
        );
        overlay.granted = false;
        assert!(
            overlay
                .pointer_query(3, &popup_frame(2).identity())
                .is_none()
        );
    }

    #[test]
    fn hover_includes_every_border_cell_and_only_notifies_when_inside_changes() {
        let overlay = popup_overlay();
        for (line, column) in [(0, 0), (0, 47), (7, 47), (7, 0)] {
            let enter = overlay.pointer_hover(line, column).unwrap();
            assert!(enter.inside);
            assert!(overlay.pointer_hover(4, 20).is_none());
            let leave = overlay.pointer_hover(-1, u16::MAX as usize).unwrap();
            assert!(!leave.inside);
            assert!(overlay.pointer_hover(-1, u16::MAX as usize).is_none());
        }
        for (line, column) in [(-1, 0), (8, 0), (0, 48), (0, u16::MAX as usize)] {
            assert!(overlay.pointer_hover(line, column).is_none());
            assert!(!overlay.pointer_inside.get());
        }
        assert!(overlay.pointer_hover(7, 47).unwrap().inside);
        overlay.columns.set(47);
        assert!(!overlay.reconcile_pointer().unwrap().inside);
        assert!(overlay.reconcile_pointer().is_none());
        assert!(overlay.pointer_hover(7, 46).unwrap().inside);
        overlay.rows.set(7);
        assert!(!overlay.reconcile_pointer().unwrap().inside);
        overlay.rows.set(0);
        overlay.columns.set(0);
        assert!(overlay.pointer_hover(0, 0).is_none());
    }

    #[test]
    fn pointer_surface_resets_on_new_item_hide_and_reopen_not_content_revision() {
        let mut overlay = popup_overlay();
        assert!(overlay.pointer_hover(2, 2).unwrap().inside);
        assert!(overlay.accept(3, popup_frame(2)));
        assert!(overlay.pointer_inside.get());
        assert!(!overlay.accept(4, popup_frame(3)));
        assert!(!overlay.accept(3, popup_frame(2)));
        assert!(overlay.pointer_inside.get());

        let mut moved = popup_frame(3);
        moved.coordinates = moved.coordinates.with_x_fixed(41);
        assert!(overlay.accept(3, moved));
        assert!(overlay.pointer_inside.get());
        assert_eq!(overlay.pointer_position.get(), Some((2, 1)));

        let mut next = popup_frame(4);
        next.content.key = "pane:2".to_owned();
        assert!(overlay.accept(3, next));
        assert!(!overlay.pointer_inside.get());
        present_popup(&mut overlay);
        assert!(overlay.pointer_hover(2, 2).unwrap().inside);
        let mut hidden = popup_frame(5);
        hidden.visible = false;
        assert!(overlay.accept(3, hidden));
        assert!(!overlay.pointer_inside.get());
        assert!(overlay.pointer_hover(2, 2).is_none());
        assert!(
            overlay
                .pointer_query(3, &popup_frame(5).identity())
                .is_none()
        );
        assert!(overlay.accept(3, popup_frame(6)));
        assert!(!overlay.pointer_inside.get());
        assert!(overlay.pointer_position.get().is_none());
        assert!(
            overlay
                .pointer_query(3, &popup_frame(6).identity())
                .is_none()
        );
        present_popup(&mut overlay);
        assert!(
            !overlay
                .pointer_query(3, &popup_frame(6).identity())
                .unwrap()
                .inside
        );
    }

    #[test]
    fn stationary_pointer_survives_async_growth_and_only_leaves_when_new_bounds_exclude_it() {
        let mut overlay = popup_overlay();
        assert!(overlay.pointer_hover(6, 30).unwrap().inside);
        let mut grown = popup_frame(2);
        grown.coordinates = grown
            .coordinates
            .with_x_fixed(39)
            .with_y_fixed(0)
            .with_width_fixed(60)
            .with_height_fixed(12);
        assert!(overlay.accept(3, grown.clone()));
        assert_eq!(overlay.pointer_position.get(), Some((8, 31)));
        assert!(overlay.pointer_query(3, &grown.identity()).is_none());
        present_popup(&mut overlay);
        assert!(overlay.pointer_query(3, &grown.identity()).unwrap().inside);
        assert!(overlay.reconcile_pointer().is_none());

        let mut clipped = grown;
        clipped.revision = 3;
        clipped.coordinates = clipped.coordinates.with_height_fixed(8);
        assert!(overlay.accept(3, clipped.clone()));
        assert!(!overlay.pointer_inside.get());
        assert!(overlay.pointer_position.get().is_none());
        assert!(overlay.pointer_query(3, &clipped.identity()).is_none());
        present_popup(&mut overlay);
        assert!(
            !overlay
                .pointer_query(3, &clipped.identity())
                .unwrap()
                .inside
        );
    }

    #[test]
    fn popup_wire_and_helper_replica_are_scoped_to_the_owning_client() {
        let mut overlay = popup_overlay();
        let mut foreign = popup_frame(2);
        foreign.client_id = 1;
        assert!(!overlay.accept(3, foreign));
        assert_eq!(overlay.state.as_ref().unwrap().revision, 1);

        let mut replica = Overlay::default();
        replica.load(
            &BTreeMap::from([
                ("owner".to_owned(), "3".to_owned()),
                ("owner_client_id".to_owned(), "1".to_owned()),
            ]),
            9,
            2,
        );
        assert!(replica.foreign_client);
        assert!(!replica.event(Event::PermissionRequestResult(PermissionStatus::Granted)));
        assert!(!replica.accept(3, popup_frame(1)));
        assert!(!replica.granted);
        assert!(replica.state.is_none());

        let frame = popup_frame(1);
        let mut controller = Controller {
            instance: frame.instance.clone(),
            revision: frame.revision,
            pending: Some(frame.clone()),
            ..Controller::default()
        };
        let mut ready = ready_for(&frame, 9);
        ready.client_id = 1;
        assert!(!controller.ready(&ready, 3, &BTreeMap::new()));
        assert!(controller.id.is_none());
        ready.client_id = 0;
        assert!(controller.ready(&ready, 3, &BTreeMap::new()));
        assert_eq!(controller.id, Some(9));
        assert!(serde_json::from_str::<Ready>(r#"{"id":9}"#).is_err());
    }

    #[test]
    fn generic_outside_press_does_not_dismiss_or_clear_popup_hover() {
        let mut overlay = popup_overlay();
        assert!(overlay.pointer_hover(0, 0).unwrap().inside);
        for mouse in [
            Mouse::LeftClick(-1, u16::MAX as usize),
            Mouse::RightClick(-1, u16::MAX as usize),
        ] {
            overlay.event(Event::Mouse(mouse));
            assert!(overlay.state.as_ref().unwrap().visible);
            assert!(overlay.pointer_inside.get());
        }
    }

    fn guide_frame(revision: u64) -> GuideFrame {
        GuideFrame {
            instance: "guide:test".to_owned(),
            generation: 2,
            revision,
            visible: true,
            closed: false,
            lines: vec!["\u{1b}[38;2;137;180;250mCtrl+p\u{1b}[0m Panes".to_owned()],
            coordinates: FloatingPaneCoordinates::default()
                .with_width_fixed(48)
                .with_height_fixed(1),
        }
    }

    fn guide_overlay() -> GuideOverlay {
        GuideOverlay {
            owner: Some(3),
            id: 7,
            instance: "guide:test".to_owned(),
            generation: 2,
            ..GuideOverlay::default()
        }
    }

    #[test]
    fn guide_consumer_isolates_owner_launch_and_revision() {
        let mut guide = guide_overlay();
        assert!(!guide.accept(4, guide_frame(1)));
        let mut foreign = guide_frame(1);
        foreign.instance = "another-guide".to_owned();
        assert!(!guide.accept(3, foreign));
        let mut old_launch = guide_frame(1);
        old_launch.generation = 1;
        assert!(!guide.accept(3, old_launch));
        assert!(!guide.accept(3, guide_frame(0)));
        assert!(guide.accept(3, guide_frame(2)));
        assert!(!guide.accept(3, guide_frame(1)));
        assert!(!guide.accept(3, guide_frame(2)));
        let state = guide.state.as_ref().unwrap();
        assert_eq!(state.coordinates.pinned, Some(true));
        assert_eq!(state.coordinates.borderless, Some(true));

        let mut hidden = guide_frame(3);
        hidden.visible = false;
        hidden.lines.clear();
        assert!(guide.accept(3, hidden));
        assert!(!guide.accept(3, guide_frame(2)));
        assert!(!guide.state.as_ref().unwrap().visible);
        assert!(guide.accept(3, guide_frame(4)));

        let mut retired = guide_frame(5);
        retired.visible = false;
        retired.closed = true;
        retired.lines.clear();
        assert!(guide.accept(3, retired));
        assert!(!guide.accept(3, guide_frame(6)));
    }

    #[test]
    fn guide_consumer_requires_complete_bounded_frames() {
        let mut guide = guide_overlay();
        let mut empty = guide_frame(1);
        empty.lines.clear();
        assert!(!guide.accept(3, empty));
        let mut oversized = guide_frame(1);
        oversized.lines = vec![String::new(); MAX_GUIDE_ROWS + 1];
        assert!(!guide.accept(3, oversized));
        let mut contradictory = guide_frame(1);
        contradictory.closed = true;
        assert!(!guide.accept(3, contradictory));
        assert!(guide.state.is_none());
    }

    #[test]
    fn guide_readiness_survives_updates_but_requires_a_fresh_frame_after_hiding() {
        let frame = guide_frame(3);
        let mut guide = GuideController {
            id: Some(7),
            instance: frame.instance.clone(),
            generation: frame.generation,
            revision: frame.revision,
            visible_revision: frame.revision,
            launched: true,
            pending: Some(frame),
            ..GuideController::default()
        };
        let configuration = BTreeMap::new();
        let mut ready = GuideReady {
            id: 7,
            instance: "guide:test".to_owned(),
            generation: 2,
            revision: 3,
        };
        assert!(!guide.ready_for_display());
        assert!(guide.ready(&ready, 3, &configuration));
        assert!(guide.ready_for_display());

        guide.pending.as_mut().unwrap().revision = 4;
        guide.revision = 4;
        assert!(guide.ready_for_display());
        ready.revision = 5;
        assert!(!guide.ready(&ready, 3, &configuration));
        ready.revision = 4;
        ready.id = 8;
        assert!(!guide.ready(&ready, 3, &configuration));
        ready.id = 7;
        assert!(guide.ready(&ready, 3, &configuration));

        guide.pending.as_mut().unwrap().visible = false;
        assert!(!guide.ready_for_display());
        guide.pending.as_mut().unwrap().visible = true;
        guide.pending.as_mut().unwrap().revision = 5;
        guide.revision = 5;
        guide.visible_revision = 5;
        assert!(!guide.ready_for_display());
        assert!(guide.ready(&ready, 3, &configuration));
        assert!(!guide.ready_for_display());
        ready.revision = 5;
        assert!(guide.ready(&ready, 3, &configuration));
        assert!(guide.ready_for_display());

        guide.forget(8);
        assert_eq!(guide.id, Some(7));
        guide.forget(7);
        assert!(guide.id.is_none());
        assert!(guide.pending.is_none());
        assert_eq!(guide.generation, 3);
        assert!(!guide.ready(&ready, 3, &configuration));
    }

    #[test]
    fn guide_clipping_preserves_complete_ansi_and_unicode_cells() {
        let line = "\u{1b}[38;2;137;180;250mCtrl+p\u{1b}[0m \u{1b}]8;;https://example.test\u{1b}\\界e\u{301}\u{1b}]8;;\u{1b}\\";
        for columns in 0..=12 {
            let (prefix, cells) = guide_line_prefix(line, columns);
            let mut terminal = TerminalConsumer::default();
            terminal.consume(prefix);
            terminal.consume(OSC8_CLOSE);
            assert_eq!(ui::width(&terminal.text()), cells);
            assert!(cells <= columns);
            assert!(terminal.link.is_none());
            if columns >= 10 {
                assert!(terminal.text().ends_with("界e\u{301}"));
            }
        }
        assert_eq!(guide_line_prefix("abc\u{1b}[38;2;1", 20), ("abc", 3));
        assert_eq!(guide_line_prefix("abc\u{1b}]8;;unfinished", 20), ("abc", 3));
        assert_eq!(guide_line_prefix("first\nsecond", 20), ("first", 5));
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
            repository: bkpr::ci_contract::ForgeRepository {
                provider: bkpr::ci_contract::ForgeProvider::Github,
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
        for (subtitle, header_line) in [
            (row("idle", ui::PopupColor::Text), 3),
            (ui::PopupRow::default(), 2),
        ] {
            for columns in 0_usize..=100 {
                let mut overlay = Overlay {
                    granted: true,
                    owner: Some(7),
                    instance: "instance".to_owned(),
                    state: Some(Tooltip {
                        client_id: 0,
                        instance: "instance".to_owned(),
                        revision: 42,
                        visible: true,
                        content: ui::PopupContent {
                            key: "pane:1".to_owned(),
                            title: row("App", ui::PopupColor::Text),
                            subtitle: subtitle.clone(),
                            intro: vec![
                                header.clone(),
                                row("check with duration", ui::PopupColor::Text),
                            ],
                            ..ui::PopupContent::default()
                        },
                        coordinates: FloatingPaneCoordinates::default()
                            .with_x_fixed(0)
                            .with_y_fixed(0)
                            .with_width_fixed(columns)
                            .with_height_fixed(9),
                    }),
                    ..Overlay::default()
                };
                let frame = overlay.state.take().unwrap();
                if columns > 0 {
                    assert!(overlay.accept(7, frame));
                    present_popup(&mut overlay);
                } else {
                    overlay.state = Some(frame);
                }
                for line in -1..=9 {
                    for column in 0..=columns {
                        let hit = overlay.refresh_at(line, column);
                        let expected = columns >= ui::refresh_button_width() + 2
                            && line == header_line
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
                        &overlay.layout.borrow().lines[header_line as usize - 1].row,
                        columns - 2,
                    ));
                    assert_eq!(terminal.glyphs.last(), Some(&(']', None)));
                    overlay.event(Event::Mouse(Mouse::LeftClick(header_line, columns - 2)));
                    assert!(overlay.refresh_at(header_line, columns - 2).is_none());
                    assert!(overlay.state.as_ref().unwrap().visible);
                }
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
                let normal =
                    render_action_line(&lines[0].row, columns, false, &ui::Theme::default(), 0);
                let hover =
                    render_action_line(&lines[0].row, columns, true, &ui::Theme::default(), 0);
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
            ui::PopupColor::Ci(bkpr::ci_contract::CiStatus::Unknown),
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
                    color: ui::PopupColor::Native(bkpr::Status::Idle),
                    bold: true,
                    working_clock: false,
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
            owner: Some(7),
            instance: "instance".to_owned(),
            state: Some(Tooltip {
                client_id: 0,
                instance: "instance".to_owned(),
                revision: 2,
                visible: true,
                content: ui::PopupContent {
                    key: "workspace:1".to_owned(),
                    title: row("Workspace", ui::PopupColor::Text),
                    subtitle: row("1 terminal", ui::PopupColor::Muted),
                    intro: vec![header.clone(), row("check", ui::PopupColor::Text)],
                    ..ui::PopupContent::default()
                },
                coordinates: FloatingPaneCoordinates::default()
                    .with_x_fixed(0)
                    .with_y_fixed(0)
                    .with_width_fixed(16)
                    .with_height_fixed(5),
            }),
            ..Overlay::default()
        };
        let frame = overlay.state.take().unwrap();
        assert!(overlay.accept(7, frame));
        present_popup(&mut overlay);
        assert!(overlay.refresh_at(3, 14).is_none()); // Footer, not the hidden header.
        overlay.event(Event::Mouse(Mouse::ScrollDown(2)));
        present_popup(&mut overlay);
        assert_eq!(
            overlay.refresh_at(1, 14).unwrap().target,
            header.refresh.unwrap()
        );
        assert!(overlay.refresh_at(2, 14).is_none()); // Wrapped continuation has no action.
        let mut loading = overlay.state.as_ref().unwrap().clone();
        loading.revision += 1;
        loading.content.intro[0].refresh = None;
        assert!(overlay.accept(7, loading));
        assert!(overlay.refresh_at(1, 14).is_none());
        present_popup(&mut overlay);
        assert!(overlay.refresh_at(1, 14).is_none());
        let mut hidden = overlay.state.as_ref().unwrap().clone();
        hidden.revision += 1;
        hidden.visible = false;
        assert!(overlay.accept(7, hidden));
        assert!(overlay.refresh_at(1, 14).is_none());
    }

    #[test]
    fn popup_wraps_unicode_and_preserves_palette_boundaries_without_controls() {
        let rows = vec![ui::PopupRow {
            spans: vec![
                ui::PopupSpan {
                    text: "\u{f05e1} ".to_owned(),
                    color: ui::PopupColor::Ci(bkpr::ci_contract::CiStatus::Passed),
                    bold: false,
                    working_clock: false,
                },
                ui::PopupSpan {
                    text: "日本語\u{1b}\n checks".to_owned(),
                    color: ui::PopupColor::Text,
                    bold: true,
                    working_clock: false,
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
                    assert!(rendered.contains(span.color.ansi(&ui::Theme::default())));
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
            working_clock: false,
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
                working_clock: false,
            },
            ui::PopupSpan {
                text: " -567".to_owned(),
                color: ui::PopupColor::Deleted,
                bold: false,
                working_clock: false,
            },
        ];
        branch.right = vec![ui::PopupSpan {
            text: "\u{f419} origin/日本語/a-long-comparison-base".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
            working_clock: false,
        }];
        branch.link = Some("https://forge.test/owner/project".to_owned());
        let mut check = row("unit tests", ui::PopupColor::Text);
        check.truncate = true;
        check.right = vec![ui::PopupSpan {
            text: "2:03".to_owned(),
            color: ui::PopupColor::Muted,
            bold: false,
            working_clock: false,
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
            working_clock: false,
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
            working_clock: false,
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
            working_clock: false,
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
                    &ui::Theme::default(),
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
    fn pane_preview_links_keep_actual_pr_and_linear_targets_out_of_padding_and_chrome() {
        use bkpr::ci_contract::{
            ChangeRequest, ChangeRequestState, CiFetchState, CiSnapshot, CiStatus, ForgeProvider,
            ForgeRepository, IssueTracker, LinkedIssue,
        };
        let pr = "https://github.com/owner/repo/pull/42";
        let ticket = "https://linear.app/team/issue/ENG-42/unicode-preview";
        let snapshot = bkpr::Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
            instance_id: "preview:test".to_owned(),
            revision: 1,
            session: "test".to_owned(),
            connected: true,
            error: None,
            focused_pane_id: None,
            workspaces: Vec::new(),
            panes: vec![bkpr::Pane {
                id: "pane".to_owned(),
                terminal_id: "terminal_1".to_owned(),
                workspace_id: "workspace".to_owned(),
                tab_id: "tab".to_owned(),
                name: "Editor".to_owned(),
                description: String::new(),
                harness: None,
                agent_id: None,
                status: bkpr::Status::Idle,
                attention: None,
                cwd: None,
                branch: None,
                branch_changes: None,
                project: None,
                ci: Some(CiSnapshot {
                    status: CiStatus::NoChecks,
                    checks: Vec::new(),
                    change_request: Some(ChangeRequest {
                        number: 42,
                        title: "日本語 pull request title that wraps".to_owned(),
                        url: pr.to_owned(),
                        state: ChangeRequestState::Open,
                        description: String::new(),
                        issues: vec![LinkedIssue {
                            provider: IssueTracker::Linear,
                            identifier: "ENG-42".to_owned(),
                            title: Some("日本語 linked ticket with wrapping context".to_owned()),
                            url: ticket.to_owned(),
                        }],
                        base: None,
                    }),
                    fetch_state: CiFetchState::Ready,
                    updated_at: 0,
                    stale: false,
                    error: None,
                    repository: ForgeRepository {
                        provider: ForgeProvider::Github,
                        host: "github.com".to_owned(),
                        path: "owner/repo".to_owned(),
                        web_url: "https://github.com/owner/repo".to_owned(),
                    },
                    head_sha: "abc123".to_owned(),
                    branch: None,
                    local_head_sha: "abc123".to_owned(),
                    checked_ref: None,
                    truncated: false,
                    commit_missing: false,
                }),
            }],
        };
        let content = ui::tooltip(&ui::Item::Pane("pane".to_owned()), &snapshot);
        let rows = content.rows();
        assert!(
            rows.iter()
                .any(|row| row.link.as_deref() == Some(pr) && row.plain().contains("#42"))
        );
        assert!(
            rows.iter()
                .any(|row| row.link.as_deref() == Some(ticket) && row.plain().contains("ENG-42"))
        );
        for columns in 4..=64 {
            let mut destinations = std::collections::BTreeSet::new();
            for line in wrapped_rows(&rows, columns) {
                let mut terminal = TerminalConsumer::default();
                terminal.consume(&outlined_line(
                    render_line(&line.row, columns),
                    1,
                    3,
                    columns + 2,
                    &ui::Theme::default(),
                ));
                let cells = terminal.cell_links();
                assert_eq!(cells.len(), columns + 2);
                assert!(cells[0].is_none() && cells[columns + 1].is_none());
                let body = ui::width(&line.row.plain()).min(columns);
                if let Some(target) = line.row.link.as_deref() {
                    assert!(cells[1..body + 1].iter().all(|link| *link == Some(target)));
                    destinations.insert(target.to_owned());
                }
                assert!(cells[body + 1..].iter().all(Option::is_none));
                assert!(terminal.link.is_none());
            }
            assert!(destinations.contains(pr) && destinations.contains(ticket));
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
            terminal.consume(&outlined_line(text, index, 5, 18, &ui::Theme::default()));
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
        terminal.consume(&outlined_line(
            String::new(),
            0,
            0,
            0,
            &ui::Theme::default(),
        ));
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
                &ui::Theme::default(),
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
            subtitle: row("working", ui::PopupColor::Native(bkpr::Status::Working)),
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
        let tiny_tab = TabInfo {
            display_area_columns: 31,
            ..tab
        };
        assert!(geometry(&sidebar, 0, &tiny_tab, 0..=1, &content).is_none());
    }

    #[test]
    fn popup_width_grows_from_minimum_to_cap_and_wraps_overflow() {
        let sidebar = PaneInfo {
            pane_content_columns: 20,
            ..PaneInfo::default()
        };
        let tab = TabInfo {
            display_area_columns: 140,
            display_area_rows: 40,
            ..TabInfo::default()
        };
        for (length, expected) in [(10, 48), (50, 52), (100, 80)] {
            let source = row(&"word ".repeat(length / 5), ui::PopupColor::Text);
            let content = ui::PopupContent {
                title: source.clone(),
                ..ui::PopupContent::default()
            };
            let placement = geometry(&sidebar, 20, &tab, 0..=0, &content).unwrap();
            assert_eq!(
                placement.coordinates.width,
                FloatingPaneCoordinates::default()
                    .with_width_fixed(expected)
                    .width
            );
            let lines = wrapped_rows(&[source], expected - 2);
            assert_eq!(lines.len() > 1, length > expected - 2);
            for line in lines {
                let text = visible(&render_line(&line.row, expected - 2));
                assert_eq!(ui::width(&text), expected - 2);
                assert!(!text.trim().contains("wor "));
            }
        }
    }

    #[test]
    fn natural_unicode_width_keeps_suffix_and_unlinked_refresh_button_visible() {
        let mut header = refresh_header();
        header.spans = row(&"界e\u{301}".repeat(16), ui::PopupColor::Text).spans;
        header.suffix = row("+12", ui::PopupColor::Added).spans;
        let content = ui::PopupContent {
            title: header.clone(),
            ..ui::PopupContent::default()
        };
        let sidebar = PaneInfo {
            pane_content_columns: 20,
            ..PaneInfo::default()
        };
        let tab = TabInfo {
            display_area_columns: 140,
            display_area_rows: 20,
            ..TabInfo::default()
        };
        let width = 48 + 3 + 1 + ui::refresh_button_width() + 2;
        let placement = geometry(&sidebar, 20, &tab, 0..=0, &content).unwrap();
        assert_eq!(
            placement.coordinates.width,
            FloatingPaneCoordinates::default()
                .with_width_fixed(width)
                .width
        );
        let lines = wrapped_rows(&[header.clone()], width - 2);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].row.refresh, header.refresh);
        let mut terminal = TerminalConsumer::default();
        terminal.consume(&render_line(&lines[0].row, width - 2));
        assert_eq!(ui::width(&terminal.text()), width - 2);
        assert!(
            terminal
                .text()
                .starts_with(&format!("{}+12", "界e\u{301}".repeat(16)))
        );
        assert!(terminal.text().ends_with(ui::refresh_button_text()));
        assert!(
            terminal.cell_links()[width - 2 - ui::refresh_button_width()..]
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn resized_sidebar_popup_overlaps_only_the_separator_not_item_cells() {
        let content = ui::PopupContent {
            title: row("Context", ui::PopupColor::Text),
            ..ui::PopupContent::default()
        };
        let tab = TabInfo {
            display_area_columns: 100,
            display_area_rows: 20,
            ..TabInfo::default()
        };
        for sidebar_columns in [16, 38, 70, 88] {
            for on_right in [true, false] {
                let pane_x = if on_right { 0 } else { 100 - sidebar_columns };
                let content_x = pane_x + usize::from(!on_right);
                let sidebar = PaneInfo {
                    pane_x,
                    pane_columns: sidebar_columns,
                    pane_content_x: content_x,
                    pane_content_columns: sidebar_columns - 1,
                    ..PaneInfo::default()
                };
                let placement =
                    geometry(&sidebar, sidebar_columns - 1, &tab, 2..=3, &content).unwrap();
                let width = 48.min(101 - sidebar_columns);
                let x = if on_right {
                    sidebar_columns - 1
                } else {
                    pane_x + 1 - width
                };
                assert_eq!(
                    placement.coordinates.x,
                    FloatingPaneCoordinates::default().with_x_fixed(x).x
                );
                assert_eq!(
                    placement.coordinates.width,
                    FloatingPaneCoordinates::default()
                        .with_width_fixed(width)
                        .width
                );
                assert!(x + width <= tab.display_area_columns);
                assert!(x >= content_x + sidebar.pane_content_columns || x + width <= content_x);
                assert!(x == pane_x + sidebar_columns - 1 || x + width == pane_x + 1);
            }
        }
        for (columns, content_x) in [(0, 0), (100, 100)] {
            let sidebar = PaneInfo {
                pane_content_x: content_x,
                pane_content_columns: 20,
                ..PaneInfo::default()
            };
            let tab = TabInfo {
                display_area_columns: columns,
                ..tab.clone()
            };
            assert!(geometry(&sidebar, 20, &tab, 0..=0, &content).is_none());
        }
    }

    #[test]
    fn standalone_outline_clips_every_width_without_open_edges_or_linked_chrome() {
        for columns in 0_usize..=80 {
            for rows in 1..=8 {
                for row in 0..rows {
                    let mut body = row_with_link("日本語 +1234 -567");
                    body.spans[0].text = ui::fit(&body.spans[0].text, columns.saturating_sub(2));
                    let mut terminal = TerminalConsumer::default();
                    terminal.consume(&outlined_line(
                        render_line(&body, columns.saturating_sub(2)),
                        row,
                        rows,
                        columns,
                        &ui::Theme::default(),
                    ));
                    let text = terminal.text();
                    assert_eq!(ui::width(&text), columns);
                    assert!(!text.contains(['├', '┤']));
                    let links = terminal.cell_links();
                    assert!(links.first().is_none_or(Option::is_none));
                    assert!(links.last().is_none_or(Option::is_none));
                    if columns > 0 {
                        assert!(!text.starts_with(' ') && !text.ends_with(' '));
                    }
                    if row == 0 || row + 1 == rows {
                        assert!(!text.contains(' '));
                        assert!(links.iter().all(Option::is_none));
                    }
                    assert!(terminal.link.is_none());
                }
            }
        }
    }

    fn row_with_link(text: &str) -> ui::PopupRow {
        let mut body = row(text, ui::PopupColor::Text);
        body.link = Some("https://example.test/details".to_owned());
        body
    }

    #[test]
    fn source_bands_anchor_popup_vertically_without_changing_its_closed_outline() {
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
        for content_x in [0, 85] {
            let sidebar = PaneInfo {
                pane_x: content_x,
                pane_columns: 15,
                pane_content_x: content_x,
                pane_content_columns: 15,
                pane_content_y: 1,
                ..PaneInfo::default()
            };
            for band in [0..=0, 7..=7, 7..=8, 18..=18] {
                let placement = geometry(&sidebar, 14, &tab, band.clone(), &content).unwrap();
                let height = (wrapped_rows(&content.rows(), 46).len() + 3)
                    .max(band.end() - band.start() + 1);
                let y = (sidebar.pane_content_y + band.start()).min(tab.display_area_rows - height);
                assert_eq!(
                    placement.coordinates.y,
                    FloatingPaneCoordinates::default().with_y_fixed(y).y
                );
                assert_eq!(
                    placement.coordinates.height,
                    FloatingPaneCoordinates::default()
                        .with_height_fixed(height)
                        .height
                );
            }
            assert!(geometry(&sidebar, 14, &tab, 19..=22, &content).is_none());
            assert!(geometry(&sidebar, 14, &tab, 18..=22, &content).is_some());
            let empty = std::ops::RangeInclusive::new(8, 7);
            assert!(geometry(&sidebar, 14, &tab, empty, &content).is_none());
        }
    }
}
