use crate::{overlay, pane_search, ui};
use serde::{Deserialize, Serialize};
use shephrd::Snapshot;
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

pub const UPDATE: &str = "shephrd-finder-update";
pub const READY: &str = "shephrd-finder-ready";
pub const CANCEL: &str = "shephrd-finder-cancel";
pub const SELECTED: &str = "shephrd-finder-selected";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Input {
    Key(KeyWithModifier),
    Paste(String),
}

#[derive(Clone, Serialize, Deserialize)]
struct SequencedInput {
    sequence: u64,
    event: Input,
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    instance: String,
    generation: u64,
    revision: u64,
    visible: bool,
    snapshot: Snapshot,
    coordinates: FloatingPaneCoordinates,
    input_sequence: u64,
    inputs: Vec<SequencedInput>,
}

#[derive(Serialize, Deserialize)]
struct Callback {
    instance: String,
    generation: u64,
    revision: u64,
    input_sequence: u64,
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    restore: bool,
}

pub enum Notice {
    Ready,
    Cancelled { restore: bool },
    Selected { pane_id: String },
}

#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    Ready,
    Behind,
    AlreadyReady,
}

pub struct Controller {
    pub id: Option<u32>,
    instance: String,
    generation: u64,
    revision: u64,
    open_revision: u64,
    ready_generation: Option<u64>,
    launched: bool,
    pending: Option<State>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            id: None,
            instance: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_nanos().to_string())
                .unwrap_or_else(|_| "clock-unavailable".to_owned()),
            generation: 0,
            revision: 0,
            open_revision: 0,
            ready_generation: None,
            launched: false,
            pending: None,
        }
    }
}

impl Controller {
    pub fn open(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        snapshot: &Snapshot,
        columns: usize,
        rows: usize,
    ) {
        if self.is_open() {
            self.update(owner, configuration, snapshot, columns, rows);
            return;
        }
        self.generation = self.generation.saturating_add(1);
        self.revision = self.revision.saturating_add(1);
        self.open_revision = self.revision;
        self.ready_generation = None;
        self.pending = Some(State {
            instance: self.instance.clone(),
            generation: self.generation,
            revision: self.revision,
            visible: true,
            snapshot: snapshot.clone(),
            coordinates: geometry(columns, rows),
            input_sequence: 0,
            inputs: Vec::new(),
        });
        self.send(owner, configuration);
    }

    pub fn update(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        snapshot: &Snapshot,
        columns: usize,
        rows: usize,
    ) {
        let Some(state) = self.pending.as_mut().filter(|state| state.visible) else {
            return;
        };
        let coordinates = geometry(columns, rows);
        let data_changed = !same_data(&state.snapshot, snapshot);
        if !data_changed && state.coordinates == coordinates {
            return;
        }
        self.revision = self.revision.saturating_add(1);
        state.revision = self.revision;
        if data_changed {
            state.snapshot = snapshot.clone();
        }
        state.coordinates = coordinates;
        self.send(owner, configuration);
    }

    pub fn input(&mut self, input: Input, owner: u32, configuration: &BTreeMap<String, String>) {
        if self.queue_input(input) {
            self.send(owner, configuration);
        }
    }

    fn queue_input(&mut self, input: Input) -> bool {
        let Some(state) = self.pending.as_mut().filter(|state| state.visible) else {
            return false;
        };
        state.input_sequence = state.input_sequence.saturating_add(1);
        state.inputs.push(SequencedInput {
            sequence: state.input_sequence,
            event: input,
        });
        self.revision = self.revision.saturating_add(1);
        state.revision = self.revision;
        true
    }

    pub fn pipe(
        &mut self,
        message: &PipeMessage,
        owner: u32,
        configuration: &BTreeMap<String, String>,
    ) -> Option<Notice> {
        if !matches!(message.name.as_str(), READY | CANCEL | SELECTED) {
            return None;
        }
        let PipeSource::Plugin(source) = message.source else {
            return None;
        };
        let payload = message.payload.as_deref()?;
        if payload.len() > 8192 {
            return None;
        }
        let callback: Callback = serde_json::from_str(payload).ok()?;
        if !self.valid_callback(source, &callback) {
            return None;
        }
        match message.name.as_str() {
            READY => match self.acknowledge_ready(source, &callback) {
                Readiness::Behind => {
                    self.send(owner, configuration);
                    None
                }
                Readiness::AlreadyReady => None,
                Readiness::Ready => Some(Notice::Ready),
            },
            CANCEL => {
                self.close(owner, configuration);
                Some(Notice::Cancelled {
                    restore: callback.restore,
                })
            }
            SELECTED => {
                let pane_id = callback.pane_id?;
                self.close(owner, configuration);
                Some(Notice::Selected { pane_id })
            }
            _ => None,
        }
    }

    fn acknowledge_ready(&mut self, source: u32, callback: &Callback) -> Readiness {
        self.id = Some(source);
        self.launched = true;
        let state = self.pending.as_mut().expect("validated callback state");
        state
            .inputs
            .retain(|input| input.sequence > callback.input_sequence);
        if callback.input_sequence < state.input_sequence {
            return Readiness::Behind;
        }
        if self.ready_generation == Some(callback.generation) {
            return Readiness::AlreadyReady;
        }
        self.ready_generation = Some(callback.generation);
        Readiness::Ready
    }

    fn valid_callback(&self, source: u32, callback: &Callback) -> bool {
        self.launched
            && self.id.is_none_or(|id| id == source)
            && self.pending.as_ref().is_some_and(|state| {
                state.visible
                    && callback.instance == state.instance
                    && callback.generation == state.generation
                    && callback.revision >= self.open_revision
                    && callback.revision <= state.revision
                    && callback.input_sequence <= state.input_sequence
                    && callback
                        .pane_id
                        .as_ref()
                        .is_none_or(|id| state.snapshot.panes.iter().any(|pane| &pane.id == id))
            })
    }

    pub fn close(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        let Some(state) = self.pending.as_mut().filter(|state| state.visible) else {
            return;
        };
        self.revision = self.revision.saturating_add(1);
        state.revision = self.revision;
        state.visible = false;
        self.ready_generation = None;
        // Keep the instance, including an in-flight launch, reusable and explicitly hidden.
        self.send(owner, configuration);
    }

    pub fn is_open(&self) -> bool {
        self.pending.as_ref().is_some_and(|state| state.visible)
    }

    pub fn forget(&mut self, id: u32) {
        if self.id == Some(id) {
            self.id = None;
            self.launched = false;
            self.ready_generation = None;
            self.pending = None;
        }
    }

    fn send(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        let Some(state) = &self.pending else { return };
        let Ok(payload) = serde_json::to_string(state) else {
            return;
        };
        let mut message = MessageToPlugin::new(UPDATE).with_payload(payload);
        if let Some(id) = self.id {
            message.destination_plugin_id = Some(id);
        } else {
            let Some(url) = configuration.get("plugin_url") else {
                return;
            };
            if !state.visible && !self.launched {
                return;
            }
            let mut config = configuration.clone();
            config.insert("role".to_owned(), "finder".to_owned());
            config.insert("owner".to_owned(), owner.to_string());
            // Stable across opens: exact URL/config identity prevents duplicate launches.
            config.insert("finder_instance".to_owned(), self.instance.clone());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("shephrd pane finder".to_owned()),
                ..NewPluginArgs::default()
            });
            message.floating_pane_coordinates = Some(state.coordinates.clone());
            self.launched = true;
        }
        pipe_message_to_plugin(message);
    }
}

fn same_data(left: &Snapshot, right: &Snapshot) -> bool {
    left.instance_id == right.instance_id
        && left.panes == right.panes
        && left.workspaces == right.workspaces
}

fn geometry(columns: usize, rows: usize) -> FloatingPaneCoordinates {
    let width = (columns.saturating_mul(4) / 5)
        .clamp(1, 140)
        .min(columns.max(1));
    let height = (rows.saturating_mul(3) / 4).clamp(1, 36).min(rows.max(1));
    FloatingPaneCoordinates {
        pinned: Some(true),
        borderless: Some(true),
        ..FloatingPaneCoordinates::default()
            .with_x_fixed(columns.saturating_sub(width) / 2)
            .with_y_fixed(rows.saturating_sub(height) / 2)
            .with_width_fixed(width)
            .with_height_fixed(height)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Rect {
    fn contains(self, row: isize, column: usize) -> bool {
        row >= 0
            && (self.y..self.y + self.height).contains(&(row as usize))
            && (self.x..self.x + self.width).contains(&column)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Compact {
    #[default]
    Input,
    List,
    Preview,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Layout {
    input: Rect,
    list: Rect,
    preview: Rect,
    search: Option<Rect>,
    vertical: Option<Rect>,
    horizontal: Option<Rect>,
    border: bool,
}

impl Layout {
    fn new(columns: usize, rows: usize, compact: Compact) -> Self {
        if columns == 0 || rows == 0 {
            return Self::default();
        }
        if rows <= 2 {
            let mut result = Self::default();
            if rows == 2 || compact == Compact::Input {
                result.input = Rect {
                    x: 0,
                    y: 0,
                    width: columns,
                    height: 1,
                };
            }
            let body = Rect {
                x: 0,
                y: rows - 1,
                width: columns,
                height: 1,
            };
            match compact {
                Compact::Preview => result.preview = body,
                _ if rows == 2 || compact == Compact::List => result.list = body,
                _ => {}
            }
            return result;
        }
        let border = rows >= 7 && columns >= 4;
        let margin = usize::from(border);
        let width = columns.saturating_sub(margin * 2);
        let input = Rect {
            x: margin,
            y: margin,
            width,
            height: 1,
        };
        // Keep input, list and preview reachable in three rows; larger views can
        // spend one row on a search rule without stealing either body surface.
        let search = (rows >= 4).then_some(Rect {
            x: 0,
            y: margin + 1,
            width: columns,
            height: 1,
        });
        let top = margin + 1 + usize::from(search.is_some());
        let bottom = rows - margin;
        let body = bottom.saturating_sub(top);
        let mut result = Self {
            input,
            search,
            border,
            ..Self::default()
        };
        if width >= 84 && body >= 2 {
            let list_width = width * 2 / 5;
            result.list = Rect {
                x: margin,
                y: top,
                width: list_width,
                height: body,
            };
            result.vertical = Some(Rect {
                x: margin + list_width,
                y: top,
                width: 1,
                height: body,
            });
            result.preview = Rect {
                x: margin + list_width + 1,
                y: top,
                width: width - list_width - 1,
                height: body,
            };
        } else {
            let separator = usize::from(body >= 3);
            let list_height = ((body - separator) * 2 / 5).max(1).min(body);
            result.list = Rect {
                x: margin,
                y: top,
                width,
                height: list_height,
            };
            if separator != 0 {
                result.horizontal = Some(Rect {
                    x: 0,
                    y: top + list_height,
                    width: columns,
                    height: 1,
                });
            }
            result.preview = Rect {
                x: margin,
                y: top + list_height + separator,
                width,
                height: body.saturating_sub(list_height + separator),
            };
        }
        result
    }

    fn rule_line(self, separator: Rect) -> String {
        let junction = (self.search == Some(separator))
            .then_some(self.vertical)
            .flatten()
            .map(|vertical| vertical.x);
        let mut line = String::with_capacity(separator.width * 3);
        for column in separator.x..separator.x + separator.width {
            line.push(if self.border && column == separator.x {
                '├'
            } else if self.border && column + 1 == separator.x + separator.width {
                '┤'
            } else if junction == Some(column) {
                '┬'
            } else {
                '─'
            });
        }
        line
    }

    fn bottom_line(self, columns: usize) -> String {
        let mut line = String::with_capacity(columns * 3);
        for column in 0..columns {
            line.push(if column == 0 {
                '╰'
            } else if column + 1 == columns {
                '╯'
            } else if self.vertical.is_some_and(|vertical| vertical.x == column) {
                '┴'
            } else {
                '─'
            });
        }
        line
    }
}

#[derive(Default)]
pub struct Finder {
    owner: Option<u32>,
    id: u32,
    instance: String,
    granted: bool,
    owner_seen: bool,
    owner_tab: Option<usize>,
    active_tab: Option<usize>,
    had_focus: bool,
    pane_focused: bool,
    floating_visible: bool,
    dismissed: bool,
    ready: Option<(u64, u64)>,
    state: Option<State>,
    applied_sequence: u64,
    pending_action: Option<KeyAction>,
    index: pane_search::Index,
    input: ui::Input,
    selected: Option<String>,
    selection: usize,
    scroll: usize,
    preview: ui::PopupContent,
    preview_lines: Vec<overlay::PopupLine>,
    preview_columns: Option<usize>,
    preview_scroll: usize,
    pointer_preview: bool,
    compact: Compact,
    layout: Layout,
    cursor: Option<Option<(usize, usize)>>,
    applied_coordinates: Option<FloatingPaneCoordinates>,
    shown: Option<bool>,
}

impl Finder {
    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32) {
        self.id = id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        self.instance = configuration
            .get("finder_instance")
            .cloned()
            .unwrap_or_default();
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::TabUpdate,
            EventType::Key,
            EventType::PastedText,
            EventType::Mouse,
            EventType::BeforeClose,
        ]);
        request_permission(super::PERMISSIONS);
    }

    fn reconcile_focus(&mut self) {
        if !self.visible() {
            return;
        }
        let focused = self.pane_focused && self.floating_visible;
        if self
            .owner_tab
            .zip(self.active_tab)
            .is_some_and(|(owner, active)| owner != active)
            || (self.had_focus && !focused)
        {
            self.finish(CANCEL, None, false);
        } else if focused {
            self.had_focus = true;
        }
    }

    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
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
            Event::PermissionRequestResult(PermissionStatus::Denied) => close_self(),
            Event::PaneUpdate(manifest) => {
                let owner_tab = manifest.panes.iter().find_map(|(tab, panes)| {
                    panes
                        .iter()
                        .any(|pane| pane.is_plugin && Some(pane.id) == self.owner)
                        .then_some(*tab)
                });
                if let Some(tab) = owner_tab {
                    self.owner_seen = true;
                    self.owner_tab = Some(tab);
                } else if self.owner_seen && self.granted {
                    close_self();
                    return false;
                }
                self.pane_focused = manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| pane.is_plugin && pane.id == self.id && pane.is_focused);
                self.reconcile_focus();
            }
            Event::TabUpdate(tabs) => {
                let active = tabs.iter().find(|tab| tab.active);
                self.active_tab = active.map(|tab| tab.position);
                self.floating_visible = active.is_some_and(|tab| tab.are_floating_panes_visible);
                self.reconcile_focus();
            }
            Event::PaneClosed(PaneId::Plugin(id)) if Some(id) == self.owner && self.granted => {
                close_self();
            }
            Event::BeforeClose if self.granted && self.visible() => {
                self.callback(CANCEL, None, false);
                self.dismissed = true;
            }
            Event::Key(key) if self.granted && self.visible() => {
                let action = self.input_event(Input::Key(key));
                self.dispatch(action);
            }
            Event::PastedText(text) if self.granted && self.visible() => {
                self.input_event(Input::Paste(text));
            }
            Event::Mouse(mouse) if self.granted && self.visible() => self.mouse(mouse),
            _ => return false,
        }
        true
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if let PipeSource::Cli(pipe) = &message.source {
            // Never strand an unexpected CLI sender, including malformed/bootstrap messages.
            unblock_cli_pipe_input(pipe);
            return false;
        }
        if message.name != UPDATE
            || !matches!(message.source, PipeSource::Plugin(id) if Some(id) == self.owner)
        {
            return false;
        }
        let Some(payload) = message.payload else {
            return false;
        };
        if payload.len() > shephrd::MAX_MESSAGE_BYTES * 2 {
            return false;
        }
        let Ok(state) = serde_json::from_str::<State>(&payload) else {
            return false;
        };
        if !self.receive(state) {
            return false;
        }
        self.apply();
        true
    }

    fn receive(&mut self, mut state: State) -> bool {
        if state.instance != self.instance
            || self
                .state
                .as_ref()
                .is_some_and(|current| state.revision <= current.revision)
        {
            return false;
        }
        let opening = state.visible
            && self
                .state
                .as_ref()
                .is_none_or(|current| !current.visible || current.generation != state.generation);
        let data_changed = self
            .state
            .as_ref()
            .is_none_or(|current| !same_data(&current.snapshot, &state.snapshot));
        if opening {
            self.input = ui::Input::default();
            self.selected = state.snapshot.focused_pane_id.clone();
            self.selection = 0;
            self.scroll = 0;
            self.preview_scroll = 0;
            self.compact = Compact::Input;
            self.had_focus = false;
            self.pane_focused = false;
            self.dismissed = false;
            self.ready = None;
            self.applied_sequence = 0;
            self.pending_action = None;
        }
        if data_changed {
            self.index.rebuild(&state.snapshot);
        }
        if let (Some(columns), Some(rows)) = (state.coordinates.width, state.coordinates.height) {
            self.layout = Layout::new(columns.to_fixed(0), rows.to_fixed(0), self.compact);
        }
        let inputs = std::mem::take(&mut state.inputs);
        self.state = Some(state);
        if data_changed || opening {
            self.search();
        }
        if self.visible() {
            for input in inputs {
                if input.sequence <= self.applied_sequence {
                    continue;
                }
                if self.pending_action.is_some() || input.sequence != self.applied_sequence + 1 {
                    break;
                }
                let action = self.input_event(input.event);
                self.applied_sequence = input.sequence;
                if action != KeyAction::None {
                    self.pending_action = Some(action);
                }
            }
        }
        true
    }

    fn visible(&self) -> bool {
        !self.dismissed && self.state.as_ref().is_some_and(|state| state.visible)
    }

    fn apply(&mut self) {
        if !self.granted {
            return;
        }
        if !self.visible() {
            if self.shown != Some(false) {
                set_selectable(false);
                self.set_cursor(None);
                hide_self();
                self.shown = Some(false);
            }
            return;
        }
        if let Some(action) = self.pending_action.take() {
            self.dispatch(action);
            if !self.visible() {
                return;
            }
        }
        let state = self.state.as_ref().expect("visible state");
        let pane = PaneId::Plugin(self.id);
        // Restoring a suppressed pane replaces its native floating geometry.
        if self.shown != Some(true) || self.applied_coordinates.as_ref() != Some(&state.coordinates)
        {
            change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
            self.applied_coordinates = Some(state.coordinates.clone());
        }
        if self.shown != Some(true) {
            set_selectable(true);
            // Readiness never steals focus; only the owner's valid READY handoff does.
            show_pane_with_id(pane, true, false);
            self.shown = Some(true);
        }
        let revision = (state.generation, state.revision);
        if self.ready != Some(revision) {
            self.callback(READY, None, false);
            self.ready = Some(revision);
        }
    }

    fn callback(&self, name: &str, pane_id: Option<String>, restore: bool) {
        let (Some(owner), Some(state)) = (self.owner, &self.state) else {
            return;
        };
        let callback = Callback {
            instance: state.instance.clone(),
            generation: state.generation,
            revision: state.revision,
            input_sequence: self.applied_sequence,
            pane_id,
            restore,
        };
        if let Ok(payload) = serde_json::to_string(&callback) {
            pipe_message_to_plugin(
                MessageToPlugin::new(name)
                    .with_destination_plugin_id(owner)
                    .with_payload(payload),
            );
        }
    }

    fn finish(&mut self, name: &str, pane_id: Option<String>, restore: bool) {
        if !self.visible() {
            return;
        }
        self.callback(name, pane_id, restore);
        self.dismissed = true;
        self.had_focus = false;
        set_selectable(false);
        self.set_cursor(None);
        hide_self();
        self.shown = Some(false);
    }

    fn input_event(&mut self, input: Input) -> KeyAction {
        match input {
            Input::Key(key) => self.key(key),
            Input::Paste(text) => {
                let before = self.input.value.len();
                self.input.insert(&text);
                self.compact = Compact::Input;
                if self.input.value.len() != before {
                    self.selection = 0;
                    self.selected = None;
                    self.scroll = 0;
                    self.search();
                }
                KeyAction::None
            }
        }
    }

    fn dispatch(&mut self, action: KeyAction) {
        match action {
            KeyAction::Cancel => self.finish(CANCEL, None, true),
            KeyAction::Select => {
                if let Some(pane) = self.selected.clone() {
                    self.finish(SELECTED, Some(pane), false);
                }
            }
            KeyAction::None => {}
        }
    }

    fn search(&mut self) {
        self.index.search(&self.input.value);
        let Some(state) = &self.state else { return };
        self.selection = self
            .selected
            .as_ref()
            .and_then(|id| {
                self.index
                    .results()
                    .iter()
                    .position(|result| state.snapshot.panes[result.pane].id == *id)
            })
            .unwrap_or_else(|| {
                self.selection
                    .min(self.index.results().len().saturating_sub(1))
            });
        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        let Some(state) = &self.state else { return };
        let selected = self
            .index
            .results()
            .get(self.selection)
            .map(|result| state.snapshot.panes[result.pane].id.clone());
        if selected != self.selected {
            self.preview_scroll = 0;
        }
        self.selected = selected;
        let content = self
            .selected
            .as_ref()
            .map(|id| ui::tooltip(&ui::Item::Pane(id.clone()), &state.snapshot))
            .unwrap_or_default();
        if content != self.preview {
            self.preview = content;
            self.preview_columns = None;
        }
        self.ensure_selection_visible();
    }

    fn ensure_selection_visible(&mut self) {
        let height = self.layout.list.height.max(1);
        if self.selection < self.scroll {
            self.scroll = self.selection;
        } else if self.selection >= self.scroll.saturating_add(height) {
            self.scroll = self.selection.saturating_add(1).saturating_sub(height);
        }
        self.scroll = self
            .scroll
            .min(self.index.results().len().saturating_sub(height));
    }

    fn navigate(&mut self, delta: isize) {
        self.selection = self
            .selection
            .saturating_add_signed(delta)
            .min(self.index.results().len().saturating_sub(1));
        if self.layout.input.height == 0 || self.layout.list.height == 0 {
            self.compact = Compact::List;
        }
        self.sync_selection();
    }

    fn scroll_preview(&mut self, delta: isize) {
        self.prepare_preview();
        self.preview_scroll = self.preview_scroll.saturating_add_signed(delta).min(
            self.preview_lines
                .len()
                .saturating_sub(self.layout.preview.height.max(1)),
        );
    }

    fn prepare_preview(&mut self) {
        let preview = self.layout.preview;
        if self.preview_columns != Some(preview.width) {
            let mut rows = self.preview.rows();
            for row in &mut rows {
                if row.refresh.take().is_some() {
                    row.right.clear();
                }
            }
            self.preview_lines = overlay::wrapped_rows(&rows, preview.width);
            self.preview_columns = Some(preview.width);
        }
        self.preview_scroll = self
            .preview_scroll
            .min(self.preview_lines.len().saturating_sub(preview.height));
    }

    fn key(&mut self, key: KeyWithModifier) -> KeyAction {
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        if key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return KeyAction::None;
        }
        if key.bare_key == BareKey::Esc {
            return KeyAction::Cancel;
        }
        if key.bare_key == BareKey::Enter {
            return if self.selected.is_some() {
                KeyAction::Select
            } else {
                KeyAction::None
            };
        }
        match key.bare_key {
            BareKey::Up if !ctrl => self.navigate(-1),
            BareKey::Down if !ctrl => self.navigate(1),
            BareKey::Char('p' | 'k') if ctrl => self.navigate(-1),
            BareKey::Char('n' | 'j') if ctrl => self.navigate(1),
            BareKey::PageUp | BareKey::PageDown => {
                let page = self.layout.preview.height.max(1) as isize;
                self.scroll_preview(if key.bare_key == BareKey::PageUp {
                    -page
                } else {
                    page
                });
                if self.layout.preview.height == 0 {
                    self.compact = Compact::Preview;
                }
            }
            BareKey::Tab => {
                self.compact = match self.compact {
                    Compact::Input => Compact::List,
                    Compact::List => Compact::Preview,
                    Compact::Preview => Compact::Input,
                };
                self.pointer_preview = self.compact == Compact::Preview;
            }
            _ => {
                let before = self.input.value.len();
                match key.bare_key {
                    BareKey::Char('u') if ctrl => {
                        self.input.value.clear();
                        self.input.cursor = 0;
                    }
                    BareKey::Char(character) if !ctrl => self.input.insert(&character.to_string()),
                    BareKey::Backspace if !ctrl => self.input.backspace(),
                    BareKey::Delete if !ctrl => self.input.delete(),
                    BareKey::Left if !ctrl => self.input.left(),
                    BareKey::Right if !ctrl => self.input.right(),
                    BareKey::Home if !ctrl => self.input.cursor = 0,
                    BareKey::End if !ctrl => self.input.cursor = self.input.value.len(),
                    _ => return KeyAction::None,
                }
                self.compact = Compact::Input;
                if self.input.value.len() != before {
                    self.selection = 0;
                    self.selected = None;
                    self.scroll = 0;
                    self.search();
                }
            }
        }
        KeyAction::None
    }

    fn mouse(&mut self, mouse: Mouse) {
        match mouse {
            Mouse::LeftClick(-1, column) | Mouse::RightClick(-1, column)
                if column == u16::MAX as usize =>
            {
                self.finish(CANCEL, None, false)
            }
            Mouse::Hover(row, column) | Mouse::Hold(row, column) => {
                self.pointer_preview = self.layout.preview.contains(row, column);
            }
            Mouse::LeftClick(row, column) if self.layout.list.contains(row, column) => {
                let selected = self.scroll + row as usize - self.layout.list.y;
                if selected < self.index.results().len() {
                    self.selection = selected;
                    self.pointer_preview = false;
                    self.sync_selection();
                }
            }
            Mouse::LeftClick(row, column) if self.layout.preview.contains(row, column) => {
                // OSC8 body glyphs retain their real links; the terminal owns opening them.
                self.pointer_preview = true;
            }
            Mouse::ScrollUp(lines) => {
                let delta = -(lines.min(isize::MAX as usize) as isize);
                if self.pointer_preview {
                    self.scroll_preview(delta)
                } else {
                    self.navigate(delta)
                }
            }
            Mouse::ScrollDown(lines) => {
                let delta = lines.min(isize::MAX as usize) as isize;
                if self.pointer_preview {
                    self.scroll_preview(delta)
                } else {
                    self.navigate(delta)
                }
            }
            _ => {}
        }
    }

    fn set_cursor(&mut self, cursor: Option<(usize, usize)>) {
        if self.cursor != Some(cursor) {
            show_cursor(cursor);
            self.cursor = Some(cursor);
        }
    }

    pub fn render(&mut self, rows: usize, columns: usize) {
        if !self.granted || !self.visible() {
            return;
        }
        self.layout = Layout::new(columns, rows, self.compact);
        self.ensure_selection_visible();
        let preview = self.layout.preview;
        self.prepare_preview();
        let input = self.layout.input;
        let prefix = if input.width >= 3 { "> " } else { "" };
        let input_width = input.width.saturating_sub(ui::width(prefix));
        let (query, cursor) = self.input.visible(input_width);
        // Host commands precede all render ANSI; the parent flushes this frame afterwards.
        self.set_cursor(
            (input.height > 0 && input_width > 0)
                .then_some((input.x + ui::width(prefix) + cursor, input.y)),
        );
        let plain = if self.layout.border {
            String::new()
        } else {
            " ".repeat(columns)
        };
        let frame = [0, 1, rows.saturating_sub(1)].map(|row| {
            if self.layout.border && row + 1 == rows {
                format!("{}{}", ui::BRIGHT, self.layout.bottom_line(columns))
            } else if self.layout.border {
                overlay::outlined_line(
                    " ".repeat(columns.saturating_sub(2)),
                    row,
                    rows,
                    columns,
                    None,
                )
            } else {
                String::new()
            }
        });
        for row in 0..rows {
            let line = if self.layout.border {
                &frame[if row == 0 {
                    0
                } else if row + 1 == rows {
                    2
                } else {
                    1
                }]
            } else {
                &plain
            };
            print!(
                "\u{1b}[{};1H{}{}{line}{}",
                row + 1,
                ui::RESET,
                ui::BASE,
                ui::RESET
            );
        }
        if self.layout.border {
            let count = self.index.results().len();
            let total = self
                .state
                .as_ref()
                .map_or(0, |state| state.snapshot.panes.len());
            let title = format!(" Pane finder · {count}/{total} ");
            paint(
                2,
                0,
                &format!(
                    "{}{}",
                    ui::BRIGHT,
                    ui::truncate(&title, columns.saturating_sub(4))
                ),
            );
        }
        if input.height > 0 {
            paint(input.x, input.y, &format!("{}{prefix}{query}", ui::BRIGHT));
        }
        if let Some(separator) = self.layout.search {
            paint(
                separator.x,
                separator.y,
                &format!("{}{}", ui::BRIGHT, self.layout.rule_line(separator)),
            );
        }
        if let Some(separator) = self.layout.vertical {
            let line = format!("{}│", ui::BRIGHT);
            for row in separator.y..separator.y + separator.height {
                paint(separator.x, row, &line);
            }
        }
        if let Some(separator) = self.layout.horizontal {
            paint(
                separator.x,
                separator.y,
                &format!("{}{}", ui::BRIGHT, self.layout.rule_line(separator)),
            );
        }
        let list = self.layout.list;
        for row in 0..list.height {
            let index = self.scroll + row;
            let text = self.result_line(index, list.width);
            let background = if index == self.selection && !self.index.results().is_empty() {
                ui::Theme::default().selected
            } else {
                ""
            };
            paint(list.x, list.y + row, &format!("{background}{text}"));
        }
        for row in 0..preview.height {
            let text = self
                .preview_lines
                .get(row + self.preview_scroll)
                .map(|line| overlay::render_action_line(&line.row, preview.width, false))
                .unwrap_or_else(|| {
                    ui::fit(
                        if row == 0 && self.selected.is_none() {
                            "No pane selected"
                        } else {
                            ""
                        },
                        preview.width,
                    )
                });
            paint(preview.x, preview.y + row, &text);
        }
        print!("{}{}", overlay::OSC8_CLOSE, ui::RESET);
    }

    fn result_line(&mut self, index: usize, columns: usize) -> String {
        let Some(state) = &self.state else {
            return " ".repeat(columns);
        };
        if self.index.results().get(index).is_none() {
            return ui::fit(
                if index == 0 && self.index.results().is_empty() {
                    "No matching panes"
                } else {
                    ""
                },
                columns,
            );
        }
        crate::finder_entry::render(
            &mut self.index,
            &state.snapshot,
            index,
            index == self.selection,
            columns,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyAction {
    None,
    Cancel,
    Select,
}

fn paint(x: usize, y: usize, text: &str) {
    print!(
        "{}\u{1b}[{};{}H{}{}{text}{}{}",
        overlay::OSC8_CLOSE,
        y + 1,
        x + 1,
        ui::RESET,
        ui::BASE,
        overlay::OSC8_CLOSE,
        ui::RESET
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use shephrd::{Pane, Status, Workspace};
    use std::collections::BTreeSet;

    fn snapshot() -> Snapshot {
        Snapshot {
            protocol: shephrd::PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision: 1,
            session: "test".to_owned(),
            connected: true,
            error: None,
            focused_pane_id: Some("pane-2".to_owned()),
            workspaces: vec![Workspace {
                id: "workspace".to_owned(),
                name: "Project workspace".to_owned(),
                active_tab_id: "tab".to_owned(),
                projects: Vec::new(),
                status: Status::Idle,
                attention: None,
            }],
            panes: (1..=3)
                .map(|index| Pane {
                    id: format!("pane-{index}"),
                    terminal_id: format!("terminal_{index}"),
                    workspace_id: "workspace".to_owned(),
                    tab_id: "tab".to_owned(),
                    name: format!("Terminal {index}"),
                    description: format!("Working on feature {index}"),
                    harness: None,
                    status: Status::Idle,
                    attention: None,
                    cwd: Some(format!("/project/{index}")),
                    branch: Some(format!("feature/{index}")),
                    branch_changes: None,
                    project: None,
                    ci: None,
                })
                .collect(),
        }
    }

    pub(super) fn state(generation: u64, revision: u64) -> State {
        State {
            instance: "finder:test".to_owned(),
            generation,
            revision,
            visible: true,
            snapshot: snapshot(),
            coordinates: geometry(160, 48),
            input_sequence: 0,
            inputs: Vec::new(),
        }
    }

    fn finder() -> Finder {
        let mut finder = Finder {
            instance: "finder:test".to_owned(),
            layout: Layout::new(100, 28, Compact::Input),
            ..Finder::default()
        };
        assert!(finder.receive(state(1, 1)));
        finder
    }

    #[test]
    fn hidden_floating_layer_dismisses_after_focus_but_not_during_startup() {
        let mut finder = finder();
        finder.owner = Some(7);
        finder.id = 8;
        let manifest = PaneManifest {
            panes: std::collections::HashMap::from([(
                0,
                vec![
                    PaneInfo {
                        id: 7,
                        is_plugin: true,
                        ..PaneInfo::default()
                    },
                    PaneInfo {
                        id: 8,
                        is_plugin: true,
                        is_floating: true,
                        is_focused: true,
                        ..PaneInfo::default()
                    },
                ],
            )]),
        };
        finder.event(Event::PaneUpdate(manifest));
        let mut tab = TabInfo {
            active: true,
            ..TabInfo::default()
        };
        finder.event(Event::TabUpdate(vec![tab.clone()]));
        assert!(finder.visible());
        tab.are_floating_panes_visible = true;
        finder.event(Event::TabUpdate(vec![tab.clone()]));
        assert!(finder.visible());
        // Pinned panes retain is_focused even when clicking a tiled pane hides the layer.
        tab.are_floating_panes_visible = false;
        finder.event(Event::TabUpdate(vec![tab]));
        assert!(!finder.visible());
    }

    fn key(bare_key: BareKey) -> KeyWithModifier {
        KeyWithModifier::new(bare_key)
    }

    fn ctrl(character: char) -> KeyWithModifier {
        KeyWithModifier {
            bare_key: BareKey::Char(character),
            key_modifiers: BTreeSet::from([KeyModifier::Ctrl]),
        }
    }

    #[test]
    fn modal_geometry_is_centered_capped_and_clamped_to_the_display() {
        for columns in [1, 2, 20, 80, 175, 300] {
            for rows in [1, 2, 5, 24, 48, 100] {
                let coordinates = geometry(columns, rows);
                let x = coordinates.x.unwrap().to_position(columns);
                let y = coordinates.y.unwrap().to_position(rows);
                let width = coordinates.width.unwrap().to_position(columns);
                let height = coordinates.height.unwrap().to_position(rows);
                assert!((1..=columns.min(140)).contains(&width));
                assert!((1..=rows.min(36)).contains(&height));
                assert!(x + width <= columns && y + height <= rows);
                assert!((columns - width).abs_diff(x * 2) <= 1);
                assert!((rows - height).abs_diff(y * 2) <= 1);
            }
        }
    }

    #[test]
    fn adaptive_surfaces_stay_disjoint_and_inside_every_small_viewport() {
        for columns in 0..=150 {
            for rows in 0..=40 {
                for compact in [Compact::Input, Compact::List, Compact::Preview] {
                    let layout = Layout::new(columns, rows, compact);
                    let rects = [
                        layout.input,
                        layout.list,
                        layout.preview,
                        layout.search.unwrap_or_default(),
                        layout.vertical.unwrap_or_default(),
                        layout.horizontal.unwrap_or_default(),
                    ];
                    for rect in rects {
                        assert!(rect.x + rect.width <= columns);
                        assert!(rect.y + rect.height <= rows);
                        if layout.border
                            && [layout.input, layout.list, layout.preview].contains(&rect)
                        {
                            assert!(rect.x >= 1 && rect.x + rect.width < columns);
                            assert!(rect.y >= 1 && rect.y + rect.height < rows);
                        }
                    }
                    for (index, left) in rects.iter().enumerate() {
                        for right in &rects[index + 1..] {
                            if left.width * left.height == 0 || right.width * right.height == 0 {
                                continue;
                            }
                            assert!(
                                left.x + left.width <= right.x
                                    || right.x + right.width <= left.x
                                    || left.y + left.height <= right.y
                                    || right.y + right.height <= left.y
                            );
                        }
                    }
                    if columns > 0 && rows >= 3 {
                        assert!(layout.input.height > 0);
                        assert!(layout.list.height > 0);
                        assert!(layout.preview.height > 0);
                    }
                    if let Some(rule) = layout.search {
                        assert_eq!(rule.y, layout.input.y + layout.input.height);
                        assert_eq!(rule.width, columns);
                        assert_eq!(ui::width(&layout.rule_line(rule)), columns);
                        assert_eq!(layout.list.y, rule.y + 1);
                    }
                    if let Some(rule) = layout.horizontal {
                        assert_eq!(rule.width, columns);
                        assert_eq!(ui::width(&layout.rule_line(rule)), columns);
                    }
                    if layout.border {
                        assert_eq!(ui::width(&layout.bottom_line(columns)), columns);
                        assert_eq!(layout.preview.y + layout.preview.height, rows - 1);
                    }
                }
            }
        }
        let wide = Layout::new(110, 25, Compact::Input);
        assert_eq!(wide.list.y, wide.preview.y);
        assert!(wide.list.x + wide.list.width < wide.preview.x);
        let narrow = Layout::new(60, 25, Compact::Input);
        assert!(narrow.list.y + narrow.list.height < narrow.preview.y);
    }

    #[test]
    fn wide_rules_join_the_frame_without_crossing_the_full_width_input() {
        for rows in [7, 8, 25, 40] {
            let columns = 110;
            let layout = Layout::new(columns, rows, Compact::Input);
            let search = layout.search.unwrap();
            let vertical = layout.vertical.unwrap();
            let top = layout.rule_line(search);
            let bottom = layout.bottom_line(columns);
            assert!(top.starts_with('├') && top.ends_with('┤'));
            assert!(bottom.starts_with('╰') && bottom.ends_with('╯'));
            assert_eq!(top.chars().nth(vertical.x), Some('┬'));
            assert_eq!(bottom.chars().nth(vertical.x), Some('┴'));
            assert_eq!(top.matches('┬').count(), 1);
            assert_eq!(bottom.matches('┴').count(), 1);
            assert_eq!(vertical.y, search.y + 1);
            assert_eq!(vertical.y + vertical.height, rows - 1);
            assert_eq!(layout.input.x, 1);
            assert_eq!(layout.input.width, columns - 2);
            assert!(layout.input.y + layout.input.height <= search.y);
            assert_eq!(layout.list.height, rows - 4);
            assert_eq!(layout.preview.height, rows - 4);
            assert!(layout.horizontal.is_none());
        }
    }

    #[test]
    fn narrow_rules_join_both_sides_and_body_uses_the_last_interior_row() {
        for rows in [7, 8, 25, 40] {
            let columns = 60;
            let layout = Layout::new(columns, rows, Compact::Input);
            assert!(layout.vertical.is_none());
            for separator in [layout.search.unwrap(), layout.horizontal.unwrap()] {
                let line = layout.rule_line(separator);
                assert_eq!(line, format!("├{}┤", "─".repeat(columns - 2)));
            }
            let middle = layout.horizontal.unwrap();
            assert_eq!(middle.y, layout.list.y + layout.list.height);
            assert_eq!(layout.preview.y, middle.y + 1);
            assert_eq!(layout.preview.y + layout.preview.height, rows - 1);
            assert_eq!(layout.list.height + layout.preview.height + 1, rows - 4);
            assert_eq!(
                layout.bottom_line(columns),
                format!("╰{}╯", "─".repeat(columns - 2))
            );
        }
    }

    #[test]
    fn cursor_remains_in_the_input_surface_at_every_tiny_viewport() {
        let mut finder = finder();
        for character in "α界e\u{301} a long query".chars() {
            finder.key(key(BareKey::Char(character)));
        }
        for columns in 0..=12 {
            for rows in 0..=8 {
                for compact in [Compact::Input, Compact::List, Compact::Preview] {
                    let layout = Layout::new(columns, rows, compact);
                    let input = layout.input;
                    let prefix_width = if input.width >= 3 { 2 } else { 0 };
                    let input_width = input.width.saturating_sub(prefix_width);
                    let (query, offset) = finder.input.visible(input_width);
                    assert_eq!(ui::width(&query), input_width);
                    if input.height > 0 && input_width > 0 {
                        let cursor = (input.x + prefix_width + offset, input.y);
                        assert!(input.contains(cursor.1 as isize, cursor.0));
                        assert!(cursor.0 < columns && cursor.1 < rows);
                        assert!(!layout.list.contains(cursor.1 as isize, cursor.0));
                        assert!(!layout.preview.contains(cursor.1 as isize, cursor.0));
                    }
                }
            }
        }
    }

    #[test]
    fn reclaimed_body_rows_keep_scrolled_selection_and_rule_hit_testing_aligned() {
        let mut finder = finder();
        let mut update = state(1, 2);
        let template = update.snapshot.panes[0].clone();
        update.snapshot.panes = (0..32)
            .map(|number| {
                let mut pane = template.clone();
                pane.id = format!("pane-{number}");
                pane.terminal_id = format!("terminal_{number}");
                pane
            })
            .collect();
        assert!(finder.receive(update));
        for (columns, rows) in [(110, 7), (60, 7), (110, 25), (60, 25)] {
            finder.layout = Layout::new(columns, rows, Compact::Input);
            finder.navigate(isize::MAX);
            let list = finder.layout.list;
            assert_eq!(finder.selection - finder.scroll, list.height - 1);
            let expected = finder.state.as_ref().unwrap().snapshot.panes
                [finder.index.results()[finder.scroll].pane]
                .id
                .clone();
            finder.mouse(Mouse::LeftClick(list.y as isize, list.x));
            assert_eq!(finder.selected.as_ref(), Some(&expected));
            finder.navigate(isize::MAX);
            let selected = finder.selected.clone();
            for separator in [
                finder.layout.search,
                finder.layout.vertical,
                finder.layout.horizontal,
            ]
            .into_iter()
            .flatten()
            {
                finder.mouse(Mouse::LeftClick(separator.y as isize, separator.x));
                assert_eq!(finder.selected, selected);
            }
            finder.mouse(Mouse::LeftClick((rows - 1) as isize, list.x));
            assert_eq!(finder.selected, selected);
            let expected = finder.state.as_ref().unwrap().snapshot.panes
                [finder.index.results()[finder.scroll + list.height - 1].pane]
                .id
                .clone();
            finder.mouse(Mouse::LeftClick(
                (list.y + list.height - 1) as isize,
                list.x + list.width - 1,
            ));
            assert_eq!(finder.selected.as_ref(), Some(&expected));
        }
    }

    #[test]
    fn selection_tracks_pane_identity_through_reordering_removal_and_focus_updates() {
        let mut finder = finder();
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        let mut refreshed = state(1, 2);
        refreshed.snapshot.panes.reverse();
        refreshed.snapshot.focused_pane_id = Some("pane-1".to_owned());
        assert!(finder.receive(refreshed.clone()));
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        let preview = finder.preview.clone();
        refreshed.revision = 3;
        refreshed.snapshot.revision += 1;
        refreshed.snapshot.focused_pane_id = Some("pane-3".to_owned());
        assert!(finder.receive(refreshed.clone()));
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        assert_eq!(finder.preview, preview);
        refreshed.revision = 4;
        refreshed.snapshot.panes.retain(|pane| pane.id != "pane-2");
        assert!(finder.receive(refreshed.clone()));
        assert!(
            finder.selected.as_ref().is_some_and(|id| refreshed
                .snapshot
                .panes
                .iter()
                .any(|pane| &pane.id == id))
        );
        refreshed.revision = 5;
        refreshed.snapshot.panes.clear();
        assert!(finder.receive(refreshed));
        assert!(finder.selected.is_none());
        assert!(finder.index.results().is_empty());
        assert!(finder.preview.is_empty());
    }

    #[test]
    fn unicode_editor_and_navigation_do_not_treat_plain_letters_as_commands() {
        let mut finder = finder();
        for character in "α界ß".chars() {
            finder.key(key(BareKey::Char(character)));
        }
        finder.key(key(BareKey::Left));
        finder.key(key(BareKey::Backspace));
        assert_eq!(finder.input.value, "αß");
        assert_eq!(finder.input.cursor, "α".len());
        finder.key(key(BareKey::Delete));
        assert_eq!(finder.input.value, "α");
        finder.key(ctrl('u'));
        assert!(finder.input.value.is_empty());
        assert_eq!(finder.input.cursor, 0);
        assert_eq!(finder.selected.as_deref(), Some("pane-1"));
        finder.key(ctrl('j'));
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        finder.key(ctrl('p'));
        assert_eq!(finder.selected.as_deref(), Some("pane-1"));
        finder.key(key(BareKey::Char('j')));
        assert_eq!(finder.input.value, "j");
        assert_eq!(finder.key(key(BareKey::Esc)), KeyAction::Cancel);
        assert_eq!(finder.key(key(BareKey::Enter)), KeyAction::Select);
        for columns in 0..=8 {
            let (text, cursor) = finder.input.visible(columns);
            assert_eq!(ui::width(&text), columns);
            assert!(columns == 0 || cursor < columns);
        }
    }

    #[test]
    fn mouse_selection_and_preview_scrolling_have_independent_state() {
        let mut finder = finder();
        let snapshot = &finder.state.as_ref().unwrap().snapshot;
        let first = snapshot.panes[finder.index.results()[0].pane].id.clone();
        let second = snapshot.panes[finder.index.results()[1].pane].id.clone();
        let list = finder.layout.list;
        finder.mouse(Mouse::LeftClick(list.y as isize, list.x));
        assert_eq!(finder.selected.as_ref(), Some(&first));
        finder.layout.preview.width = 8;
        finder.layout.preview.height = 2;
        let preview = finder.layout.preview;
        finder.prepare_preview();
        assert!(finder.preview_lines.len() > preview.height);
        finder.mouse(Mouse::Hover(preview.y as isize, preview.x));
        finder.mouse(Mouse::ScrollDown(usize::MAX));
        assert_eq!(
            finder.preview_scroll,
            finder.preview_lines.len().saturating_sub(preview.height)
        );
        assert_eq!(finder.selected.as_ref(), Some(&first));
        finder.mouse(Mouse::Hover(list.y as isize, list.x));
        finder.mouse(Mouse::ScrollDown(1));
        assert_eq!(finder.selected.as_ref(), Some(&second));
        assert_eq!(finder.preview_scroll, 0);
        let expected = ui::tooltip(
            &ui::Item::Pane(second),
            &finder.state.as_ref().unwrap().snapshot,
        );
        assert_eq!(finder.preview, expected);
    }

    #[test]
    fn one_row_view_can_reach_input_list_and_exact_preview_without_cursor_overflow() {
        let mut finder = finder();
        finder.layout = Layout::new(5, 1, Compact::Input);
        assert_eq!(finder.layout.input.height, 1);
        finder.key(key(BareKey::Tab));
        finder.layout = Layout::new(5, 1, finder.compact);
        assert_eq!(finder.layout.input.height, 0);
        assert_eq!(finder.layout.list.height, 1);
        finder.key(key(BareKey::Tab));
        finder.layout = Layout::new(5, 1, finder.compact);
        assert_eq!(finder.layout.preview.height, 1);
        finder.key(key(BareKey::Char('界')));
        assert_eq!(finder.compact, Compact::Input);
        let (text, cursor) = finder.input.visible(1);
        assert_eq!(ui::width(&text), 1);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn stale_guest_updates_cannot_reopen_or_reset_a_new_generation() {
        let mut finder = finder();
        finder.key(key(BareKey::Char('x')));
        let mut closed = state(1, 2);
        closed.visible = false;
        assert!(finder.receive(closed));
        assert!(!finder.visible());
        assert!(!finder.receive(state(1, 1)));
        assert!(!finder.visible());
        assert!(finder.receive(state(2, 3)));
        assert!(finder.input.value.is_empty());
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        assert!(!finder.had_focus);
        let mut foreign = state(3, 4);
        foreign.instance = "another-owner".to_owned();
        assert!(!finder.receive(foreign));
    }

    #[test]
    fn controller_rejects_callbacks_from_other_sources_opens_or_removed_panes() {
        let mut controller = Controller {
            id: Some(7),
            instance: "finder:test".to_owned(),
            generation: 2,
            revision: 8,
            open_revision: 6,
            launched: true,
            pending: Some(state(2, 8)),
            ..Controller::default()
        };
        let mut callback = Callback {
            instance: "finder:test".to_owned(),
            generation: 2,
            revision: 7,
            input_sequence: 0,
            pane_id: Some("pane-2".to_owned()),
            restore: false,
        };
        assert!(controller.valid_callback(7, &callback));
        assert!(!controller.valid_callback(8, &callback));
        callback.generation = 1;
        assert!(!controller.valid_callback(7, &callback));
        callback.generation = 2;
        callback.revision = 5;
        assert!(!controller.valid_callback(7, &callback));
        callback.revision = 9;
        assert!(!controller.valid_callback(7, &callback));
        callback.revision = 8;
        controller
            .pending
            .as_mut()
            .unwrap()
            .snapshot
            .panes
            .retain(|pane| pane.id != "pane-2");
        assert!(!controller.valid_callback(7, &callback));
        callback.pane_id = None;
        assert!(controller.valid_callback(7, &callback));
        controller.pending.as_mut().unwrap().visible = false;
        assert!(!controller.valid_callback(7, &callback));
    }

    #[test]
    fn unrelated_revisions_and_focus_do_not_change_search_data() {
        let original = snapshot();
        let mut refreshed = original.clone();
        refreshed.revision += 1;
        refreshed.focused_pane_id = None;
        refreshed.connected = false;
        refreshed.error = Some("transport state only".to_owned());
        assert!(same_data(&original, &refreshed));
        refreshed.panes[0].description.push_str(" new activity");
        assert!(!same_data(&original, &refreshed));
    }
}

#[cfg(test)]
mod buffered_input_tests {
    use super::*;

    fn controller(generation: u64) -> Controller {
        let pending = super::tests::state(generation, 1);
        Controller {
            instance: pending.instance.clone(),
            generation,
            revision: 1,
            open_revision: 1,
            launched: true,
            pending: Some(pending),
            ..Controller::default()
        }
    }

    fn guest() -> Finder {
        Finder {
            instance: "finder:test".to_owned(),
            ..Finder::default()
        }
    }

    fn callback(finder: &Finder) -> Callback {
        let state = finder.state.as_ref().unwrap();
        Callback {
            instance: state.instance.clone(),
            generation: state.generation,
            revision: state.revision,
            input_sequence: finder.applied_sequence,
            pane_id: None,
            restore: false,
        }
    }

    #[test]
    fn cold_query_and_selection_arrive_in_order_before_readiness() {
        let mut owner = controller(1);
        owner.queue_input(Input::Paste("Terminal".to_owned()));
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Down)));
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Left)));
        let mut finder = guest();
        assert!(finder.receive(owner.pending.clone().unwrap()));
        assert_eq!(finder.input.value, "Terminal");
        assert_eq!(finder.input.cursor, "Termina".len());
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        assert_eq!(finder.applied_sequence, 3);
        assert!(!finder.granted && !finder.had_focus);
        let ready = callback(&finder);
        assert!(owner.valid_callback(7, &ready));
        assert_eq!(owner.acknowledge_ready(7, &ready), Readiness::Ready);
        assert!(owner.pending.as_ref().unwrap().inputs.is_empty());
    }

    #[test]
    fn late_ready_catches_up_new_input_before_the_owner_can_handoff_focus() {
        let mut owner = controller(1);
        let mut finder = guest();
        assert!(finder.receive(owner.pending.clone().unwrap()));
        let early_ready = callback(&finder);
        owner.queue_input(Input::Paste("Terminal".to_owned()));
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Down)));
        assert!(owner.valid_callback(7, &early_ready));
        assert_eq!(owner.acknowledge_ready(7, &early_ready), Readiness::Behind);
        assert!(owner.ready_generation.is_none());
        assert_eq!(owner.id, Some(7));
        assert!(finder.receive(owner.pending.clone().unwrap()));
        let current_ready = callback(&finder);
        assert_eq!(owner.acknowledge_ready(7, &current_ready), Readiness::Ready);
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        // A final input between readiness and handoff is still applied, never replayed.
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Down)));
        assert!(finder.receive(owner.pending.clone().unwrap()));
        assert_eq!(finder.selected.as_deref(), Some("pane-3"));
        assert_eq!(
            owner.acknowledge_ready(7, &callback(&finder)),
            Readiness::AlreadyReady
        );
    }

    #[test]
    fn metadata_refresh_and_overlapping_delivery_do_not_replay_paste_or_navigation() {
        let mut owner = controller(1);
        owner.queue_input(Input::Paste("Terminal".to_owned()));
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Down)));
        let original = owner.pending.clone().unwrap();
        let mut finder = guest();
        assert!(finder.receive(original.clone()));
        assert!(!finder.receive(original));
        let mut update = owner.pending.clone().unwrap();
        update.revision += 1;
        update.snapshot.panes[1]
            .description
            .push_str(" fresh metadata");
        assert!(finder.receive(update.clone()));
        assert_eq!(finder.input.value, "Terminal");
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        assert_eq!(finder.applied_sequence, 2);
        update.revision += 1;
        update.inputs.push(SequencedInput {
            sequence: 3,
            event: Input::Key(KeyWithModifier::new(BareKey::Down)),
        });
        update.input_sequence = 3;
        assert!(finder.receive(update));
        assert_eq!(finder.selected.as_deref(), Some("pane-3"));
        assert_eq!(finder.input.value, "Terminal");
    }

    #[test]
    fn warm_open_resets_input_sequence_but_rejects_the_previous_open_acknowledgment() {
        let mut owner = controller(1);
        owner.queue_input(Input::Paste("old query".to_owned()));
        let mut finder = guest();
        assert!(finder.receive(owner.pending.clone().unwrap()));
        let previous_ready = callback(&finder);
        let mut next = super::tests::state(2, 5);
        next.input_sequence = 2;
        next.inputs = vec![
            SequencedInput {
                sequence: 1,
                event: Input::Paste("Terminal".to_owned()),
            },
            SequencedInput {
                sequence: 2,
                event: Input::Key(KeyWithModifier::new(BareKey::Down)),
            },
        ];
        owner.pending = Some(next.clone());
        owner.generation = 2;
        owner.revision = 5;
        owner.open_revision = 5;
        owner.ready_generation = None;
        assert!(finder.receive(next));
        assert_eq!(finder.input.value, "Terminal");
        assert_eq!(finder.selected.as_deref(), Some("pane-2"));
        assert_eq!(finder.applied_sequence, 2);
        assert!(!owner.valid_callback(7, &previous_ready));
        assert_eq!(
            owner.acknowledge_ready(7, &callback(&finder)),
            Readiness::Ready
        );
    }

    #[test]
    fn pending_escape_never_announces_ready_or_replays_later_typing() {
        let mut owner = controller(1);
        owner.queue_input(Input::Paste("Terminal".to_owned()));
        owner.queue_input(Input::Key(KeyWithModifier::new(BareKey::Esc)));
        owner.queue_input(Input::Paste("must not be applied".to_owned()));
        let mut finder = guest();
        assert!(finder.receive(owner.pending.clone().unwrap()));
        assert_eq!(finder.input.value, "Terminal");
        assert_eq!(finder.pending_action, Some(KeyAction::Cancel));
        assert_eq!(finder.applied_sequence, 2);
        assert!(finder.ready.is_none());
        assert!(!finder.had_focus);
    }
}
