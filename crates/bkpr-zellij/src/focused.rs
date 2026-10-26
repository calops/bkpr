//! Shared transport, readiness, framing and focus lifecycle for focused overlays.
//! `Kind` and `Content` supply domain behavior; passive cards stay in `overlay`.
//! New focused views reuse this host rather than owning another helper protocol.
//!
use crate::{animation::WorkingClock, finder, overlay, ui};
use bkpr::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Instant;
use zellij_tile::prelude::*;

pub const UPDATE: &str = "bkpr-focused-update";
pub const READY: &str = "bkpr-focused-ready";
pub const CANCEL: &str = "bkpr-focused-cancel";
pub const SELECTED: &str = "bkpr-focused-selected";
pub const SUBMIT: &str = "bkpr-focused-submit";
const PRESENTED: &str = "bkpr-focused-presented";

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
    kind: Kind,
    busy: bool,
    error: Option<String>,
    snapshot: Snapshot,
    coordinates: FloatingPaneCoordinates,
    input_sequence: u64,
    inputs: Vec<SequencedInput>,
}

#[derive(Serialize, Deserialize)]
struct Callback {
    client_id: ClientId,
    instance: String,
    generation: u64,
    revision: u64,
    input_sequence: u64,
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    restore: bool,
    #[serde(default)]
    action: Option<bkpr::Action>,
}

pub enum Notice {
    Ready,
    Cancelled { restore: bool },
    Selected { pane_id: String },
    Submitted(bkpr::Action),
}

#[derive(Debug, PartialEq, Eq)]
enum Readiness {
    Ready,
    Behind,
    AlreadyReady,
}

pub struct Controller {
    pub id: Option<u32>,
    pub context: Option<ReturnContext>,
    pub submission: Option<u64>,
    pub client_id: ClientId,
    instance: String,
    generation: u64,
    revision: u64,
    open_revision: u64,
    submitted_revision: u64,
    ready_generation: Option<u64>,
    launched: bool,
    pending: Option<State>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            id: None,
            context: None,
            submission: None,
            client_id: 0,
            instance: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_nanos().to_string())
                .unwrap_or_else(|_| "clock-unavailable".to_owned()),
            generation: 0,
            revision: 0,
            open_revision: 0,
            submitted_revision: 0,
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
        kind: Kind,
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
            kind: kind.clone(),
            busy: false,
            error: None,
            snapshot: snapshot.clone(),
            coordinates: geometry(&kind, columns, rows),
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
        let coordinates = geometry(&state.kind, columns, rows);
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
        if !matches!(message.name.as_str(), READY | CANCEL | SELECTED | SUBMIT) {
            return None;
        }
        let PipeSource::Plugin(source) = message.source else {
            return None;
        };
        let payload = message.payload.as_deref()?;
        if payload.len() > bkpr::MAX_MESSAGE_BYTES {
            return None;
        }
        let callback: Callback = serde_json::from_str(payload).ok()?;
        if !self.valid_callback(source, &callback) {
            return None;
        }
        self.id = Some(source);
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
                if self.pending.as_ref().is_some_and(|state| !state.busy) {
                    self.close(owner, configuration);
                }
                Some(Notice::Cancelled {
                    restore: callback.restore,
                })
            }
            SELECTED => {
                let pane_id = callback.pane_id?;
                self.close(owner, configuration);
                Some(Notice::Selected { pane_id })
            }
            SUBMIT => {
                let action = callback.action?;
                let state = self.pending.as_mut()?;
                if state.busy
                    || callback.revision <= self.submitted_revision
                    || !state.kind.accepts(&action)
                {
                    return None;
                }
                self.submitted_revision = callback.revision;
                state.busy = true;
                state.error = None;
                self.revision = self.revision.saturating_add(1);
                state.revision = self.revision;
                self.send(owner, configuration);
                Some(Notice::Submitted(action))
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
        if callback.input_sequence < state.input_sequence || callback.revision < state.revision {
            return Readiness::Behind;
        }
        if self.ready_generation == Some(callback.generation) {
            return Readiness::AlreadyReady;
        }
        self.ready_generation = Some(callback.generation);
        Readiness::Ready
    }

    fn valid_callback(&self, source: u32, callback: &Callback) -> bool {
        callback.client_id == self.client_id
            && self.launched
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

    pub fn kind(&self) -> Option<&Kind> {
        self.pending
            .as_ref()
            .filter(|state| state.visible)
            .map(|state| &state.kind)
    }

    pub fn submission_failed(
        &mut self,
        error: String,
        owner: u32,
        configuration: &BTreeMap<String, String>,
    ) {
        if let Some(state) = self
            .pending
            .as_mut()
            .filter(|state| state.visible && state.busy)
        {
            self.revision = self.revision.saturating_add(1);
            state.revision = self.revision;
            state.busy = false;
            state.error = Some(error);
            self.send(owner, configuration);
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
            config.insert("role".to_owned(), "focused-overlay".to_owned());
            config.insert("modal_overlay".to_owned(), "true".to_owned());
            config.insert("owner".to_owned(), owner.to_string());
            config.insert("owner_client_id".to_owned(), self.client_id.to_string());
            // Stable across opens: exact URL/config identity prevents duplicate launches.
            config.insert("focused_instance".to_owned(), self.instance.clone());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("bkpr focused overlay".to_owned()),
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

fn geometry(kind: &Kind, columns: usize, rows: usize) -> FloatingPaneCoordinates {
    let (width, height) = match kind {
        Kind::Finder => (
            (columns.saturating_mul(4) / 5).clamp(1, 140),
            (rows.saturating_mul(3) / 4).clamp(1, 36),
        ),
        Kind::Workspace => (64, 10),
        Kind::Pane { .. } => (64, 8),
    };
    let width = width.min(columns.max(1));
    let height = height.min(rows.max(1));
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

pub struct ReturnContext {
    pub sidebar: bool,
    pub terminal: Option<u32>,
    pub cursor: Option<ui::Item>,
    pub mode: InputMode,
    pub ready: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Finder,
    Workspace,
    Pane { workspace_id: String },
}

impl Kind {
    fn accepts(&self, action: &bkpr::Action) -> bool {
        match (self, action) {
            (Self::Workspace, bkpr::Action::CreateWorkspace { .. }) => true,
            (
                Self::Pane {
                    workspace_id: expected,
                },
                bkpr::Action::CreatePane { workspace_id, .. },
            ) => expected == workspace_id,
            _ => false,
        }
    }
}

fn configuration_valid(configuration: &BTreeMap<String, String>) -> bool {
    if configuration.get("role").map(String::as_str) != Some("focused-overlay")
        || configuration.get("modal_overlay").map(String::as_str) != Some("true")
        || configuration
            .get("owner")
            .and_then(|value| value.parse::<u32>().ok())
            .is_none()
        || configuration
            .get("owner_client_id")
            .and_then(|value| value.parse::<ClientId>().ok())
            .is_none()
        || configuration
            .get("focused_instance")
            .is_none_or(String::is_empty)
    {
        return false;
    }
    let mut sidebar = configuration.clone();
    for key in [
        // Added by native message routing, not sidebar application configuration.
        "caller_cwd",
        "modal_overlay",
        "owner",
        "owner_client_id",
        "focused_instance",
    ] {
        sidebar.remove(key);
    }
    sidebar.insert("role".to_owned(), "sidebar".to_owned());
    crate::native::configuration_valid(&sidebar, "sidebar")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    None,
    Cancel,
    Select,
    Submit,
}

#[allow(clippy::large_enum_variant)] // A helper owns one content value; keep its buffers inline.
enum Content {
    Finder(finder::Finder),
    Form(Form),
}

impl Default for Content {
    fn default() -> Self {
        Self::Finder(finder::Finder::default())
    }
}

impl Content {
    fn new(kind: &Kind) -> Self {
        match kind {
            Kind::Finder => Self::default(),
            _ => Self::Form(Form::default()),
        }
    }

    fn update(
        &mut self,
        snapshot: &Snapshot,
        opening: bool,
        changed: bool,
        coordinates: &FloatingPaneCoordinates,
    ) {
        if let Self::Finder(finder) = self {
            finder.update(snapshot, opening, changed, coordinates);
        }
    }

    fn input(&mut self, state: &State, input: Input) -> KeyAction {
        match self {
            Self::Finder(finder) => finder.input_event(&state.snapshot, input),
            Self::Form(form) => form.input(&state.kind, input),
        }
    }

    fn mouse(&mut self, snapshot: &Snapshot, mouse: Mouse) -> KeyAction {
        if matches!(mouse, Mouse::LeftClick(-1, column) | Mouse::RightClick(-1, column) if column == u16::MAX as usize)
        {
            return KeyAction::Cancel;
        }
        match self {
            Self::Finder(finder) => finder.mouse(snapshot, mouse),
            Self::Form(_) => KeyAction::None,
        }
    }

    fn selected(&self) -> Option<&str> {
        match self {
            Self::Finder(finder) => finder.selected(),
            Self::Form(_) => None,
        }
    }

    fn working(&self) -> bool {
        matches!(self, Self::Finder(finder) if finder.working_clock_active())
    }

    fn prepare(&mut self, kind: &Kind, rows: usize, columns: usize) -> Option<(usize, usize)> {
        match self {
            Self::Finder(finder) => finder.prepare(rows, columns),
            Self::Form(form) => form.cursor(kind, rows, columns),
        }
    }

    fn render(
        &mut self,
        state: &State,
        theme: &ui::Theme,
        clock: usize,
        rows: usize,
        columns: usize,
    ) {
        match self {
            Self::Finder(finder) => finder.render(&state.snapshot, theme, clock, rows, columns),
            Self::Form(form) => form.render(state, theme, rows, columns),
        }
    }
}

#[derive(Default)]
struct Form {
    name: ui::Input,
    cwd: ui::Input,
    cwd_active: bool,
}

impl Form {
    fn input(&mut self, kind: &Kind, input: Input) -> KeyAction {
        if matches!(&input, Input::Key(key) if key.key_modifiers.contains(&KeyModifier::Alt) || key.key_modifiers.contains(&KeyModifier::Super))
        {
            return KeyAction::None;
        }
        let editor = if matches!(kind, Kind::Pane { .. }) || self.cwd_active {
            &mut self.cwd
        } else {
            &mut self.name
        };
        match input {
            Input::Key(key) if key.bare_key == BareKey::Esc => return KeyAction::Cancel,
            Input::Key(key) if key.bare_key == BareKey::Enter => return KeyAction::Submit,
            Input::Key(key) if key.bare_key == BareKey::Tab => {
                if matches!(kind, Kind::Workspace) {
                    self.cwd_active = !self.cwd_active;
                }
            }
            Input::Key(key) => {
                editor.key(&key);
            }
            Input::Paste(text) => editor.insert(&text),
        }
        KeyAction::None
    }

    fn action(&self, kind: &Kind) -> Option<bkpr::Action> {
        let optional = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        match kind {
            Kind::Workspace => Some(bkpr::Action::CreateWorkspace {
                label: optional(&self.name.value),
                cwd: optional(&self.cwd.value),
            }),
            Kind::Pane { workspace_id } => Some(bkpr::Action::CreatePane {
                workspace_id: workspace_id.clone(),
                cwd: optional(&self.cwd.value),
            }),
            Kind::Finder => None,
        }
    }

    fn field(&self, kind: &Kind) -> (&ui::Input, &'static str, usize) {
        if matches!(kind, Kind::Pane { .. }) || self.cwd_active {
            (
                &self.cwd,
                "Working directory (optional)",
                if matches!(kind, Kind::Workspace) {
                    5
                } else {
                    2
                },
            )
        } else {
            (&self.name, "Workspace name (optional)", 2)
        }
    }

    fn cursor(&self, kind: &Kind, rows: usize, columns: usize) -> Option<(usize, usize)> {
        let margin = usize::from(rows >= 3 && columns >= 4);
        let width = columns.saturating_sub(margin * 2);
        if rows == 0 || width == 0 {
            return None;
        }
        let (input, _, desired) = self.field(kind);
        let expanded = rows >= 8 && (!matches!(kind, Kind::Workspace) || rows >= 9);
        let row = if expanded {
            desired + 1
        } else {
            margin.min(rows - 1)
        };
        let (_, cursor) = input.visible(width);
        Some((margin + cursor, row.min(rows.saturating_sub(margin + 1))))
    }

    fn render(&self, state: &State, theme: &ui::Theme, rows: usize, columns: usize) {
        let border = rows >= 3 && columns >= 4;
        let margin = usize::from(border);
        let width = columns.saturating_sub(margin * 2);
        let title = if matches!(state.kind, Kind::Workspace) {
            " Create workspace "
        } else {
            " Create terminal "
        };
        frame(rows, columns, border, title, theme);
        if rows == 0 || width == 0 {
            return;
        }
        if rows >= 8 && (!matches!(state.kind, Kind::Workspace) || rows >= 9) {
            if matches!(state.kind, Kind::Workspace) {
                paint(
                    margin,
                    2,
                    &ui::truncate("Workspace name (optional)", width),
                    theme,
                );
                paint(margin, 3, &self.name.visible(width).0, theme);
            }
            let row = if matches!(state.kind, Kind::Workspace) {
                5
            } else {
                2
            };
            paint(
                margin,
                row,
                &ui::truncate("Working directory (optional)", width),
                theme,
            );
            paint(margin, row + 1, &self.cwd.visible(width).0, theme);
        } else if let Some((_, row)) = self.cursor(&state.kind, rows, columns) {
            paint(
                margin,
                row,
                &self.field(&state.kind).0.visible(width).0,
                theme,
            );
        }
        if rows >= 5 {
            let hint = if let Some(error) = state.error.as_deref() {
                error
            } else if state.busy {
                "Creating…"
            } else if matches!(state.kind, Kind::Workspace) {
                "Enter create · Esc cancel · Tab next field"
            } else {
                "Enter create · Esc cancel"
            };
            let style = if state.error.is_some() {
                &theme.deleted
            } else {
                &theme.muted
            };
            paint(
                margin,
                rows - margin - 1,
                &format!("{style}{}", ui::truncate(hint, width)),
                theme,
            );
        }
    }
}

pub(crate) fn frame(rows: usize, columns: usize, border: bool, title: &str, theme: &ui::Theme) {
    let blank = " ".repeat(columns.saturating_sub(usize::from(border) * 2));
    let lines = [0, 1, rows.saturating_sub(1)].map(|row| {
        if border {
            overlay::outlined_line(blank.clone(), row, rows, columns, theme)
        } else {
            blank.clone()
        }
    });
    for row in 0..rows {
        paint(
            0,
            row,
            &lines[if row == 0 {
                0
            } else if row + 1 == rows {
                2
            } else {
                1
            }],
            theme,
        );
    }
    if border {
        paint(
            2,
            0,
            &format!(
                "{}{}",
                theme.bright,
                ui::truncate(title, columns.saturating_sub(4))
            ),
            theme,
        );
    }
}

pub(crate) fn paint(x: usize, y: usize, text: &str, theme: &ui::Theme) {
    print!(
        "{}\u{1b}[{};{}H{}{}{text}{}{}",
        overlay::OSC8_CLOSE,
        y + 1,
        x + 1,
        ui::RESET,
        theme.base,
        overlay::OSC8_CLOSE,
        ui::RESET
    );
}
#[derive(Default)]
pub struct Host {
    client_id: ClientId,
    theme: ui::Theme,
    working_clock: WorkingClock,
    owner: Option<u32>,
    id: u32,
    instance: String,
    granted: bool,
    foreign_client: bool,
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
    content: Content,
    presented: Option<(u64, u64)>,
    cursor: Option<Option<(usize, usize)>>,
    applied_coordinates: Option<FloatingPaneCoordinates>,
    shown: Option<bool>,
}
impl Host {
    pub fn set_terminal_colors(&mut self, colors: &crate::palette::TerminalColors) -> bool {
        self.theme.update(colors)
    }

    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32, client_id: ClientId) {
        self.id = id;
        self.client_id = client_id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        self.instance = configuration
            .get("focused_instance")
            .cloned()
            .unwrap_or_default();
        self.foreign_client = configuration
            .get("owner_client_id")
            .and_then(|owner| owner.parse::<ClientId>().ok())
            != Some(client_id)
            || !configuration_valid(configuration);
        // Foreign replicas must not focus, hide or close the shared focused overlay pane.
        if self.foreign_client {
            return;
        }
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::CustomMessage,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::TabUpdate,
            EventType::Key,
            EventType::PastedText,
            EventType::Mouse,
            EventType::BeforeClose,
            EventType::Timer,
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
        if self.foreign_client {
            return false;
        }
        match event {
            Event::Timer(_) => {
                let active = self.working_clock_active();
                return self.working_clock.tick(Instant::now(), active);
            }
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
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.granted = false;
                self.working_clock.schedule(Instant::now(), false);
                close_self();
            }
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
                self.callback(CANCEL, None, false, None);
                self.working_clock.schedule(Instant::now(), false);
                self.dismissed = true;
            }
            Event::Key(key) if self.granted && self.visible() => {
                let action = self.input_event(Input::Key(key));
                self.dispatch(action);
            }
            Event::PastedText(text) if self.granted && self.visible() => {
                self.input_event(Input::Paste(text));
            }
            Event::Mouse(mouse) if self.granted && self.visible() => {
                let action = self
                    .content
                    .mouse(&self.state.as_ref().unwrap().snapshot, mouse);
                self.dispatch(action);
            }
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
        if self.foreign_client {
            return false;
        }
        if message.name == PRESENTED
            && matches!(message.source, PipeSource::Plugin(id) if id == self.id)
        {
            let Some(payload) = message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= 8192)
            else {
                return false;
            };
            let Ok(callback) = serde_json::from_str::<Callback>(payload) else {
                return false;
            };
            if self.acknowledge_frame(&callback) {
                self.callback(READY, None, false, None);
            }
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
        if payload.len() > bkpr::MAX_MESSAGE_BYTES * 2 {
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
            || self.state.as_ref().is_some_and(|current| {
                state.revision <= current.revision
                    || state.generation < current.generation
                    || (state.generation == current.generation && state.kind != current.kind)
            })
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
            // Hidden helpers can retain reports from the tab we just returned from.
            // Wait for current native reports before reconciling the new generation.
            self.owner_tab = None;
            self.active_tab = None;
            self.floating_visible = false;
            self.had_focus = false;
            self.pane_focused = false;
            self.dismissed = false;
            self.ready = None;
            self.presented = None;
            self.applied_sequence = 0;
            self.pending_action = None;
            self.content = Content::new(&state.kind);
        }
        self.content
            .update(&state.snapshot, opening, data_changed, &state.coordinates);
        let inputs = std::mem::take(&mut state.inputs);
        self.theme.prepare_snapshot(&state.snapshot);
        self.state = Some(state);
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
        if !self.working_clock_active() {
            self.working_clock.schedule(Instant::now(), false);
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
            self.working_clock.schedule(Instant::now(), false);
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
        if self.shown != Some(true) {
            show_pane_with_id(pane, true, false);
            // Native applies selectability only to installed, unsuppressed panes.
            set_selectable(true);
            self.shown = Some(true);
            self.applied_coordinates = None;
        }
        // Native show restores suppressed coordinates; always place afterwards.
        if self.applied_coordinates.as_ref() != Some(&state.coordinates) {
            change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
            self.applied_coordinates = Some(state.coordinates.clone());
        }
    }

    fn callback(
        &self,
        name: &str,
        pane_id: Option<String>,
        restore: bool,
        action: Option<bkpr::Action>,
    ) {
        let (Some(owner), Some(state)) = (self.owner, &self.state) else {
            return;
        };
        let callback = Callback {
            client_id: self.client_id,
            instance: state.instance.clone(),
            generation: state.generation,
            revision: state.revision,
            input_sequence: self.applied_sequence,
            pane_id,
            restore,
            action,
        };
        if let Ok(payload) = serde_json::to_string(&callback) {
            pipe_message_to_plugin(
                MessageToPlugin::new(name)
                    .with_destination_plugin_id(if name == PRESENTED { self.id } else { owner })
                    .with_payload(payload),
            );
        }
    }

    fn finish(&mut self, name: &str, pane_id: Option<String>, restore: bool) {
        if !self.visible() {
            return;
        }
        self.callback(name, pane_id, restore, None);
        self.dismissed = true;
        self.working_clock.schedule(Instant::now(), false);
        self.had_focus = false;
        set_selectable(false);
        self.set_cursor(None);
        hide_self();
        self.shown = Some(false);
    }

    fn input_event(&mut self, input: Input) -> KeyAction {
        let Some(state) = self.state.as_ref() else {
            return KeyAction::None;
        };
        if state.busy {
            return if matches!(input, Input::Key(key) if key.bare_key == BareKey::Esc) {
                KeyAction::Cancel
            } else {
                KeyAction::None
            };
        }
        self.content.input(state, input)
    }

    fn dispatch(&mut self, action: KeyAction) {
        match action {
            KeyAction::Cancel => {
                if self.state.as_ref().is_some_and(|state| state.busy) {
                    // Only the owner knows whether creation is still queued.
                    self.callback(CANCEL, None, true, None);
                } else {
                    self.finish(CANCEL, None, true);
                }
            }
            KeyAction::Select => {
                if let Some(pane) = self.content.selected().map(str::to_owned) {
                    self.finish(SELECTED, Some(pane), false);
                }
            }
            KeyAction::Submit => {
                if let (Content::Form(form), Some(state)) = (&self.content, &self.state)
                    && !state.busy
                    && let Some(action) = form.action(&state.kind)
                {
                    self.callback(SUBMIT, None, false, Some(action));
                    self.state.as_mut().unwrap().busy = true;
                }
            }
            KeyAction::None => {}
        }
    }

    fn set_cursor(&mut self, cursor: Option<(usize, usize)>) {
        if self.cursor != Some(cursor) {
            show_cursor(cursor);
            self.cursor = Some(cursor);
        }
    }

    fn acknowledge_frame(&mut self, callback: &Callback) -> bool {
        let revision = (callback.generation, callback.revision);
        if !self.granted
            || !self.visible()
            || callback.client_id != self.client_id
            || self.ready == Some(revision)
            || self.presented != Some(revision)
            || !self.state.as_ref().is_some_and(|state| {
                callback.instance == state.instance
                    && callback.generation == state.generation
                    && callback.revision == state.revision
                    && callback.input_sequence == self.applied_sequence
            })
        {
            return false;
        }
        self.ready = Some(revision);
        true
    }
    fn working_clock_active(&self) -> bool {
        self.granted && self.visible() && self.shown == Some(true) && self.content.working()
    }

    pub fn render(&mut self, rows: usize, columns: usize) {
        if self.foreign_client || !self.granted || !self.visible() {
            return;
        }
        let state = self.state.as_ref().unwrap();
        let exact_size = state
            .coordinates
            .width
            .is_some_and(|width| width.to_fixed(0) == columns)
            && state
                .coordinates
                .height
                .is_some_and(|height| height.to_fixed(0) == rows);
        if !exact_size {
            self.set_cursor(None);
            print!("{}\u{1b}[2J", overlay::OSC8_CLOSE);
            return;
        }
        let cursor = self.content.prepare(&state.kind, rows, columns);
        self.set_cursor(cursor);
        let active = self.working_clock_active();
        self.working_clock.schedule(Instant::now(), active);
        let state = self.state.as_ref().unwrap();
        let revision = (state.generation, state.revision);
        if self.ready != Some(revision) {
            // Native applies this complete exact-size frame before delivering this self pipe.
            self.callback(PRESENTED, None, false, None);
            self.presented = Some(revision);
        }
        self.content.render(
            state,
            &self.theme,
            self.working_clock.frame(),
            rows,
            columns,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::{Pane, Status, Workspace};
    fn snapshot() -> Snapshot {
        Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
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
                    agent_id: None,
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
    fn state(kind: Kind, generation: u64, revision: u64) -> State {
        State {
            instance: "focused:test".to_owned(),
            generation,
            revision,
            visible: true,
            coordinates: geometry(&kind, 160, 48),
            kind,
            busy: false,
            error: None,
            snapshot: snapshot(),
            input_sequence: 0,
            inputs: Vec::new(),
        }
    }

    fn owner(kind: Kind) -> Controller {
        Controller {
            id: Some(7),
            client_id: 1,
            instance: "focused:test".to_owned(),
            generation: 1,
            revision: 1,
            open_revision: 1,
            launched: true,
            pending: Some(state(kind, 1, 1)),
            ..Controller::default()
        }
    }

    fn guest() -> Host {
        Host {
            client_id: 1,
            owner: Some(3),
            id: 7,
            instance: "focused:test".to_owned(),
            ..Host::default()
        }
    }

    fn key(key: BareKey) -> Input {
        Input::Key(KeyWithModifier::new(key))
    }

    fn callback(host: &Host) -> Callback {
        let state = host.state.as_ref().unwrap();
        Callback {
            client_id: host.client_id,
            instance: state.instance.clone(),
            generation: state.generation,
            revision: state.revision,
            input_sequence: host.applied_sequence,
            pane_id: None,
            restore: false,
            action: None,
        }
    }

    fn message(name: &str, callback: &Callback, source: u32) -> PipeMessage {
        PipeMessage {
            source: PipeSource::Plugin(source),
            name: name.to_owned(),
            payload: Some(serde_json::to_string(callback).unwrap()),
            args: BTreeMap::new(),
            is_private: true,
        }
    }

    fn form(host: &Host) -> &Form {
        match &host.content {
            Content::Form(form) => form,
            _ => panic!("expected creation form"),
        }
    }

    #[test]
    fn cold_workspace_form_applies_keys_and_paste_once_then_retains_failed_submission() {
        let mut owner = owner(Kind::Workspace);
        for input in [
            Input::Paste("  Work α  ".to_owned()),
            key(BareKey::Tab),
            Input::Paste(" /tmp/project ".to_owned()),
            key(BareKey::Enter),
            Input::Paste("must not leak".to_owned()),
        ] {
            owner.queue_input(input);
        }
        let mut host = guest();
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert_eq!(host.applied_sequence, 4);
        assert_eq!(host.pending_action, Some(KeyAction::Submit));
        assert_eq!(form(&host).name.value, "  Work α  ");
        assert_eq!(form(&host).cwd.value, " /tmp/project ");
        let mut submitted = callback(&host);
        submitted.action = form(&host).action(&Kind::Workspace);
        assert!(
            matches!(&submitted.action, Some(bkpr::Action::CreateWorkspace { label: Some(label), cwd: Some(cwd) }) if label == "Work α" && cwd == "/tmp/project")
        );
        let message = message(SUBMIT, &submitted, 7);
        assert!(matches!(
            owner.pipe(&message, 3, &BTreeMap::new()),
            Some(Notice::Submitted(_))
        ));
        assert!(owner.pipe(&message, 3, &BTreeMap::new()).is_none());
        host.pending_action = None;
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert_eq!(form(&host).cwd.value, " /tmp/project ");
        assert_eq!(host.input_event(key(BareKey::Enter)), KeyAction::None);
        host.dispatch(KeyAction::Cancel);
        assert!(host.visible(), "busy cancellation must wait for the owner");
        let mut cancel = callback(&host);
        cancel.restore = true;
        let cancel = self::message(CANCEL, &cancel, 7);
        assert!(matches!(
            owner.pipe(&cancel, 3, &BTreeMap::new()),
            Some(Notice::Cancelled { restore: true })
        ));
        assert!(
            owner.is_open(),
            "only the owner can cancel a queued creation"
        );
        owner.submission_failed("Directory does not exist".to_owned(), 3, &BTreeMap::new());
        assert!(owner.pipe(&message, 3, &BTreeMap::new()).is_none());
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert_eq!(
            host.state.as_ref().unwrap().error.as_deref(),
            Some("Directory does not exist")
        );
        assert_eq!(form(&host).name.value, "  Work α  ");
        assert_eq!(form(&host).cwd.value, " /tmp/project ");
        host.input_event(key(BareKey::Backspace));
        assert_eq!(host.input_event(key(BareKey::Enter)), KeyAction::Submit);
    }

    #[test]
    fn pane_form_has_one_editor_preserves_workspace_and_accepts_optional_blanks() {
        let kind = Kind::Pane {
            workspace_id: "workspace-target".to_owned(),
        };
        let mut host = guest();
        assert!(host.receive(state(kind.clone(), 1, 1)));
        host.input_event(key(BareKey::Tab));
        host.input_event(Input::Paste("   ".to_owned()));
        assert!(form(&host).name.value.is_empty());
        assert!(
            matches!(form(&host).action(&kind), Some(bkpr::Action::CreatePane { workspace_id, cwd: None }) if workspace_id == "workspace-target")
        );
        host.input_event(Input::Paste("/tmp/界".to_owned()));
        host.input_event(key(BareKey::Left));
        host.input_event(key(BareKey::Delete));
        assert_eq!(form(&host).cwd.value, "   /tmp/");
        assert_eq!(host.input_event(key(BareKey::Esc)), KeyAction::Cancel);
        assert!(!Kind::Workspace.accepts(&form(&host).action(&kind).unwrap()));
        assert!(
            !Kind::Pane {
                workspace_id: "other".to_owned()
            }
            .accepts(&form(&host).action(&kind).unwrap())
        );
    }

    #[test]
    fn buffered_finder_input_is_ordered_deduplicated_and_caught_up_before_handoff() {
        let mut owner = owner(Kind::Finder);
        let mut host = guest();
        assert!(host.receive(owner.pending.clone().unwrap()));
        let early = callback(&host);
        owner.queue_input(Input::Paste("Terminal".to_owned()));
        owner.queue_input(key(BareKey::Down));
        assert_eq!(owner.acknowledge_ready(7, &early), Readiness::Behind);
        assert!(owner.ready_generation.is_none());
        let update = owner.pending.clone().unwrap();
        assert!(host.receive(update.clone()));
        assert!(!host.receive(update));
        assert_eq!(host.content.selected(), Some("pane-2"));
        assert_eq!(host.applied_sequence, 2);
        assert_eq!(
            owner.acknowledge_ready(7, &callback(&host)),
            Readiness::Ready
        );
        assert!(owner.pending.as_ref().unwrap().inputs.is_empty());
        owner.queue_input(key(BareKey::Down));
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert_eq!(host.content.selected(), Some("pane-3"));
        assert_eq!(
            owner.acknowledge_ready(7, &callback(&host)),
            Readiness::AlreadyReady
        );
    }

    #[test]
    fn readiness_waits_for_exact_presented_frame_and_rejects_stale_resize_frames() {
        let mut host = guest();
        assert!(host.receive(state(Kind::Workspace, 1, 1)));
        host.granted = true;
        let before = callback(&host);
        assert!(!host.acknowledge_frame(&before));
        host.render(9, 63);
        assert!(host.presented.is_none());
        host.render(10, 64);
        assert!(host.ready.is_none());
        assert!(host.acknowledge_frame(&before));
        assert!(!host.acknowledge_frame(&before));
        let mut resized = state(Kind::Workspace, 1, 2);
        resized.coordinates = geometry(&Kind::Workspace, 40, 7);
        assert!(host.receive(resized));
        assert!(!host.acknowledge_frame(&before));
        host.render(10, 64);
        let current = callback(&host);
        assert!(!host.acknowledge_frame(&current));
        host.render(7, 40);
        assert!(host.acknowledge_frame(&current));
    }

    #[test]
    fn rapid_reopen_retains_helper_but_never_accepts_foreign_or_previous_generation_callbacks() {
        let mut owner = owner(Kind::Finder);
        let mut host = guest();
        assert!(host.receive(owner.pending.clone().unwrap()));
        let previous = callback(&host);
        owner.close(3, &BTreeMap::new());
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert!(!host.visible());
        assert_eq!(owner.id, Some(7));
        assert!(owner.launched);
        owner.open(3, &BTreeMap::new(), &snapshot(), Kind::Workspace, 100, 30);
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert!(host.visible());
        assert!(form(&host).name.value.is_empty());
        assert!(!owner.valid_callback(7, &previous));
        let mut current = callback(&host);
        assert!(owner.valid_callback(7, &current));
        assert!(!owner.valid_callback(8, &current));
        current.client_id = 2;
        assert!(!owner.valid_callback(7, &current));
        current.client_id = 1;
        current.instance = "foreign".to_owned();
        assert!(!owner.valid_callback(7, &current));
        let mut old = state(Kind::Finder, 1, 100);
        assert!(!host.receive(old.clone()));
        old.generation = 2;
        assert!(!host.receive(old));
    }

    #[test]
    fn cancellation_stops_buffered_input_and_focus_loss_does_not_steal_focus_back() {
        let mut owner = owner(Kind::Workspace);
        owner.queue_input(Input::Paste("keep".to_owned()));
        owner.queue_input(key(BareKey::Esc));
        owner.queue_input(Input::Paste("discard".to_owned()));
        let mut host = guest();
        assert!(host.receive(owner.pending.clone().unwrap()));
        assert_eq!(host.applied_sequence, 2);
        assert_eq!(host.pending_action, Some(KeyAction::Cancel));
        assert_eq!(form(&host).name.value, "keep");
        assert!(host.ready.is_none());
        let mut cancel = callback(&host);
        cancel.restore = true;
        assert!(matches!(
            owner.pipe(&message(CANCEL, &cancel, 7), 3, &BTreeMap::new()),
            Some(Notice::Cancelled { restore: true })
        ));
        host.granted = true;
        host.pending_action = None;
        host.owner_tab = Some(0);
        host.active_tab = Some(1);
        host.reconcile_focus();
        assert!(!host.visible());
        assert!(host.dismissed);
    }

    #[test]
    fn reopening_ignores_visibility_cached_before_the_owner_tab_returned() {
        let mut host = guest();
        let mut hidden = state(
            Kind::Pane {
                workspace_id: "workspace".to_owned(),
            },
            1,
            1,
        );
        hidden.visible = false;
        assert!(host.receive(hidden));
        host.owner_tab = Some(1);
        host.active_tab = Some(2);
        host.floating_visible = false;
        assert!(host.receive(state(Kind::Workspace, 2, 2)));
        host.reconcile_focus();
        assert!(
            host.visible(),
            "old hidden-tab metadata cancelled the new overlay"
        );
        host.owner_tab = Some(1);
        host.active_tab = Some(1);
        host.reconcile_focus();
        assert!(host.visible());
        host.active_tab = Some(2);
        host.reconcile_focus();
        assert!(!host.visible(), "a fresh tab departure must still cancel");
    }

    fn configuration() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("role".to_owned(), "focused-overlay".to_owned()),
            ("session".to_owned(), "test".to_owned()),
            ("session_id".to_owned(), "session-test".to_owned()),
            ("program".to_owned(), "/bin/bkpr".to_owned()),
            ("initial_cwd".to_owned(), "/tmp".to_owned()),
            ("plugin_url".to_owned(), "file:/plugin.wasm".to_owned()),
            ("sidebar_width".to_owned(), "24".to_owned()),
            ("owner".to_owned(), "3".to_owned()),
            ("owner_client_id".to_owned(), "1".to_owned()),
            ("focused_instance".to_owned(), "focused:test".to_owned()),
            ("modal_overlay".to_owned(), "true".to_owned()),
        ])
    }

    #[test]
    fn foreign_replica_is_inert_and_configuration_rejects_legacy_roles_or_extra_keys() {
        let mut config = configuration();
        assert!(configuration_valid(&config));
        let mut host = Host::default();
        host.load(&config, 7, 2);
        assert!(!host.event(Event::PermissionRequestResult(PermissionStatus::Granted)));
        assert!(!host.event(Event::BeforeClose));
        assert!(!host.granted && !host.dismissed);
        host.render(10, 64);
        assert!(host.cursor.is_none());
        let update = PipeMessage {
            source: PipeSource::Plugin(3),
            name: UPDATE.to_owned(),
            payload: Some(serde_json::to_string(&state(Kind::Workspace, 1, 1)).unwrap()),
            args: BTreeMap::new(),
            is_private: true,
        };
        assert!(!host.pipe(update));
        assert!(host.state.is_none());
        config.insert("role".to_owned(), "finder".to_owned());
        assert!(!configuration_valid(&config));
        config.insert("role".to_owned(), "focused-overlay".to_owned());
        config.insert("unexpected".to_owned(), "true".to_owned());
        assert!(!configuration_valid(&config));
    }

    #[test]
    fn host_routing_metadata_does_not_disable_an_owned_overlay() {
        let mut config = configuration();
        config.insert("caller_cwd".to_owned(), "/tmp/launch-directory".to_owned());
        let mut host = Host::default();
        host.load(&config, 7, 1);
        let mut update = state(Kind::Workspace, 1, 1);
        update.input_sequence = 1;
        update.inputs.push(SequencedInput {
            sequence: 1,
            event: Input::Paste("typed workspace".to_owned()),
        });
        assert!(host.pipe(PipeMessage {
            source: PipeSource::Plugin(3),
            name: UPDATE.to_owned(),
            payload: Some(serde_json::to_string(&update).unwrap()),
            args: BTreeMap::new(),
            is_private: true,
        }));
        assert_eq!(form(&host).name.value, "typed workspace");
    }

    #[test]
    fn all_content_geometries_and_form_cursors_are_centered_and_clamped() {
        for kind in [
            Kind::Finder,
            Kind::Workspace,
            Kind::Pane {
                workspace_id: "workspace".to_owned(),
            },
        ] {
            for columns in [1, 2, 20, 80, 175, 300] {
                for rows in [1, 2, 5, 24, 48, 100] {
                    let coordinates = geometry(&kind, columns, rows);
                    let x = coordinates.x.unwrap().to_position(columns);
                    let y = coordinates.y.unwrap().to_position(rows);
                    let width = coordinates.width.unwrap().to_position(columns);
                    let height = coordinates.height.unwrap().to_position(rows);
                    assert!(width > 0 && height > 0 && x + width <= columns && y + height <= rows);
                    assert!((columns - width).abs_diff(x * 2) <= 1);
                    assert!((rows - height).abs_diff(y * 2) <= 1);
                    for cwd_active in [false, true] {
                        let form = Form {
                            cwd_active,
                            ..Form::default()
                        };
                        if let Some((x, y)) = form.cursor(&kind, height, width) {
                            assert!(x < width && y < height);
                        }
                    }
                }
            }
        }
    }
}
