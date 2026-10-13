mod native;
mod overlay;
mod ui;

use serde_json::from_slice;
use shephrd::{Action, Effect, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Response, Snapshot};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};
use ui::{Item, Modal};
use zellij_tile::prelude::*;

// Zellij caches permissions per WASM path, replacing rather than merging each role's grant.
const PERMISSIONS: &[PermissionType] = &[
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::MessageAndLaunchOtherPlugins,
    PermissionType::ReadCliPipes,
];

const CONTROL_PIPE: &str = "shephrd-controls";

#[derive(Clone)]
struct DesiredSelection {
    item: Item,
    request_id: u64,
    acknowledged: bool,
}

struct PendingUserAction {
    started: Instant,
    shown: bool,
}

struct CachedPopup {
    item: Item,
    instance: String,
    revision: u64,
    formatted_at: u64,
    content: ui::PopupContent,
}

#[derive(Default)]
struct Frontend {
    configuration: BTreeMap<String, String>,
    id: u32,
    client_id: u16,
    overlay_role: bool,
    native_mode: bool,
    native_owner_role: bool,
    native_owner: native::Owner,
    native_owner_id: Option<u32>,
    native_client: String,
    // Explicit keyboard control owns selection; pointer interaction does not own focus.
    local_selection: bool,
    keyboard_control: bool,
    control_has_focus: bool,
    control_terminal: Option<u32>,
    overlay: overlay::Overlay,
    tooltip: overlay::Controller,
    theme: ui::Theme,
    granted: bool,
    snapshot: Option<Snapshot>,
    state_buffers: BTreeMap<String, String>,
    error: Option<String>,
    queue: VecDeque<Request>,
    in_flight: Option<Request>,
    close_targets: Vec<u32>,
    request_id: u64,
    panes: BTreeMap<u32, String>,
    manifest: PaneManifest,
    tabs: Vec<TabInfo>,
    native_focus: Option<u32>,
    cursor: Option<Item>,
    desired_selection: Option<DesiredSelection>,
    pending_user_action: Option<PendingUserAction>,
    relayed_selection: Option<native::UiSelection>,
    relay_revision: u64,
    relay_epoch: String,
    collapsed: BTreeSet<String>,
    tree_cache: Vec<ui::TreeRow>,
    items: Vec<Item>,
    hover: Option<Item>,
    popup_item: Option<Item>,
    popup_cache: VecDeque<CachedPopup>,
    last_click: Option<(Item, Instant)>,
    modal: Option<Modal>,
    scroll: usize,
    rows: usize,
    columns: usize,
    render_cursor: Option<Option<(usize, usize)>>,
    visible: bool,
}

register_plugin!(Frontend);

impl Frontend {
    fn enter_control(&mut self) {
        if self.keyboard_control {
            return;
        }
        self.control_terminal = self.native_focus;
        self.keyboard_control = true;
        self.control_has_focus = false;
        self.local_selection = true;
        self.hide_tooltip();
        set_selectable(true);
        switch_to_input_mode(&InputMode::Normal);
        focus_plugin_pane(self.id, false, false);
    }

    fn leave_control(&mut self, restore_focus: bool) {
        if !self.keyboard_control {
            return;
        }
        self.keyboard_control = false;
        self.local_selection = false;
        if self.modal.take().is_some() {
            self.hide_tooltip();
        }
        self.control_has_focus = false;
        set_selectable(false);
        if let Some(terminal) = self.control_terminal.take()
            && restore_focus
        {
            focus_terminal_pane(terminal, false, false);
        }
        self.reconcile_cursor();
    }

    fn control_pipe(&mut self, message: &PipeMessage) -> bool {
        if !self.granted || !self.visible || message.source != PipeSource::Keybind {
            return false;
        }
        let active_sidebar = self.tabs.iter().find(|tab| tab.active).is_some_and(|tab| {
            self.manifest.panes.get(&tab.position).is_some_and(|panes| {
                panes
                    .iter()
                    .any(|pane| pane.is_plugin && pane.id == self.id && !pane.is_suppressed)
            })
        });
        if !active_sidebar {
            return false;
        }
        match message.payload.as_deref() {
            Some("toggle") if !self.keyboard_control => self.enter_control(),
            Some("toggle" | "leave") => {
                self.leave_control(true);
                switch_to_input_mode(&InputMode::Locked);
            }
            _ => return false,
        }
        true
    }

    fn enqueue(&mut self, action: Action) {
        // Background registration/focus notifications can arrive more than once.
        if !self.native_mode
            && matches!(
                &action,
                Action::RegisterPane { .. } | Action::FocusPane { .. } | Action::Snapshot
            )
            && (self.queue.iter().any(|request| request.action == action)
                || self
                    .in_flight
                    .as_ref()
                    .is_some_and(|request| request.action == action))
        {
            return;
        }
        self.request_id = self.request_id.saturating_add(1);
        let item = match &action {
            Action::SelectWorkspace { workspace_id } => Some(Item::Workspace(workspace_id.clone())),
            Action::SelectPane { pane_id } => Some(Item::Pane(pane_id.clone())),
            _ => None,
        };
        if let Some(item) = item {
            self.cursor = Some(item.clone());
            self.relayed_selection = None;
            self.local_selection = true;
            self.desired_selection = Some(DesiredSelection {
                item,
                request_id: self.request_id,
                acknowledged: false,
            });
        }
        if Self::user_action(&action) {
            self.begin_user_action(Instant::now());
        }
        self.queue.push_back(Request::new(self.request_id, action));
        self.dispatch();
    }

    fn user_action(action: &Action) -> bool {
        matches!(
            action,
            Action::SelectWorkspace { .. }
                | Action::SelectPane { .. }
                | Action::CreateWorkspace { .. }
                | Action::CreatePane { .. }
                | Action::RenameWorkspace { .. }
                | Action::RenamePane { .. }
                | Action::CloseWorkspace { .. }
                | Action::ClosePane { .. }
        )
    }

    fn begin_user_action(&mut self, now: Instant) {
        if self.pending_user_action.is_none() {
            self.pending_user_action = Some(PendingUserAction {
                started: now,
                shown: false,
            });
            set_timeout(2.0);
        }
    }

    fn user_action_pending(&self) -> bool {
        self.desired_selection.is_some()
            || self
                .in_flight
                .as_ref()
                .is_some_and(|request| Self::user_action(&request.action))
            || self
                .queue
                .iter()
                .any(|request| Self::user_action(&request.action))
    }

    fn finish_user_action(&mut self) {
        if !self.user_action_pending() {
            self.pending_user_action = None;
        }
    }

    fn synchronizing(&self, now: Instant) -> bool {
        self.user_action_pending()
            && self.pending_user_action.as_ref().is_some_and(|pending| {
                now.saturating_duration_since(pending.started) >= Duration::from_secs(2)
            })
    }

    fn expire_user_action(&mut self, now: Instant) -> bool {
        if self.synchronizing(now)
            && let Some(pending) = &mut self.pending_user_action
            && !pending.shown
        {
            pending.shown = true;
            return true;
        }
        false
    }

    fn finish_selection(&mut self, request_id: u64, failed: bool) {
        if self
            .desired_selection
            .as_ref()
            .is_some_and(|desired| desired.request_id == request_id)
        {
            if failed {
                self.desired_selection = None;
                if self.relayed_selection.as_ref().is_some_and(|relay| {
                    relay.client == self.native_client && relay.request_id == request_id
                }) {
                    self.relayed_selection = None;
                }
                self.cursor = None;
                self.local_selection = self.keyboard_control || self.modal.is_some();
            } else if let Some(desired) = &mut self.desired_selection {
                desired.acknowledged = true;
            }
        }
        self.reconcile_cursor();
        self.finish_user_action();
    }

    fn reset_pending_actions(&mut self) {
        self.queue.clear();
        self.in_flight = None;
        self.desired_selection = None;
        self.relayed_selection = None;
        self.relay_revision = 0;
        self.relay_epoch.clear();
        self.pending_user_action = None;
        self.local_selection = self.keyboard_control || self.modal.is_some();
    }

    fn accept_selection_relay(
        &mut self,
        epoch: String,
        revision: u64,
        selection: Option<native::UiSelection>,
    ) -> bool {
        // Other client-owner replicas publish canonical state, not our relay channel.
        if epoch.is_empty() {
            return false;
        }
        let epoch_changed = epoch != self.relay_epoch;
        if epoch_changed {
            self.relay_epoch = epoch;
            self.relay_revision = 0;
            self.relayed_selection = None;
        }
        if !epoch_changed && revision <= self.relay_revision {
            return false;
        }
        self.relay_revision = revision;
        self.relayed_selection = selection.filter(|relay| {
            relay.revision == revision
                && self.desired_selection.as_ref().is_none_or(|desired| {
                    relay.client == self.native_client && relay.request_id >= desired.request_id
                })
        });
        self.reconcile_cursor();
        self.ensure_cursor_visible();
        self.refresh_tooltip();
        true
    }

    fn dispatch(&mut self) {
        if !self.granted || self.in_flight.is_some() {
            return;
        }
        let Some(request) = self.queue.pop_front() else {
            return;
        };
        self.close_targets = self
            .panes
            .iter()
            .filter_map(|(native, public)| {
                let closes = match &request.action {
                    Action::ClosePane { pane_id } => pane_id == public,
                    Action::CloseWorkspace { workspace_id } => {
                        self.snapshot.as_ref().is_some_and(|state| {
                            state.panes.iter().any(|pane| {
                                &pane.id == public && &pane.workspace_id == workspace_id
                            })
                        })
                    }
                    _ => false,
                };
                closes.then_some(*native)
            })
            .collect();
        if self.native_mode {
            if let Err(error) = native::send(
                self.native_owner_id,
                &self.native_client,
                self.client_id,
                &request,
            ) {
                self.error = Some(format!("Cannot encode native request: {error}"));
                self.finish_selection(request.request_id, true);
            } else {
                self.in_flight = Some(request);
            }
            return;
        }
        let json = match serde_json::to_string(&request) {
            Ok(json) => json,
            Err(error) => {
                self.error = Some(format!("Cannot encode request: {error}"));
                self.finish_selection(request.request_id, true);
                return;
            }
        };
        let Some(program) = self.configuration.get("program") else {
            return;
        };
        let Some(socket) = self.configuration.get("socket") else {
            return;
        };
        let context =
            BTreeMap::from([("shephrd_request".to_owned(), request.request_id.to_string())]);
        run_command(
            &[program, "request", "--socket", socket, "--json", &json],
            context,
        );
        self.in_flight = Some(request);
    }

    fn startup(&mut self) {
        if self.native_mode {
            self.error = None;
            self.enqueue(Action::RegisterFrontend);
            self.enqueue(Action::Snapshot);
            return;
        }
        self.error = None;
        self.enqueue(Action::RegisterFrontend);
        self.enqueue(Action::Snapshot);
        let mappings: Vec<_> = self
            .panes
            .iter()
            .map(|(native, public)| (public.clone(), *native))
            .collect();
        for (pane_id, zellij_pane_id) in mappings {
            self.enqueue(Action::RegisterPane {
                pane_id,
                zellij_pane_id,
            });
        }
    }

    fn native_response(&mut self, envelope: native::NativeResponse) {
        if envelope.client != self.native_client
            || self
                .in_flight
                .as_ref()
                .is_none_or(|request| request.request_id != envelope.response.request_id)
        {
            return;
        }
        let Some(request) = self.in_flight.take() else {
            return;
        };
        let obsolete_selection = matches!(
            request.action,
            Action::SelectWorkspace { .. } | Action::SelectPane { .. }
        ) && self
            .desired_selection
            .as_ref()
            .is_some_and(|desired| desired.request_id != request.request_id);
        let response = envelope.response;
        let mut failed = true;
        if response.protocol != PROTOCOL_VERSION {
            self.error = Some("Unsupported native owner protocol".to_owned());
        } else {
            let valid = response
                .state
                .map(|state| self.accept_snapshot(state, true))
                .unwrap_or(true);
            if let Some(error) = response.error {
                self.error = Some(error);
            } else if valid {
                failed = false;
                self.error = None;
                if matches!(
                    request.action,
                    Action::CreateWorkspace { .. } | Action::CreatePane { .. }
                ) && self.desired_selection.is_none()
                {
                    self.local_selection = false;
                }
                for effect in response.effects {
                    // A superseded click must not move host focus back to an older target.
                    if !obsolete_selection || !matches!(effect, Effect::FocusPane { .. }) {
                        self.effect(effect);
                    }
                }
            }
        }
        self.finish_selection(request.request_id, failed);
        self.ensure_cursor_visible();
        self.dispatch();
    }

    fn validate_snapshot(
        &self,
        state: &Snapshot,
        allow_instance_change: bool,
    ) -> Result<bool, String> {
        if state.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported companion protocol {} (need {PROTOCOL_VERSION})",
                state.protocol
            ));
        }
        if state.instance_id.is_empty() {
            return Err("Missing companion instance identity".to_owned());
        }
        if let Some(current) = &self.snapshot
            && current.instance_id == state.instance_id
        {
            if state.revision < current.revision {
                return Ok(false);
            }
            if state.revision == current.revision {
                return if state == current {
                    Ok(false)
                } else {
                    Err("Conflicting state at the same companion revision".to_owned())
                };
            }
        }
        if !allow_instance_change
            && self
                .snapshot
                .as_ref()
                .is_some_and(|current| current.instance_id != state.instance_id)
        {
            return Err("Companion restarted; press R to reconnect".to_owned());
        }
        let workspaces: BTreeSet<_> = state.workspaces.iter().map(|w| w.id.as_str()).collect();
        let panes: BTreeSet<_> = state.panes.iter().map(|p| p.id.as_str()).collect();
        if workspaces.len() != state.workspaces.len()
            || panes.len() != state.panes.len()
            || workspaces.contains("")
            || panes.contains("")
            || state
                .panes
                .iter()
                .any(|p| !workspaces.contains(p.workspace_id.as_str()))
        {
            return Err("Malformed companion workspace/pane identities".to_owned());
        }
        Ok(true)
    }

    fn accept_snapshot(&mut self, state: Snapshot, allow_instance_change: bool) -> bool {
        match self.validate_snapshot(&state, allow_instance_change) {
            Ok(true) => {
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|current| current.instance_id != state.instance_id)
                {
                    self.reset_pending_actions();
                }
                let follow_focus = self.native_mode
                    && self.desired_selection.is_none()
                    && self.relayed_selection.is_none()
                    && !self.local_selection
                    && self.modal.is_none()
                    && self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.focused_pane_id.as_ref())
                        != state.focused_pane_id.as_ref();
                if follow_focus
                    && let Some(pane) = state
                        .panes
                        .iter()
                        .find(|p| Some(&p.id) == state.focused_pane_id.as_ref())
                {
                    self.collapsed.remove(&pane.workspace_id);
                }
                self.snapshot = Some(state);
                self.reconcile_cursor();
                if follow_focus {
                    self.ensure_cursor_visible();
                }
                self.refresh_tooltip();
                true
            }
            Ok(false) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    fn state_pipe(&mut self, source: &str, payload: Option<String>) -> bool {
        let mut buffer = self.state_buffers.remove(source).unwrap_or_default();
        let ended = payload.is_none();
        if let Some(payload) = payload {
            let buffered = self.state_buffers.values().map(String::len).sum::<usize>();
            if buffer
                .len()
                .saturating_add(payload.len())
                .saturating_add(buffered)
                > MAX_MESSAGE_BYTES
            {
                self.error = Some("Oversized companion state stream ignored".to_owned());
                return true;
            }
            buffer.push_str(&payload);
        }
        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].trim();
            if !line.is_empty() {
                match serde_json::from_str::<Snapshot>(line) {
                    Ok(state) => {
                        self.accept_snapshot(state, false);
                    }
                    Err(error) => {
                        self.error =
                            Some(format!("Malformed companion state update ignored: {error}"))
                    }
                }
            }
            buffer.drain(..=newline);
        }
        if !buffer.trim().is_empty() {
            // CLI -- payloads may be complete without a newline. STDIN chunks retain an
            // incomplete object until the next record boundary or EOF.
            match serde_json::from_str::<Snapshot>(&buffer) {
                Ok(state) => {
                    self.accept_snapshot(state, false);
                }
                Err(error) if error.is_eof() && !ended => {
                    if self.state_buffers.len() < 4 {
                        self.state_buffers.insert(source.to_owned(), buffer);
                    } else {
                        self.error = Some("Too many concurrent companion state streams".to_owned());
                    }
                }
                Err(error) => {
                    self.error = Some(format!("Malformed companion state update ignored: {error}"))
                }
            }
        }
        true
    }

    fn reconcile_cursor(&mut self) {
        let Some(state) = &self.snapshot else { return };
        if self.desired_selection.as_ref().is_some_and(|desired| {
            let exists = match &desired.item {
                Item::Workspace(id) => state.workspaces.iter().any(|workspace| &workspace.id == id),
                Item::Pane(id) => state.panes.iter().any(|pane| &pane.id == id),
            };
            let confirmed = desired.acknowledged
                && state.panes.iter().any(|pane| {
                    state.focused_pane_id.as_ref() == Some(&pane.id)
                        && match &desired.item {
                            Item::Workspace(id) => &pane.workspace_id == id,
                            Item::Pane(id) => &pane.id == id,
                        }
                });
            !exists || confirmed
        }) {
            self.desired_selection = None;
            self.local_selection = self.keyboard_control || self.modal.is_some();
        }
        if self.relayed_selection.as_ref().is_some_and(|relay| {
            state.focused_pane_id.as_ref() == Some(&relay.terminal_id)
                || !state.panes.iter().any(|pane| pane.id == relay.terminal_id)
        }) {
            self.relayed_selection = None;
        }
        self.collapsed
            .retain(|id| state.workspaces.iter().any(|w| &w.id == id));
        self.tree_cache = ui::tree(state, &self.collapsed);
        self.items = ui::items(&self.tree_cache);
        if let Some(desired) = &self.desired_selection {
            self.cursor = if self.items.contains(&desired.item) {
                Some(desired.item.clone())
            } else {
                desired
                    .item
                    .workspace_id(state)
                    .map(|id| Item::Workspace(id.to_owned()))
            };
        } else if let Some(relay) = &self.relayed_selection {
            self.cursor = state
                .panes
                .iter()
                .find(|pane| pane.id == relay.terminal_id)
                .map(|pane| {
                    let item = Item::Pane(pane.id.clone());
                    if self.items.contains(&item) {
                        item
                    } else {
                        Item::Workspace(pane.workspace_id.clone())
                    }
                });
        } else if self.native_mode && !self.local_selection && self.modal.is_none() {
            self.cursor = state
                .focused_pane_id
                .as_ref()
                .and_then(|id| state.panes.iter().find(|pane| &pane.id == id))
                .map(|pane| {
                    let item = Item::Pane(pane.id.clone());
                    if self.items.contains(&item) {
                        item
                    } else {
                        Item::Workspace(pane.workspace_id.clone())
                    }
                })
                .or_else(|| self.items.first().cloned());
        } else if !self
            .cursor
            .as_ref()
            .is_some_and(|cursor| self.items.contains(cursor))
        {
            self.cursor = state
                .focused_pane_id
                .as_ref()
                .map(|id| Item::Pane(id.clone()))
                .filter(|item| self.items.contains(item))
                .or_else(|| self.items.first().cloned());
        }
        if !self
            .hover
            .as_ref()
            .is_some_and(|hover| self.items.contains(hover))
        {
            self.hover = None;
        }
        if self.popup_item.as_ref().is_some_and(|item| match item {
            Item::Workspace(id) => !state.workspaces.iter().any(|workspace| &workspace.id == id),
            Item::Pane(id) => !state.panes.iter().any(|pane| &pane.id == id),
        }) {
            self.popup_item = None;
        }
        self.finish_user_action();
    }

    fn response(
        &mut self,
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: BTreeMap<String, String>,
    ) {
        let Some(id) = context
            .get("shephrd_request")
            .and_then(|id| id.parse::<u64>().ok())
        else {
            return;
        };
        if self
            .in_flight
            .as_ref()
            .is_none_or(|request| request.request_id != id)
        {
            return;
        }
        let Some(request) = self.in_flight.take() else {
            return;
        };
        let close_targets = std::mem::take(&mut self.close_targets);
        let response = if stdout.len() <= MAX_MESSAGE_BYTES {
            from_slice::<Response>(&stdout).ok()
        } else {
            None
        };
        let mut failed = true;
        match response {
            Some(response)
                if response.protocol == PROTOCOL_VERSION
                    && response.request_id == request.request_id =>
            {
                let valid = response
                    .state
                    .map(|state| self.accept_snapshot(state, true))
                    .unwrap_or(true);
                if let Some(error) = response.error {
                    self.error = Some(error);
                } else if code != Some(0) {
                    self.error = Some(format!(
                        "Companion request exited {:?}: {}",
                        code,
                        ui::clean(&String::from_utf8_lossy(&stderr))
                    ));
                } else if valid {
                    failed = false;
                    if !matches!(
                        request.action,
                        Action::RegisterPane { .. }
                            | Action::PaneClosed { .. }
                            | Action::FocusPane { .. }
                            | Action::Snapshot
                    ) {
                        self.error = None;
                    }
                    if matches!(
                        request.action,
                        Action::CreateWorkspace { .. } | Action::CreatePane { .. }
                    ) && let Some(state) = &self.snapshot
                        && let Some(focused) = &state.focused_pane_id
                    {
                        self.cursor = Some(Item::Pane(focused.clone()));
                        if let Some(pane) = state.panes.iter().find(|pane| &pane.id == focused) {
                            self.collapsed.remove(&pane.workspace_id);
                        }
                        self.reconcile_cursor();
                    }
                    for native in close_targets {
                        close_terminal_pane(native);
                    }
                    for effect in response.effects {
                        self.effect(effect);
                    }
                }
            }
            Some(response) => {
                self.error = Some(format!(
                    "Invalid response protocol/request identity ({}/{})",
                    response.protocol, response.request_id
                ))
            }
            None => {
                self.error = Some(format!(
                    "Companion request failed ({code:?}): {}",
                    if stderr.is_empty() {
                        "invalid or oversized JSON response".to_owned()
                    } else {
                        ui::truncate(&String::from_utf8_lossy(&stderr), 512)
                    }
                ))
            }
        }
        self.finish_selection(request.request_id, failed);
        self.dispatch();
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::FocusPane { zellij_pane_id } => {
                let exists = self
                    .manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| !pane.is_plugin && pane.id == zellij_pane_id);
                if exists {
                    self.leave_control(false);
                    switch_to_input_mode(&InputMode::Locked);
                    focus_terminal_pane(zellij_pane_id, false, false);
                } else {
                    self.error = Some(format!(
                        "Native pane {zellij_pane_id} is no longer open; select again"
                    ));
                    self.panes.remove(&zellij_pane_id);
                    if self.native_mode {
                        self.enqueue(Action::Snapshot);
                    } else {
                        self.enqueue(Action::PaneClosed { zellij_pane_id });
                    }
                }
            }
            Effect::OpenPane { pane_id, command } => {
                if self.native_mode {
                    self.error = Some("Native owner returned an attachment effect".to_owned());
                    return;
                }
                if let Some((&native_id, _)) =
                    self.panes.iter().find(|(_, public)| *public == &pane_id)
                {
                    if get_pane_info(PaneId::Terminal(native_id)).is_some() {
                        self.leave_control(false);
                        switch_to_input_mode(&InputMode::Locked);
                        focus_terminal_pane(native_id, false, false);
                        return;
                    }
                    self.panes.remove(&native_id);
                    self.enqueue(Action::PaneClosed {
                        zellij_pane_id: native_id,
                    });
                }
                let tab_name = self.snapshot.as_ref().and_then(|state| {
                    let pane = state.panes.iter().find(|pane| pane.id == pane_id)?;
                    let workspace = state
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == pane.workspace_id)?;
                    Some(format!(
                        "{} · {}",
                        ui::clean(&workspace.name),
                        ui::clean(&pane.name)
                    ))
                });
                let Some(tab_name) = tab_name else {
                    self.error = Some(format!("Attachment {pane_id} has no workspace"));
                    return;
                };
                let sidebar_width = self
                    .manifest
                    .panes
                    .values()
                    .flatten()
                    .find(|pane| pane.is_plugin && pane.id == self.id)
                    .map_or(self.columns, |pane| pane.pane_columns);
                let layout = match shephrd::attachment_layout(
                    &self.configuration["plugin_url"],
                    &self.configuration,
                    &pane_id,
                    &command,
                    &tab_name,
                    sidebar_width,
                ) {
                    Ok(layout) => layout,
                    Err(error) => {
                        self.error = Some(format!("Could not build attachment tab: {error}"));
                        return;
                    }
                };
                switch_to_input_mode(&InputMode::Locked);
                if new_tabs_with_layout(&layout).is_empty() {
                    self.error = Some(format!("Zellij could not open attachment tab {pane_id}"));
                }
            }
            Effect::Attachment { .. } => {
                self.error =
                    Some("Companion returned an attachment-only effect to the sidebar".to_owned())
            }
        }
    }

    fn register_pane(&mut self, native_id: u32, public_id: String) {
        if self.panes.get(&native_id) == Some(&public_id) {
            return;
        }
        self.panes.insert(native_id, public_id.clone());
        self.enqueue(Action::RegisterPane {
            pane_id: public_id,
            zellij_pane_id: native_id,
        });
    }

    fn forget_pane(&mut self, id: u32) {
        if self.panes.remove(&id).is_some() {
            self.enqueue(Action::PaneClosed { zellij_pane_id: id });
        }
        if self.native_focus == Some(id) {
            self.native_focus = None;
        }
    }

    fn close_attachment(&mut self, id: u32) {
        // The command is only a proxy. Dropping it must not kill a live Herdr PTY.
        self.forget_pane(id);
        close_terminal_pane(id);
    }

    fn pane_update(&mut self, manifest: PaneManifest) {
        if self.native_mode {
            self.manifest = manifest;
            self.reconcile_cursor();
            self.synchronize_focus();
            self.refresh_tooltip();
            return;
        }
        let initial: Vec<_> = manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| !pane.is_plugin && !pane.exited)
            .filter_map(|pane| {
                pane.title
                    .strip_prefix("shephrd:")
                    .filter(|id| !id.is_empty())
                    .map(|id| (pane.id, id.to_owned()))
            })
            .collect();
        self.manifest = manifest;
        for (native, public) in initial {
            self.register_pane(native, public);
        }
        let exited: Vec<_> = self
            .manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| {
                !pane.is_plugin
                    && pane.exited
                    && (self.panes.contains_key(&pane.id) || pane.title.starts_with("shephrd:"))
            })
            .map(|pane| pane.id)
            .collect();
        for id in exited {
            self.close_attachment(id);
        }
        self.synchronize_focus();
        self.refresh_tooltip();
    }

    fn synchronize_focus(&mut self) {
        let Some(active_tab) = self.tabs.iter().find(|tab| tab.active) else {
            return;
        };
        let focused = self
            .manifest
            .panes
            .get(&active_tab.position)
            .and_then(|panes| {
                // A tiled pane can remain is_focused behind the visible floating layer.
                panes
                    .iter()
                    .find(|pane| {
                        pane.is_focused
                            && !pane.is_suppressed
                            && pane.is_floating
                            && active_tab.are_floating_panes_visible
                    })
                    .or_else(|| {
                        panes.iter().find(|pane| {
                            pane.is_focused && !pane.is_suppressed && !pane.is_floating
                        })
                    })
            });
        let sidebar_focused = focused.is_some_and(|pane| pane.is_plugin && pane.id == self.id);
        let focus_known = focused.is_some();
        let focused = focused.filter(|pane| !pane.is_plugin).map(|pane| pane.id);
        if self.keyboard_control {
            if sidebar_focused {
                self.control_has_focus = true;
            } else if focus_known && self.control_has_focus {
                self.leave_control(false);
            }
        }
        if self.native_mode
            && self.local_selection
            && !self.keyboard_control
            && focus_known
            && !sidebar_focused
            && self.modal.is_none()
            && !self.selection_pending()
        {
            self.local_selection = false;
            self.reconcile_cursor();
            self.ensure_cursor_visible();
        }
        if focused == self.native_focus {
            return;
        }
        self.native_focus = focused;
        if self.native_mode {
            return;
        }
        let Some(public) = focused.and_then(|native| self.panes.get(&native)).cloned() else {
            return;
        };
        if self
            .snapshot
            .as_ref()
            .and_then(|state| state.focused_pane_id.as_ref())
            != Some(&public)
        {
            self.enqueue(Action::FocusPane { pane_id: public });
        }
    }

    fn selection_pending(&self) -> bool {
        let selects = |request: &Request| {
            matches!(
                request.action,
                Action::SelectWorkspace { .. }
                    | Action::SelectPane { .. }
                    | Action::CreateWorkspace { .. }
                    | Action::CreatePane { .. }
            )
        };
        self.desired_selection.is_some()
            || self.in_flight.as_ref().is_some_and(selects)
            || self.queue.iter().any(selects)
    }

    fn hide_tooltip(&mut self) {
        self.popup_item = None;
        self.tooltip.update(self.id, &self.configuration, None);
    }

    fn accept_dismiss(&mut self, source: &PipeSource, notice: &overlay::DismissNotice) -> bool {
        let PipeSource::Plugin(source) = source else {
            return false;
        };
        if !self.tooltip.accept_dismiss(*source, notice) {
            return false;
        }
        self.hide_tooltip();
        true
    }

    fn accept_ci_refresh(
        &mut self,
        source: &PipeSource,
        request: &overlay::RefreshRequest,
    ) -> Option<shephrd::ci_contract::CiRefreshTarget> {
        let PipeSource::Plugin(source) = source else {
            return None;
        };
        if !self.native_mode || !self.granted || !self.visible || self.modal.is_some() {
            return None;
        }
        let content = ui::tooltip(self.popup_item.as_ref()?, self.snapshot.as_ref()?);
        if content.key != request.key
            || !content
                .rows()
                .iter()
                .any(|row| row.refresh.as_ref() == Some(&request.target))
            || !self.tooltip.accept_refresh(*source, request)
        {
            return None;
        }
        Some(request.target.clone())
    }

    fn refresh_tooltip(&mut self) {
        if !self.granted || self.overlay_role {
            return;
        }
        let cache_index =
            self.popup_item
                .as_ref()
                .zip(self.snapshot.as_ref())
                .map(|(item, snapshot)| {
                    if let Some(index) = self.popup_cache.iter().position(|entry| {
                        entry.item == *item
                            && entry.instance == snapshot.instance_id
                            && entry.revision == snapshot.revision
                    }) {
                        return index;
                    }
                    self.popup_cache.retain(|entry| entry.item != *item);
                    if self.popup_cache.len() == 8 {
                        self.popup_cache.pop_front();
                    }
                    self.popup_cache.push_back(CachedPopup {
                        item: item.clone(),
                        instance: snapshot.instance_id.clone(),
                        revision: snapshot.revision,
                        formatted_at: Self::popup_clock(),
                        content: ui::tooltip(item, snapshot),
                    });
                    self.popup_cache.len() - 1
                });
        let tooltip = if self.visible && self.modal.is_none() {
            self.popup_item
                .as_ref()
                .zip(self.snapshot.as_ref())
                .and_then(|(item, _snapshot)| {
                    let (position, sidebar) =
                        self.manifest.panes.iter().find_map(|(position, panes)| {
                            panes
                                .iter()
                                .find(|pane| pane.is_plugin && pane.id == self.id)
                                .map(|pane| (*position, pane))
                        })?;
                    if sidebar.is_suppressed {
                        return None;
                    }
                    let tab = self
                        .tabs
                        .iter()
                        .find(|tab| tab.position == position && tab.active)?;
                    let content = &self.popup_cache.get(cache_index?)?.content;
                    if content.is_empty() {
                        return None;
                    }
                    let band = ui::item_band(&self.tree_cache, item)?;
                    let content_rows = if self.rows > 0 {
                        self.rows
                    } else {
                        sidebar.pane_content_rows
                    };
                    let height = content_rows.saturating_sub(4);
                    if height == 0
                        || band.end().saturating_add(1) < self.scroll
                        || band.start().saturating_sub(1) >= self.scroll + height
                    {
                        return None;
                    }
                    let top = band
                        .start()
                        .saturating_add(1)
                        .saturating_sub(self.scroll)
                        .max(1);
                    let bottom = band.end().saturating_add(1).min(self.scroll + height - 1)
                        - self.scroll
                        + 2;
                    let placement =
                        overlay::geometry(sidebar, self.columns, tab, top..=bottom, content)?;
                    Some((content, placement))
                })
        } else {
            None
        };
        self.tooltip.update(self.id, &self.configuration, tooltip);
    }

    fn navigate(&mut self, amount: isize) {
        if self.items.is_empty() {
            return;
        }
        let current = self
            .cursor
            .as_ref()
            .and_then(|cursor| self.items.iter().position(|item| item == cursor))
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(amount)
            .min(self.items.len() - 1);
        self.cursor = Some(self.items[next].clone());
        self.ensure_cursor_visible();
        self.hide_tooltip();
    }

    fn ensure_cursor_visible(&mut self) {
        let Some(cursor) = &self.cursor else { return };
        let Some(first) = self
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(cursor))
        else {
            return;
        };
        let height = self.rows.saturating_sub(4);
        if first < self.scroll {
            self.scroll = first;
        } else if first + 1 >= self.scroll + height && height > 0 {
            self.scroll = (first + 2).saturating_sub(height);
        }
    }

    fn activate(&mut self) {
        match self.cursor.clone() {
            Some(Item::Workspace(workspace_id)) => {
                self.enqueue(Action::SelectWorkspace { workspace_id })
            }
            Some(Item::Pane(pane_id)) => self.enqueue(Action::SelectPane { pane_id }),
            None => {}
        }
    }

    fn fold(&mut self, workspace_id: String, folded: Option<bool>) {
        let fold = folded.unwrap_or(!self.collapsed.contains(&workspace_id));
        if fold {
            self.collapsed.insert(workspace_id.clone());
            if self.cursor.as_ref().and_then(|item| {
                self.snapshot
                    .as_ref()
                    .and_then(|state| item.workspace_id(state))
            }) == Some(workspace_id.as_str())
            {
                self.cursor = Some(Item::Workspace(workspace_id));
            }
        } else {
            self.collapsed.remove(&workspace_id);
        }
        self.reconcile_cursor();
        self.ensure_cursor_visible();
        self.refresh_tooltip();
    }

    fn edit_selected(&mut self, close: bool) {
        let Some(item) = self.cursor.clone() else {
            return;
        };
        let Some(state) = &self.snapshot else { return };
        let name = match &item {
            Item::Workspace(id) => state
                .workspaces
                .iter()
                .find(|w| &w.id == id)
                .map(|w| w.name.clone()),
            Item::Pane(id) => state
                .panes
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.name.clone()),
        };
        let Some(name) = name else { return };
        self.modal = Some(if close {
            Modal::Close { item, name }
        } else {
            Modal::Rename {
                item,
                input: ui::Input::new(name),
            }
        });
        self.hide_tooltip();
        switch_to_input_mode(&InputMode::Locked);
    }

    fn modal_key(&mut self, key: KeyWithModifier) {
        if key.bare_key == BareKey::Esc {
            self.modal = None;
            switch_to_input_mode(&InputMode::Normal);
            return;
        }
        if matches!(self.modal, Some(Modal::Close { .. })) && key.bare_key == BareKey::Char('n') {
            self.modal = None;
            switch_to_input_mode(&InputMode::Normal);
            return;
        }
        if key.bare_key == BareKey::Enter
            || (matches!(self.modal, Some(Modal::Close { .. }))
                && key.bare_key == BareKey::Char('y'))
        {
            let Some(modal) = self.modal.take() else {
                return;
            };
            let action = match modal {
                Modal::Create { name, cwd, .. } => Action::CreateWorkspace {
                    label: (!name.value.trim().is_empty()).then(|| name.value.trim().to_owned()),
                    cwd: (!cwd.value.trim().is_empty()).then(|| cwd.value.trim().to_owned()),
                },
                Modal::CreatePane { workspace_id, cwd } => Action::CreatePane {
                    workspace_id,
                    cwd: (!cwd.value.trim().is_empty()).then(|| cwd.value.trim().to_owned()),
                },
                Modal::Rename { item, input } => {
                    let label = input.value.trim().to_owned();
                    if label.is_empty() {
                        self.error = Some("A name cannot be empty".to_owned());
                        self.modal = Some(Modal::Rename { item, input });
                        return;
                    }
                    match item {
                        Item::Workspace(workspace_id) => Action::RenameWorkspace {
                            workspace_id,
                            label,
                        },
                        Item::Pane(pane_id) => Action::RenamePane { pane_id, label },
                    }
                }
                Modal::Close { item, .. } => match item {
                    Item::Workspace(workspace_id) => Action::CloseWorkspace { workspace_id },
                    Item::Pane(pane_id) => Action::ClosePane { pane_id },
                },
            };
            switch_to_input_mode(&InputMode::Normal);
            self.enqueue(action);
            return;
        }
        if key.bare_key == BareKey::Tab {
            if let Some(Modal::Create { cwd_active, .. }) = self.modal.as_mut() {
                *cwd_active = !*cwd_active;
            }
            return;
        }
        let Some(input) = self.modal.as_mut().and_then(Modal::input_mut) else {
            return;
        };
        if key.key_modifiers.contains(&KeyModifier::Ctrl) {
            if matches!(key.bare_key, BareKey::Char('u')) {
                input.value.clear();
                input.cursor = 0;
            }
            return;
        }
        if key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return;
        }
        match key.bare_key {
            BareKey::Char(character) => input.insert(&character.to_string()),
            BareKey::Backspace => input.backspace(),
            BareKey::Delete => input.delete(),
            BareKey::Left => input.left(),
            BareKey::Right => input.right(),
            BareKey::Home => input.cursor = 0,
            BareKey::End => input.cursor = input.value.len(),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyWithModifier) {
        self.last_click = None;
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        if key.key_modifiers.contains(&KeyModifier::Ctrl)
            || key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return;
        }
        self.local_selection = true;
        match key.bare_key {
            BareKey::Up | BareKey::Char('k') => self.navigate(-1),
            BareKey::Down | BareKey::Char('j') => self.navigate(1),
            BareKey::PageUp => self.navigate(-(self.rows.saturating_sub(4) as isize / 2).max(1)),
            BareKey::PageDown => self.navigate((self.rows.saturating_sub(4) as isize / 2).max(1)),
            BareKey::Home | BareKey::Char('g') => self.navigate(isize::MIN),
            BareKey::End | BareKey::Char('G') => self.navigate(isize::MAX),
            BareKey::Enter => self.activate(),
            BareKey::Left | BareKey::Right | BareKey::Char(' ') => {
                let workspace = self
                    .cursor
                    .as_ref()
                    .and_then(|item| {
                        self.snapshot
                            .as_ref()
                            .and_then(|state| item.workspace_id(state))
                    })
                    .map(str::to_owned);
                if let Some(workspace) = workspace {
                    let folded = match key.bare_key {
                        BareKey::Left => Some(true),
                        BareKey::Right => Some(false),
                        _ => None,
                    };
                    self.fold(workspace, folded);
                }
            }
            BareKey::Char('n') => {
                self.modal = Some(Modal::Create {
                    name: ui::Input::default(),
                    cwd: ui::Input::default(),
                    cwd_active: false,
                });
                self.hide_tooltip();
                switch_to_input_mode(&InputMode::Locked);
            }
            BareKey::Char('N') => {
                let workspace = self
                    .cursor
                    .as_ref()
                    .and_then(|item| {
                        self.snapshot
                            .as_ref()
                            .and_then(|state| item.workspace_id(state))
                    })
                    .map(str::to_owned);
                if let Some(workspace_id) = workspace {
                    self.modal = Some(Modal::CreatePane {
                        workspace_id,
                        cwd: ui::Input::default(),
                    });
                    self.hide_tooltip();
                    switch_to_input_mode(&InputMode::Locked);
                }
            }
            BareKey::Char('r') => self.edit_selected(false),
            BareKey::Char('x') | BareKey::Delete => self.edit_selected(true),
            BareKey::Char('R') => {
                self.error = None;
                self.startup();
            }
            BareKey::Esc => self.hide_tooltip(),
            _ => {}
        }
    }

    fn popup_clock() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_secs())
    }

    fn mouse(&mut self, mouse: Mouse) {
        if self.modal.is_some() {
            return;
        }
        let (line, column) = match mouse {
            Mouse::Hover(line, column)
            | Mouse::LeftClick(line, column)
            | Mouse::RightClick(line, column) => (line, column),
            Mouse::ScrollUp(lines) => {
                self.scroll = self.scroll.saturating_sub(lines);
                self.refresh_tooltip();
                return;
            }
            Mouse::ScrollDown(lines) => {
                self.scroll = self.scroll.saturating_add(lines).min(
                    self.tree_cache
                        .len()
                        .saturating_sub(self.rows.saturating_sub(4)),
                );
                self.refresh_tooltip();
                return;
            }
            _ => return,
        };
        let index =
            (line >= 2 && (line as usize) < self.rows.saturating_sub(2) && column < self.columns)
                .then(|| self.scroll + line as usize - 2);
        let item = index.and_then(|index| ui::item_at(&self.tree_cache, index));
        if matches!(mouse, Mouse::Hover(_, _)) {
            self.hover = item;
            return;
        }
        self.hover = item.clone();
        self.hide_tooltip();
        if matches!(mouse, Mouse::RightClick(_, _)) {
            self.last_click = None;
            // Relative last-checked age is event-driven: quiet frozen results are
            // reformatted on explicit reopening, never by hover or a polling timer.
            let now = Self::popup_clock();
            self.popup_cache.retain(|entry| entry.formatted_at == now);
            self.tooltip.begin_open();
            self.popup_item = item;
            self.refresh_tooltip();
            return;
        }
        let Some(item) = item else {
            self.last_click = None;
            return;
        };
        let group = self
            .snapshot
            .as_ref()
            .and_then(|state| item.workspace_id(state).map(str::to_owned));
        self.local_selection = true;
        self.cursor = Some(item.clone());
        let now = Instant::now();
        let double = self.last_click.as_ref().is_some_and(|(old, at)| {
            old == &item && now.duration_since(*at) < Duration::from_millis(350)
        });
        if double {
            self.last_click = None;
            if let Some(group) = group {
                self.fold(group, None);
            }
        } else {
            self.last_click = Some((item, now));
        }
        self.activate();
    }

    fn sidebar_dimensions(&self, rows: usize, columns: usize) -> (usize, usize) {
        self.manifest
            .panes
            .values()
            .flatten()
            .find(|pane| pane.is_plugin && pane.id == self.id)
            .map_or((rows, columns), |pane| {
                (
                    if pane.pane_content_rows > 0 {
                        rows.min(pane.pane_content_rows)
                    } else {
                        rows
                    },
                    if pane.pane_content_columns > 0 {
                        columns.min(pane.pane_content_columns)
                    } else {
                        columns
                    },
                )
            })
    }

    fn render_sidebar(&mut self, rows: usize, columns: usize) {
        self.render_sidebar_to(rows, columns, &mut std::io::stdout());
    }

    fn render_sidebar_to(&mut self, rows: usize, columns: usize, output: &mut impl Write) {
        // Keep text, status cells and gutters inside the reported paintable content clip.
        let (rows, columns) = self.sidebar_dimensions(rows, columns);
        let resized = self.rows != rows || self.columns != columns;
        self.rows = rows;
        self.columns = columns;
        let body_height = rows.saturating_sub(4);
        let old_scroll = self.scroll;
        self.scroll = self
            .scroll
            .min(self.tree_cache.len().saturating_sub(body_height));
        if (resized || self.scroll != old_scroll) && self.popup_item.is_some() {
            // Keep the shared outline in sync before emitting any rendering bytes.
            self.refresh_tooltip();
        }
        let tree = &self.tree_cache;
        let popup_band = self
            .popup_item
            .as_ref()
            .and_then(|item| ui::item_band(tree, item));
        let joined = self
            .tooltip
            .pending
            .as_ref()
            .filter(|state| state.visible && self.tooltip.id.is_some())
            .and_then(|state| state.connection.as_ref())
            .map(|connection| connection.side);
        let selected_group = self.cursor.as_ref().and_then(|item| match item {
            Item::Pane(_) => self
                .snapshot
                .as_ref()
                .and_then(|state| item.workspace_id(state)),
            _ => None,
        });
        let heading = self
            .snapshot
            .as_ref()
            .map(|state| match &state.machine {
                Some(machine) => format!("shephrd · {} · {machine}", state.session),
                None => format!("shephrd · {}", state.session),
            })
            .unwrap_or_else(|| "shephrd".to_owned());
        let status = if let Some(error) = &self.error {
            error.clone()
        } else if !self.granted {
            "Waiting for permissions".to_owned()
        } else if let Some(state) = &self.snapshot {
            if !state.connected {
                state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Herdr disconnected".to_owned())
            } else if self.native_mode && state.error.is_some() {
                state.error.clone().unwrap_or_default()
            } else if self.synchronizing(Instant::now()) {
                "Synchronizing…".to_owned()
            } else {
                String::new()
            }
        } else {
            if self.native_mode {
                "Connecting to native owner…".to_owned()
            } else {
                "Connecting to companion…".to_owned()
            }
        };
        let modal_render = self
            .modal
            .as_ref()
            .map(|modal| modal.lines(columns, self.native_mode));
        // Host commands share stdout with rendering; emit them before any terminal bytes.
        let cursor = modal_render
            .as_ref()
            .and_then(|(_, cursor)| *cursor)
            .filter(|(x, y)| *x < columns && y + 2 < rows.saturating_sub(2))
            .map(|(x, y)| (x, y + 2));
        // None means not initialized; Some(None) records an explicit host cursor hide.
        if self.render_cursor != Some(cursor) {
            self.render_cursor = Some(cursor);
            show_cursor(cursor);
        }
        for row in 0..rows {
            let line = if row == 0 {
                format!("\u{1b}[1m{}{}", ui::fit(&heading, columns), ui::RESET)
            } else if row == 1 {
                let color = if !status.is_empty()
                    && (self.error.is_some()
                        || self.snapshot.as_ref().is_some_and(|state| !state.connected))
                {
                    ui::status_color(shephrd::Status::Blocked)
                } else {
                    ui::MUTED
                };
                format!("{color}{}{}", ui::fit(&status, columns), ui::RESET)
            } else if row == rows.saturating_sub(2) {
                format!(
                    "{}{}{}",
                    ui::MUTED,
                    ui::fit(
                        if self.native_mode {
                            "↑↓ select · Enter focus · Space fold"
                        } else {
                            "↑↓ select · Enter attach · Space fold"
                        },
                        columns
                    ),
                    ui::RESET
                )
            } else if row == rows.saturating_sub(1) {
                format!(
                    "{}{}{}",
                    ui::MUTED,
                    ui::fit("n/N new · r rename · x close · R sync", columns),
                    ui::RESET
                )
            } else if let Some((lines, _)) = &modal_render {
                format!(
                    "{}{}",
                    ui::fit(
                        lines.get(row - 2).map(String::as_str).unwrap_or(""),
                        columns
                    ),
                    ui::RESET
                )
            } else if let (Some(tree_row), Some(state)) =
                (tree.get(self.scroll + row - 2), self.snapshot.as_ref())
            {
                ui::render_row(
                    tree_row,
                    state,
                    &self.theme,
                    columns,
                    tree_row.item.is_some() && tree_row.item == self.cursor,
                    tree_row.item.is_some() && tree_row.item == self.hover,
                    tree_row
                        .workspace_id
                        .as_deref()
                        .is_some_and(|id| selected_group == Some(id)),
                )
            } else {
                ui::fit(
                    if row == 2
                        && self
                            .snapshot
                            .as_ref()
                            .is_some_and(|state| state.workspaces.is_empty())
                    {
                        "No workspaces · n to create"
                    } else {
                        ""
                    },
                    columns,
                )
            };
            write!(
                output,
                "\u{1b}[{};1H{}{}{line}{}",
                row + 1,
                ui::RESET,
                ui::BASE,
                ui::RESET
            )
            .expect("writing sidebar row");
        }
        // Paint the outline last: it floats above guides and adjacent compact rows.
        for row in 1..rows.saturating_sub(2) {
            if self.modal.is_none()
                && columns > 0
                && let Some(band) = &popup_band
            {
                let index = self.scroll as isize + row as isize - 2;
                let first = *band.start() as isize - 1;
                let last = *band.end() as isize + 1;
                if index >= first && index <= last {
                    let top = index == first;
                    let bottom = index == last;
                    let (left, right) = ui::sidebar_edges(top, bottom, joined);
                    // Only neighboring rows carry horizontal edges; own captions stay intact.
                    if (top || bottom) && columns > 2 {
                        let middle = "─".repeat(columns - 2);
                        write!(
                            output,
                            "\u{1b}[{};2H{}{}{middle}{}",
                            row + 1,
                            ui::BASE,
                            ui::BRIGHT,
                            ui::RESET
                        )
                        .expect("writing sidebar horizontal edge");
                    }
                    write!(
                        output,
                        "\u{1b}[{};1H{}{}{left}{}",
                        row + 1,
                        ui::BASE,
                        ui::BRIGHT,
                        ui::RESET
                    )
                    .expect("writing sidebar left edge");
                    if columns > 1 {
                        write!(
                            output,
                            "\u{1b}[{};{}H{}{}{right}{}",
                            row + 1,
                            columns,
                            ui::BASE,
                            ui::BRIGHT,
                            ui::RESET
                        )
                        .expect("writing sidebar right edge");
                    }
                }
            }
        }
    }
}

impl ZellijPlugin for Frontend {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.id = ids.plugin_id;
        self.client_id = ids.client_id;
        self.visible = true;
        self.overlay_role = configuration
            .get("role")
            .is_some_and(|role| role == "overlay");
        self.native_mode = configuration.get("mode").map(String::as_str) == Some("native");
        self.native_owner_role =
            configuration.get("role").map(String::as_str) == Some("native-owner");
        self.native_client = format!(
            "{}:{}",
            self.id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        self.configuration = configuration;
        if self.overlay_role {
            self.overlay.load(&self.configuration, self.id);
            return;
        }
        if self.native_owner_role {
            self.native_owner.load(&self.configuration);
            return;
        }
        set_selectable(true);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Key,
            EventType::ModeUpdate,
            EventType::Mouse,
            EventType::Timer,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::RunCommandResult,
            EventType::CommandPaneExited,
            EventType::PaneClosed,
            EventType::Visible,
            EventType::BeforeClose,
            EventType::PastedText,
        ]);
        let valid = if self.native_mode {
            native::configuration_valid(&self.configuration, "sidebar")
        } else {
            self.configuration.get("role").map(String::as_str) == Some("sidebar")
                && self.configuration.len() == 4
                && self
                    .configuration
                    .get("socket")
                    .is_some_and(|path| Path::new(path).is_absolute())
                && self
                    .configuration
                    .get("program")
                    .is_some_and(|path| Path::new(path).is_absolute())
                && self
                    .configuration
                    .get("plugin_url")
                    .is_some_and(|url| url.starts_with("file:"))
        };
        if !valid {
            self.error = Some(if self.native_mode {
                "Invalid native sidebar configuration".to_owned()
            } else {
                "Invalid sidebar configuration: role/socket/program/plugin_url required".to_owned()
            });
            return;
        }
        let version = get_zellij_version();
        if version != "0.45.1" {
            self.error = Some(format!("Unsupported Zellij {version}; need 0.45.1"));
            return;
        }
        request_permission(PERMISSIONS);
    }

    fn update(&mut self, event: Event) -> bool {
        if self.overlay_role {
            return self.overlay.event(event);
        }
        if self.native_owner_role {
            return self.native_owner.event(event);
        }
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                set_selectable(false);
                switch_to_input_mode(&InputMode::Locked);
                self.startup();
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Required permissions denied; reload to authorize".to_owned())
            }
            Event::RunCommandResult(code, stdout, stderr, context) => {
                self.response(code, stdout, stderr, context)
            }
            Event::PaneUpdate(manifest) if self.granted && self.native_mode => {
                let before = (
                    self.native_focus,
                    self.local_selection,
                    self.scroll,
                    self.popup_item.clone(),
                );
                self.pane_update(manifest);
                return before
                    != (
                        self.native_focus,
                        self.local_selection,
                        self.scroll,
                        self.popup_item.clone(),
                    );
            }
            Event::TabUpdate(tabs) if self.granted && self.native_mode => {
                let before = (
                    self.native_focus,
                    self.local_selection,
                    self.scroll,
                    self.popup_item.clone(),
                );
                if self
                    .tabs
                    .iter()
                    .find(|tab| tab.active)
                    .map(|tab| tab.position)
                    != tabs.iter().find(|tab| tab.active).map(|tab| tab.position)
                {
                    self.hide_tooltip();
                }
                self.tabs = tabs;
                self.synchronize_focus();
                self.refresh_tooltip();
                return before
                    != (
                        self.native_focus,
                        self.local_selection,
                        self.scroll,
                        self.popup_item.clone(),
                    );
            }
            Event::PaneUpdate(manifest) if self.granted => self.pane_update(manifest),
            Event::TabUpdate(tabs) if self.granted => {
                if self
                    .tabs
                    .iter()
                    .find(|tab| tab.active)
                    .map(|tab| tab.position)
                    != tabs.iter().find(|tab| tab.active).map(|tab| tab.position)
                {
                    self.hide_tooltip();
                }
                self.tabs = tabs;
                self.synchronize_focus();
                self.refresh_tooltip();
            }
            Event::CommandPaneExited(id, _, _)
                if !self.native_mode && self.panes.contains_key(&id) =>
            {
                self.close_attachment(id);
            }
            Event::PaneClosed(PaneId::Terminal(id)) if self.granted && !self.native_mode => {
                self.forget_pane(id)
            }
            Event::PaneClosed(PaneId::Plugin(id)) => {
                self.tooltip.forget(id);
                if self.native_mode && self.native_owner_id == Some(id) && self.granted {
                    self.native_owner_id = None;
                    self.reset_pending_actions();
                    self.reconcile_cursor();
                    if let Err(error) = native::restart_owner(&self.configuration, id) {
                        self.error = Some(format!("Cannot restart native owner: {error}"));
                        return true;
                    }
                    self.startup();
                }
            }
            Event::ModeUpdate(mode) if self.granted => {
                if !self.keyboard_control || mode.mode != InputMode::Locked || self.modal.is_some()
                {
                    return false;
                }
                self.leave_control(true);
            }
            Event::Key(key) if self.granted => self.key(key),
            Event::Mouse(mouse) if self.granted => self.mouse(mouse),
            Event::Timer(_) if self.granted => {
                let now = Instant::now();
                return self.expire_user_action(now);
            }
            Event::PastedText(text) if self.granted => {
                if let Some(input) = self.modal.as_mut().and_then(Modal::input_mut) {
                    input.insert(&text);
                }
            }
            Event::Visible(visible) => {
                if self.native_mode && self.visible == visible {
                    return false;
                }
                self.visible = visible;
                if !visible {
                    self.leave_control(false);
                    if self.native_mode {
                        self.local_selection = false;
                        self.reconcile_cursor();
                    }
                    self.hide_tooltip();
                } else if self.native_mode {
                    self.reconcile_cursor();
                    self.ensure_cursor_visible();
                    self.refresh_tooltip();
                }
            }
            Event::BeforeClose if self.granted => {
                self.leave_control(false);
                self.tooltip.close(self.id, &self.configuration);
            }
            _ => return false,
        }
        true
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if self.overlay_role {
            return self.overlay.pipe(message);
        }
        if self.native_owner_role {
            return self.native_owner.pipe(message);
        }
        if message.name == CONTROL_PIPE {
            return self.control_pipe(&message);
        }
        if self.native_mode
            && self.granted
            && matches!(message.name.as_str(), native::RESPONSE | native::STATE)
        {
            let PipeSource::Plugin(owner) = message.source else {
                return false;
            };
            if self.native_owner_id.is_some_and(|id| id != owner) {
                return false;
            }
            let Some(payload) = message.payload.filter(|p| p.len() <= MAX_MESSAGE_BYTES) else {
                return false;
            };
            if message.name == native::RESPONSE {
                let Ok(envelope) = serde_json::from_str::<native::NativeResponse>(&payload) else {
                    return false;
                };
                if envelope.client != self.native_client
                    || envelope.client_id != self.client_id
                    || self
                        .in_flight
                        .as_ref()
                        .is_none_or(|request| request.request_id != envelope.response.request_id)
                {
                    return false;
                }
                self.native_owner_id = Some(owner);
                self.native_response(envelope);
                return true;
            }
            let Ok(state) = serde_json::from_str::<native::NativeState>(&payload) else {
                return false;
            };
            if state.client_id != self.client_id {
                return false;
            }
            // Owner broadcasts also reach other client replicas and repeat the response
            // snapshot. Neither is a reason to render; only a response acknowledges input.
            let changed = self.snapshot.as_ref().is_none_or(|current| {
                current.instance_id != state.snapshot.instance_id
                    || current.revision < state.snapshot.revision
            });
            let discovered = self.native_owner_id.is_none();
            if !self.accept_snapshot(state.snapshot, true) {
                return true;
            }
            let relay_changed = self.accept_selection_relay(
                state.ui_selection_epoch,
                state.ui_selection_revision,
                state.ui_selection,
            );
            self.native_owner_id = Some(owner);
            if discovered
                && let Some(request) = self.in_flight.as_ref()
                && let Err(error) =
                    native::send(Some(owner), &self.native_client, self.client_id, request)
            {
                self.error = Some(format!("Cannot encode native request: {error}"));
                return true;
            }
            return changed || relay_changed;
        }
        if message.name == overlay::READY {
            if let (PipeSource::Plugin(source), Some(payload)) = (message.source, message.payload)
                && let Ok(ready) = serde_json::from_str::<overlay::Ready>(&payload)
                && ready.id == source
                && self.tooltip.ready(&ready, self.id, &self.configuration)
            {
                return true;
            }
            return false;
        }
        if message.name == overlay::REFRESH {
            if let Some(payload) = message.payload.filter(|payload| payload.len() <= 16 * 1024)
                && let Ok(request) = serde_json::from_str::<overlay::RefreshRequest>(&payload)
                && let Some(target) = self.accept_ci_refresh(&message.source, &request)
            {
                if let Err(error) =
                    native::refresh_ci(self.native_owner_id, self.client_id, &target)
                {
                    self.error = Some(format!("Cannot encode CI refresh: {error}"));
                }
                return true;
            }
            return false;
        }
        if message.name == overlay::DISMISS {
            return message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= 4096)
                .and_then(|payload| serde_json::from_str::<overlay::DismissNotice>(payload).ok())
                .is_some_and(|notice| self.accept_dismiss(&message.source, &notice));
        }
        if self.native_mode || message.name != "shephrd-state" || !self.granted {
            return false;
        }
        let PipeSource::Cli(source) = message.source else {
            return false;
        };
        let repaint = self.state_pipe(&source, message.payload);
        unblock_cli_pipe_input(&source);
        repaint
    }

    fn render(&mut self, rows: usize, columns: usize) {
        if self.overlay_role {
            self.overlay.render(rows, columns);
        } else if !self.native_owner_role {
            self.render_sidebar(rows, columns);
        }
        std::io::stdout().flush().expect("flushing plugin render");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shephrd::{Pane, Status, Workspace};

    fn snapshot(revision: u64, focused: &str) -> Snapshot {
        Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision,
            session: "test".to_owned(),
            machine: None,
            connected: true,
            error: None,
            focused_pane_id: Some(focused.to_owned()),
            workspaces: (1..=2)
                .map(|id| Workspace {
                    id: format!("workspace_{id}"),
                    name: format!("Workspace {id}"),
                    active_tab_id: format!("tab_{id}"),
                    projects: Vec::new(),
                    status: Status::Idle,
                    attention: None,
                })
                .collect(),
            panes: (1..=2)
                .map(|id| Pane {
                    id: format!("terminal_{id}"),
                    terminal_id: format!("terminal_{id}"),
                    workspace_id: format!("workspace_{id}"),
                    tab_id: format!("tab_{id}"),
                    name: format!("Terminal {id}"),
                    description: String::new(),
                    harness: None,
                    status: Status::Idle,
                    attention: None,
                    cwd: None,
                    branch: None,
                    branch_changes: None,
                    project: None,
                    ci: None,
                })
                .collect(),
        }
    }

    fn frontend() -> Frontend {
        let mut frontend = Frontend {
            native_mode: true,
            id: 99,
            rows: 20,
            columns: 40,
            ..Frontend::default()
        };
        frontend.accept_snapshot(snapshot(1, "terminal_1"), true);
        frontend
    }

    fn popup_frontend() -> Frontend {
        let mut frontend = frontend();
        frontend.granted = true;
        frontend.visible = true;
        frontend.render_cursor = Some(None);
        frontend.tabs = vec![TabInfo {
            active: true,
            display_area_rows: 24,
            display_area_columns: 120,
            ..TabInfo::default()
        }];
        frontend.manifest.panes.insert(
            0,
            vec![
                PaneInfo {
                    id: frontend.id,
                    is_plugin: true,
                    pane_content_rows: frontend.rows,
                    pane_content_columns: frontend.columns,
                    ..PaneInfo::default()
                },
                PaneInfo {
                    id: 1,
                    is_focused: true,
                    ..PaneInfo::default()
                },
            ],
        );
        frontend
    }

    fn ci_refresh_frontend() -> (Frontend, overlay::RefreshRequest) {
        use shephrd::ci_contract::{
            CiFetchState, CiSnapshot, CiStatus, ForgeProvider, ForgeRepository,
        };
        let mut frontend = popup_frontend();
        frontend.snapshot.as_mut().unwrap().panes[0].ci = Some(CiSnapshot {
            repository: ForgeRepository {
                provider: ForgeProvider::Github,
                host: "forge.example.com".to_owned(),
                path: "owner/repo".to_owned(),
                web_url: "https://forge.example.com/owner/repo".to_owned(),
            },
            head_sha: "0123456789abcdef".to_owned(),
            branch: Some("main".to_owned()),
            status: CiStatus::Passed,
            fetch_state: CiFetchState::Ready,
            checks: Vec::new(),
            change_request: None,
            updated_at: 0,
            stale: false,
            error: None,
            truncated: false,
        });
        frontend.tooltip.begin_open();
        frontend.popup_item = Some(Item::Pane("terminal_1".to_owned()));
        frontend.refresh_tooltip();
        frontend.tooltip.id = Some(100);
        let state = frontend.tooltip.pending.as_ref().unwrap();
        let request = overlay::RefreshRequest {
            instance: state.instance.clone(),
            revision: state.revision,
            key: state.content.key.clone(),
            target: state
                .content
                .rows()
                .into_iter()
                .find_map(|row| row.refresh)
                .unwrap(),
        };
        (frontend, request)
    }

    #[test]
    fn ci_refresh_validates_source_live_popup_revision_and_exact_current_head() {
        for case in 0..17 {
            let (mut frontend, mut request) = ci_refresh_frontend();
            let mut source = PipeSource::Plugin(100);
            match case {
                0 => source = PipeSource::Plugin(101),
                1 => source = PipeSource::Cli("forged".to_owned()),
                2 => request.instance = "old-instance".to_owned(),
                3 => request.revision = request.revision.saturating_sub(1),
                4 => request.key = "pane:terminal_2".to_owned(),
                5 => request.target.head_sha = "old-head".to_owned(),
                6 => request.target.repository.host = "other.example.com".to_owned(),
                7 => request.target.repository.path = "other/repo".to_owned(),
                8 => {
                    request.target.repository.web_url =
                        "https://other.example.com/owner/repo".to_owned()
                }
                9 => frontend.tooltip.pending.as_mut().unwrap().visible = false,
                10 => frontend.visible = false,
                11 => frontend.popup_item = Some(Item::Pane("terminal_2".to_owned())),
                12 => {
                    frontend.snapshot.as_mut().unwrap().panes[0]
                        .ci
                        .as_mut()
                        .unwrap()
                        .head_sha = "new-head".to_owned()
                }
                13 => {
                    frontend.snapshot.as_mut().unwrap().panes[0]
                        .ci
                        .as_mut()
                        .unwrap()
                        .fetch_state = shephrd::ci_contract::CiFetchState::Loading
                }
                14 => frontend.native_mode = false,
                15 => frontend.granted = false,
                16 => {
                    frontend.modal = Some(Modal::Close {
                        item: Item::Pane("terminal_1".to_owned()),
                        name: "Terminal".to_owned(),
                    })
                }
                _ => unreachable!(),
            }
            assert!(
                frontend.accept_ci_refresh(&source, &request).is_none(),
                "case {case}"
            );
        }
        let (mut frontend, request) = ci_refresh_frontend();
        let mut unsupported = serde_json::to_value(&request).unwrap();
        unsupported["target"]["repository"]["provider"] = serde_json::json!("unsupported-provider");
        assert!(serde_json::from_value::<overlay::RefreshRequest>(unsupported).is_err());
        let cursor = frontend.cursor.clone();
        assert_eq!(
            frontend.accept_ci_refresh(&PipeSource::Plugin(100), &request),
            Some(request.target.clone())
        );
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &request)
                .is_none()
        );
        assert!(frontend.tooltip.pending.as_ref().unwrap().visible);
        assert_eq!(frontend.cursor, cursor);
        assert!(!frontend.keyboard_control);
        assert!(!frontend.control_has_focus);
    }

    #[test]
    fn registered_overlay_cannot_be_replaced_to_forge_refresh_source() {
        let (mut frontend, request) = ci_refresh_frontend();
        let mut forged = native_message(overlay::READY, "101");
        forged.source = PipeSource::Plugin(101);
        forged.payload = Some("101".to_owned());
        assert!(!frontend.pipe(forged));
        assert_eq!(frontend.tooltip.id, Some(100));
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(101), &request)
                .is_none()
        );
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &request)
                .is_some()
        );
    }

    #[test]
    fn ci_refresh_dedup_lasts_until_backend_loading_then_allows_completed_results() {
        let (mut frontend, request) = ci_refresh_frontend();
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &request)
                .is_some()
        );
        frontend.snapshot.as_mut().unwrap().panes[0].description =
            "Updated native activity".to_owned();
        frontend.snapshot.as_mut().unwrap().revision += 1;
        frontend.refresh_tooltip();
        let mut current = request.clone();
        current.revision = frontend.tooltip.pending.as_ref().unwrap().revision;
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &current)
                .is_none()
        );
        frontend.snapshot.as_mut().unwrap().panes[0]
            .ci
            .as_mut()
            .unwrap()
            .fetch_state = shephrd::ci_contract::CiFetchState::Loading;
        frontend.snapshot.as_mut().unwrap().revision += 1;
        frontend.refresh_tooltip();
        current.revision = frontend.tooltip.pending.as_ref().unwrap().revision;
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &current)
                .is_none()
        );
        frontend.snapshot.as_mut().unwrap().panes[0]
            .ci
            .as_mut()
            .unwrap()
            .fetch_state = shephrd::ci_contract::CiFetchState::Ready;
        frontend.snapshot.as_mut().unwrap().revision += 1;
        frontend.refresh_tooltip();
        current.revision = frontend.tooltip.pending.as_ref().unwrap().revision;
        assert!(
            frontend
                .accept_ci_refresh(&PipeSource::Plugin(100), &current)
                .is_some()
        );
        assert!(frontend.tooltip.pending.as_ref().unwrap().visible);
    }

    fn context_click(frontend: &mut Frontend, item: &Item) {
        let row = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(item))
            .unwrap();
        frontend.mouse(Mouse::RightClick((row + 2 - frontend.scroll) as isize, 5));
    }

    #[test]
    fn hover_is_highlight_only_and_right_click_preserves_selection_and_open_target() {
        let mut frontend = popup_frontend();
        let item = Item::Pane("terminal_2".to_owned());
        let row = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap();
        let cursor = frontend.cursor.clone();
        frontend.mouse(Mouse::Hover((row + 2) as isize, 5));
        assert_eq!(frontend.hover, Some(item.clone()));
        assert!(frontend.popup_item.is_none());
        assert!(
            frontend
                .tooltip
                .pending
                .as_ref()
                .is_none_or(|state| !state.visible)
        );
        context_click(&mut frontend, &item);
        assert_popup(&frontend, &item);
        assert_eq!(frontend.cursor, cursor);
        assert!(!frontend.local_selection);
        assert!(!frontend.keyboard_control);
        assert!(frontend.queue.is_empty());
        assert!(frontend.in_flight.is_none());
        frontend.mouse(Mouse::Hover(2, 5));
        assert_ne!(frontend.hover, Some(item.clone()));
        assert_popup(&frontend, &item);
        frontend.mouse(Mouse::Hover(0, u16::MAX as usize));
        assert!(frontend.hover.is_none());
        assert_popup(&frontend, &item);
        let mut updated = frontend.snapshot.clone().unwrap();
        updated.revision += 1;
        updated.panes[1].description = "fresh metadata for original target".to_owned();
        frontend.accept_snapshot(updated, true);
        assert_popup(&frontend, &item);
        assert!(
            frontend
                .tooltip
                .pending
                .as_ref()
                .unwrap()
                .content
                .rows()
                .iter()
                .any(|row| row.plain().contains("fresh metadata for original target"))
        );
        frontend.granted = false; // Queue normal selection without invoking the host transport.
        click_item(&mut frontend, item.clone());
        assert!(frontend.popup_item.is_none());
        assert_eq!(frontend.cursor, Some(item));
        assert!(frontend.desired_selection.is_some());
    }

    #[test]
    fn cached_context_reopens_then_async_snapshot_refreshes_only_the_open_item() {
        let mut frontend = popup_frontend();
        let first = Item::Pane("terminal_1".to_owned());
        let second = Item::Pane("terminal_2".to_owned());
        context_click(&mut frontend, &first);
        let original = frontend.tooltip.pending.as_ref().unwrap().content.clone();
        context_click(&mut frontend, &second);
        context_click(&mut frontend, &first);
        assert_eq!(frontend.tooltip.pending.as_ref().unwrap().content, original);
        let before = frontend.tooltip.pending.as_ref().unwrap().revision;
        for line in [2, 3, 4, 6, 8, -1] {
            frontend.mouse(Mouse::Hover(line, 5));
            assert_eq!(frontend.tooltip.pending.as_ref().unwrap().revision, before);
            assert_eq!(frontend.tooltip.pending.as_ref().unwrap().content, original);
        }
        let mut snapshot = frontend.snapshot.clone().unwrap();
        snapshot.revision += 1;
        snapshot.panes[0].description = "async original target update 日本語".to_owned();
        frontend.accept_snapshot(snapshot, true);
        assert!(
            frontend
                .tooltip
                .pending
                .as_ref()
                .unwrap()
                .content
                .rows()
                .iter()
                .any(|row| row.plain().contains("async original target update 日本語"))
        );
        context_click(&mut frontend, &second);
        assert!(
            !frontend
                .tooltip
                .pending
                .as_ref()
                .unwrap()
                .content
                .rows()
                .iter()
                .any(|row| row.plain().contains("async original target update 日本語"))
        );
        context_click(&mut frontend, &first);
        assert!(
            frontend
                .tooltip
                .pending
                .as_ref()
                .unwrap()
                .content
                .rows()
                .iter()
                .any(|row| row.plain().contains("async original target update 日本語"))
        );
    }

    #[test]
    fn dismissal_rejects_stale_card_revisions_keys_instances_and_senders() {
        for case in 0..5 {
            let (mut frontend, _) = ci_refresh_frontend();
            let state = frontend.tooltip.pending.as_ref().unwrap();
            let mut notice = overlay::DismissNotice {
                instance: state.instance.clone(),
                key: state.content.key.clone(),
                revision: state.revision,
            };
            let mut source = PipeSource::Plugin(100);
            match case {
                0 => source = PipeSource::Plugin(101),
                1 => notice.instance = "old".to_owned(),
                2 => notice.key = "pane:terminal_2".to_owned(),
                3 => notice.revision = notice.revision.saturating_sub(1),
                4 => source = PipeSource::Cli("forged".to_owned()),
                _ => unreachable!(),
            }
            assert!(!frontend.accept_dismiss(&source, &notice));
            assert!(frontend.popup_item.is_some());
        }
        let (mut frontend, _) = ci_refresh_frontend();
        let state = frontend.tooltip.pending.as_ref().unwrap();
        let old = overlay::DismissNotice {
            instance: state.instance.clone(),
            key: state.content.key.clone(),
            revision: state.revision,
        };
        frontend.snapshot.as_mut().unwrap().panes[0].description = "metadata arrived".to_owned();
        frontend.snapshot.as_mut().unwrap().revision += 1;
        frontend.refresh_tooltip();
        assert!(frontend.tooltip.accept_dismiss(100, &old));
        context_click(&mut frontend, &Item::Pane("terminal_1".to_owned()));
        assert!(!frontend.accept_dismiss(&PipeSource::Plugin(100), &old));
        let state = frontend.tooltip.pending.as_ref().unwrap();
        let current = overlay::DismissNotice {
            instance: state.instance.clone(),
            key: state.content.key.clone(),
            revision: state.revision,
        };
        assert!(frontend.accept_dismiss(&PipeSource::Plugin(100), &current));
        assert!(frontend.popup_item.is_none());
        assert!(!frontend.tooltip.pending.as_ref().unwrap().visible);
    }

    fn assert_popup(frontend: &Frontend, item: &Item) {
        assert_eq!(frontend.popup_item.as_ref(), Some(item));
        let popup = frontend.tooltip.pending.as_ref().unwrap();
        assert!(popup.visible);
        assert_eq!(
            popup.content,
            ui::tooltip(item, frontend.snapshot.as_ref().unwrap())
        );
    }

    // Consume the actual frame's cursor positioning and styles into terminal cells.
    fn sidebar_cells(frontend: &mut Frontend, rows: usize, columns: usize) -> Vec<Vec<char>> {
        let mut output = Vec::new();
        frontend.render_sidebar_to(rows, columns, &mut output);
        let text = String::from_utf8(output).unwrap();
        let mut cells = vec![vec![' '; columns]; rows];
        let (mut row, mut column) = (0, 0);
        let mut characters = text.chars();
        while let Some(character) = characters.next() {
            if character == '\u{1b}' {
                assert_eq!(characters.next(), Some('['));
                let mut parameters = String::new();
                for character in characters.by_ref() {
                    if character.is_ascii_alphabetic() {
                        match character {
                            'H' => {
                                let mut position = parameters.split(';');
                                row = position.next().unwrap().parse::<usize>().unwrap() - 1;
                                column = position.next().unwrap().parse::<usize>().unwrap() - 1;
                            }
                            'm' => {}
                            _ => panic!("unexpected sidebar terminal command"),
                        }
                        break;
                    }
                    parameters.push(character);
                }
            } else {
                assert!(
                    row < rows && column < columns,
                    "frame writes outside sidebar"
                );
                cells[row][column] = character;
                column += unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
            }
        }
        cells
    }

    #[test]
    fn compact_outline_paints_above_guide_and_neighbor_but_preserves_own_caption() {
        let mut frontend = frontend();
        frontend.render_cursor = Some(None);
        let mut state = snapshot(2, "terminal_1");
        state.panes[1].workspace_id = "workspace_1".to_owned();
        state.panes[0].branch = Some("feature/main".to_owned());
        assert!(frontend.accept_snapshot(state, true));
        let item = Item::Pane("terminal_1".to_owned());
        let band = ui::item_band(&frontend.tree_cache, &item).unwrap();
        let top = band.start() - 1 + 2;
        let bottom = band.end() + 1 + 2;
        let baseline = sidebar_cells(&mut frontend, 20, 40);
        assert_eq!(baseline[top][1], '│');
        assert!(
            baseline[bottom]
                .iter()
                .collect::<String>()
                .contains("Terminal 2")
        );
        frontend.popup_item = Some(item.clone());
        let outlined = sidebar_cells(&mut frontend, 20, 40);
        assert!(outlined[top][1..39].iter().all(|cell| *cell == '─'));
        assert!(outlined[bottom][1..39].iter().all(|cell| *cell == '─'));
        for index in band.clone() {
            assert_eq!(&outlined[index + 2][1..39], &baseline[index + 2][1..39]);
        }
        assert!(
            outlined[*band.start() + 2]
                .iter()
                .collect::<String>()
                .contains("Terminal 1")
        );
        assert!(
            outlined[*band.end() + 2]
                .iter()
                .collect::<String>()
                .contains("feature/main")
        );
        // Adjacent row hit-testing remains its actual owner despite the painted overlap.
        frontend.mouse(Mouse::Hover(bottom as isize, 5));
        assert_eq!(frontend.hover, Some(Item::Pane("terminal_2".to_owned())));
        frontend.hide_tooltip();
        assert_eq!(sidebar_cells(&mut frontend, 20, 40), baseline);
        // At the viewport edge, omit the clipped horizontal edge, never cover our caption.
        frontend.popup_item = Some(item);
        frontend.scroll = *band.start();
        let clipped = sidebar_cells(&mut frontend, 8, 40);
        assert!(clipped[2].iter().collect::<String>().contains("Terminal 1"));
    }

    #[test]
    fn first_workspace_outline_has_a_top_edge_above_its_caption() {
        let mut frontend = frontend();
        frontend.render_cursor = Some(None);
        let baseline = sidebar_cells(&mut frontend, 20, 40);
        frontend.popup_item = Some(Item::Workspace("workspace_1".to_owned()));
        let outlined = sidebar_cells(&mut frontend, 20, 40);
        assert_eq!(outlined[1][0], '╭');
        assert_eq!(outlined[1][39], '╮');
        assert!(outlined[1][1..39].iter().all(|cell| *cell == '─'));
        assert_eq!(&outlined[2][1..39], &baseline[2][1..39]);
        assert_eq!(outlined[0], baseline[0]);
    }

    #[test]
    fn sidebar_render_dimensions_stay_inside_native_content_clip() {
        let mut frontend = frontend();
        frontend
            .manifest
            .panes
            .entry(0)
            .or_default()
            .push(PaneInfo {
                id: frontend.id,
                is_plugin: true,
                pane_content_columns: 37,
                pane_content_rows: 18,
                ..PaneInfo::default()
            });
        assert_eq!(frontend.sidebar_dimensions(20, 38), (18, 37));
        assert_eq!(frontend.sidebar_dimensions(16, 16), (16, 16));
        assert_eq!(frontend.sidebar_dimensions(0, 0), (0, 0));
        frontend.manifest.panes.clear();
        assert_eq!(frontend.sidebar_dimensions(20, 38), (20, 38));
    }

    #[test]
    fn hovering_actual_source_rows_retains_item_and_terminal_selection() {
        let mut frontend = frontend();
        let item = Item::Pane("terminal_1".to_owned());
        let cursor = frontend.cursor.clone();
        let band = ui::item_band(&frontend.tree_cache, &item).unwrap();
        frontend.hover = Some(item.clone());
        for source_row in band {
            let line = source_row - frontend.scroll + 2;
            frontend.mouse(Mouse::Hover(line as isize, 0));
            assert_eq!(frontend.hover, Some(item.clone()));
            assert_eq!(frontend.cursor, cursor);
            assert!(!frontend.keyboard_control);
            assert!(!frontend.control_has_focus);
        }
    }

    #[test]
    fn compact_pointer_mapping_excludes_headers_and_resolves_actual_neighbor() {
        let mut frontend = frontend();
        let cursor = frontend.cursor.clone();
        let workspace = Item::Workspace("workspace_1".to_owned());
        frontend.tree_cache = ui::tree(
            frontend.snapshot.as_ref().unwrap(),
            &BTreeSet::from(["workspace_1".to_owned()]),
        );
        assert_eq!(ui::item_band(&frontend.tree_cache, &workspace), Some(0..=0));
        frontend.mouse(Mouse::Hover(2, 0));
        assert_eq!(frontend.hover, Some(workspace));
        frontend.mouse(Mouse::Hover(3, 0));
        assert_eq!(
            frontend.hover,
            Some(Item::Workspace("workspace_2".to_owned()))
        );
        for line in [0, 1, 18, 19] {
            frontend.mouse(Mouse::Hover(line, 0));
            assert_eq!(frontend.hover, None);
            frontend.mouse(Mouse::LeftClick(line, 0));
            assert_eq!(frontend.cursor, cursor);
        }
        frontend.mouse(Mouse::LeftClick(17, 0));
        assert_eq!(frontend.cursor, cursor);
    }

    fn host_focus(frontend: &mut Frontend, pane_id: PaneId) {
        frontend.tabs = vec![TabInfo {
            active: true,
            ..TabInfo::default()
        }];
        let (id, is_plugin) = match pane_id {
            PaneId::Terminal(id) => (id, false),
            PaneId::Plugin(id) => (id, true),
        };
        frontend.manifest.panes.insert(
            0,
            vec![PaneInfo {
                id,
                is_plugin,
                is_focused: true,
                ..PaneInfo::default()
            }],
        );
        frontend.synchronize_focus();
    }

    fn native_message(name: &str, payload: impl serde::Serialize) -> PipeMessage {
        PipeMessage {
            source: PipeSource::Plugin(100),
            name: name.to_owned(),
            payload: Some(serde_json::to_string(&payload).unwrap()),
            args: BTreeMap::new(),
            is_private: false,
        }
    }

    #[test]
    fn native_publication_does_not_acknowledge_input_or_redraw_unchanged_state() {
        let mut frontend = frontend();
        frontend.granted = true;
        frontend.in_flight = Some(Request::new(7, Action::Snapshot));
        for (client_id, state) in [
            (frontend.client_id + 1, snapshot(2, "terminal_2")),
            (frontend.client_id, snapshot(1, "terminal_1")),
        ] {
            assert!(!frontend.pipe(native_message(
                native::STATE,
                native::NativeState {
                    client_id,
                    snapshot: state,
                    ui_selection: None,
                    ui_selection_revision: 0,
                    ui_selection_epoch: String::new(),
                },
            )));
            assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
            assert_eq!(frontend.in_flight.as_ref().unwrap().request_id, 7);
        }
        let state = snapshot(2, "terminal_2");
        assert!(frontend.pipe(native_message(
            native::STATE,
            native::NativeState {
                client_id: frontend.client_id,
                snapshot: state.clone(),
                ui_selection: None,
                ui_selection_revision: 0,
                ui_selection_epoch: String::new(),
            },
        )));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
        assert_eq!(frontend.in_flight.as_ref().unwrap().request_id, 7);
        assert!(!frontend.pipe(native_message(
            native::STATE,
            native::NativeState {
                client_id: frontend.client_id,
                snapshot: state.clone(),
                ui_selection: None,
                ui_selection_revision: 0,
                ui_selection_epoch: String::new(),
            },
        )));

        for (client_id, request_id) in [
            (frontend.client_id + 1, 7),
            (frontend.client_id, 6),
            (frontend.client_id, 7),
        ] {
            let matches = client_id == frontend.client_id && request_id == 7;
            assert_eq!(
                frontend.pipe(native_message(
                    native::RESPONSE,
                    native::NativeResponse {
                        client: frontend.native_client.clone(),
                        client_id,
                        response: Response {
                            protocol: PROTOCOL_VERSION,
                            request_id,
                            state: Some(state.clone()),
                            effects: Vec::new(),
                            error: None,
                        },
                    },
                )),
                matches,
            );
            assert_eq!(frontend.in_flight.is_none(), matches);
        }
    }

    #[test]
    fn same_revision_focus_conflict_is_reported_not_hidden_as_a_duplicate() {
        let mut frontend = frontend();
        frontend.granted = true;
        assert!(frontend.pipe(native_message(
            native::STATE,
            native::NativeState {
                client_id: frontend.client_id,
                snapshot: snapshot(1, "terminal_2"),
                ui_selection: None,
                ui_selection_revision: 0,
                ui_selection_epoch: String::new(),
            },
        )));
        assert_eq!(
            frontend.error.as_deref(),
            Some("Conflicting state at the same companion revision"),
        );
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
    }

    #[test]
    fn explicit_control_survives_focus_ack_then_releases_on_departure() {
        let mut frontend = frontend();
        host_focus(&mut frontend, PaneId::Terminal(1));
        frontend.enter_control();
        host_focus(&mut frontend, PaneId::Terminal(1));
        assert!(frontend.keyboard_control);
        assert!(!frontend.control_has_focus);

        host_focus(&mut frontend, PaneId::Plugin(99));
        frontend.key(KeyWithModifier::new(BareKey::Home));
        frontend.accept_snapshot(snapshot(2, "terminal_1"), true);
        assert!(frontend.keyboard_control);
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_1".to_owned()))
        );

        host_focus(&mut frontend, PaneId::Terminal(2));
        frontend.accept_snapshot(snapshot(3, "terminal_2"), true);
        assert!(!frontend.keyboard_control);
        assert!(frontend.control_terminal.is_none());
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
    }

    #[test]
    fn existing_replicas_follow_the_same_canonical_focus() {
        let mut source = frontend();
        let mut destination = frontend();
        destination.cursor = Some(Item::Pane("terminal_2".to_owned()));
        destination.reconcile_cursor();
        assert_eq!(destination.cursor, source.cursor);

        let state = snapshot(2, "terminal_2");
        source.accept_snapshot(state.clone(), true);
        destination.accept_snapshot(state, true);
        assert_eq!(source.cursor, Some(Item::Pane("terminal_2".to_owned())));
        assert_eq!(destination.cursor, source.cursor);
    }

    #[test]
    fn sidebar_keyboard_selection_survives_metadata_until_focus_leaves() {
        let mut frontend = frontend();
        host_focus(&mut frontend, PaneId::Plugin(99));
        frontend.key(KeyWithModifier::new(BareKey::Home));
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_1".to_owned()))
        );

        frontend.accept_snapshot(snapshot(2, "terminal_1"), true);
        host_focus(&mut frontend, PaneId::Plugin(99));
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_1".to_owned()))
        );

        host_focus(&mut frontend, PaneId::Terminal(2));
        frontend.accept_snapshot(snapshot(3, "terminal_2"), true);
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
    }

    #[test]
    fn activation_keeps_optimistic_selection_until_already_broadcast_response() {
        let mut frontend = frontend();
        let item = Item::Pane("terminal_2".to_owned());
        let line = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap()
            + 2;
        frontend.mouse(Mouse::LeftClick(line as isize, 5));
        assert_eq!(frontend.cursor, Some(item.clone()));
        host_focus(&mut frontend, PaneId::Terminal(1));
        assert_eq!(frontend.cursor, Some(item.clone()));

        let request = frontend.queue.pop_front().unwrap();
        frontend.in_flight = Some(request);
        let state = snapshot(2, "terminal_2");
        frontend.accept_snapshot(state.clone(), true);
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: 1,
                state: Some(state),
                effects: Vec::new(),
                error: None,
            },
        });
        assert!(!frontend.local_selection);
        assert_eq!(frontend.cursor, Some(item));
        frontend.accept_snapshot(snapshot(3, "terminal_1"), true);
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
    }

    fn click_item(frontend: &mut Frontend, item: Item) {
        let index = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap();
        frontend.mouse(Mouse::LeftClick((index + 2 - frontend.scroll) as isize, 5));
    }

    fn acknowledge(frontend: &mut Frontend, state: Snapshot, error: Option<&str>) {
        let request_id = frontend.in_flight.as_ref().unwrap().request_id;
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id,
                state: Some(state),
                effects: Vec::new(),
                error: error.map(str::to_owned),
            },
        });
    }

    #[test]
    fn latest_click_owns_selection_through_older_ack_and_host_focus() {
        let mut frontend = frontend();
        let first = Item::Pane("terminal_2".to_owned());
        let latest = Item::Pane("terminal_1".to_owned());
        click_item(&mut frontend, first);
        frontend.in_flight = frontend.queue.pop_front();
        click_item(&mut frontend, latest.clone());
        assert_eq!(frontend.cursor, Some(latest.clone()));
        acknowledge(&mut frontend, snapshot(2, "terminal_2"), None);
        host_focus(&mut frontend, PaneId::Terminal(2));
        assert_eq!(frontend.cursor, Some(latest.clone()));
        assert_eq!(frontend.desired_selection.as_ref().unwrap().request_id, 2);
        assert!(!frontend.desired_selection.as_ref().unwrap().acknowledged);
        frontend.in_flight = frontend.queue.pop_front();
        acknowledge(&mut frontend, snapshot(2, "terminal_2"), None);
        assert_eq!(frontend.cursor, Some(latest.clone()));
        assert!(frontend.desired_selection.as_ref().unwrap().acknowledged);
        assert!(frontend.pending_user_action.is_some());
        frontend.accept_snapshot(snapshot(3, "terminal_1"), true);
        assert_eq!(frontend.cursor, Some(latest));
        assert!(frontend.desired_selection.is_none());
        assert!(frontend.pending_user_action.is_none());
    }

    #[test]
    fn repeated_target_clicks_still_require_the_latest_request_identity() {
        let mut frontend = frontend();
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        frontend.in_flight = frontend.queue.pop_front();
        click_item(&mut frontend, Item::Pane("terminal_1".to_owned()));
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        acknowledge(&mut frontend, snapshot(2, "terminal_2"), None);
        assert_eq!(frontend.desired_selection.as_ref().unwrap().request_id, 3);
        assert!(!frontend.desired_selection.as_ref().unwrap().acknowledged);
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
    }

    #[test]
    fn latest_selection_failure_or_target_removal_repairs_to_confirmed_focus() {
        let mut failed = frontend();
        click_item(&mut failed, Item::Pane("terminal_2".to_owned()));
        failed.in_flight = failed.queue.pop_front();
        acknowledge(
            &mut failed,
            snapshot(2, "terminal_1"),
            Some("Selection failed"),
        );
        assert_eq!(failed.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert!(failed.desired_selection.is_none());
        assert!(failed.pending_user_action.is_none());

        let mut removed = frontend();
        click_item(&mut removed, Item::Pane("terminal_2".to_owned()));
        removed.in_flight = removed.queue.pop_front();
        let mut state = snapshot(2, "terminal_1");
        state.panes.retain(|pane| pane.id != "terminal_2");
        removed.accept_snapshot(state.clone(), true);
        assert!(removed.desired_selection.is_none());
        assert_eq!(removed.cursor, Some(Item::Pane("terminal_1".to_owned())));
        acknowledge(&mut removed, state, Some("Terminal is no longer open"));
        assert!(removed.pending_user_action.is_none());
    }

    fn relay(revision: u64, request_id: u64, client: &str, terminal: &str) -> native::UiSelection {
        native::UiSelection {
            revision,
            request_id,
            client: client.to_owned(),
            terminal_id: terminal.to_owned(),
        }
    }

    #[test]
    fn destination_replica_holds_relay_until_confirmed_and_rejects_old_tombstones() {
        let mut destination = frontend();
        assert!(destination.accept_selection_relay(
            "owner".to_owned(),
            2,
            Some(relay(2, 7, "source", "terminal_2")),
        ));
        assert_eq!(
            destination.cursor,
            Some(Item::Pane("terminal_2".to_owned()))
        );
        destination.accept_snapshot(snapshot(2, "terminal_1"), true);
        assert_eq!(
            destination.cursor,
            Some(Item::Pane("terminal_2".to_owned()))
        );
        assert!(!destination.accept_selection_relay("owner".to_owned(), 1, None));
        assert_eq!(
            destination.cursor,
            Some(Item::Pane("terminal_2".to_owned()))
        );
        destination.accept_snapshot(snapshot(3, "terminal_2"), true);
        assert!(destination.relayed_selection.is_none());
        assert!(destination.pending_user_action.is_none());
    }

    #[test]
    fn local_latest_selection_wins_over_older_relay_and_epoch_tombstone() {
        let mut frontend = frontend();
        frontend.native_client = "source".to_owned();
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        frontend.in_flight = frontend.queue.pop_front();
        click_item(&mut frontend, Item::Pane("terminal_1".to_owned()));
        frontend.accept_selection_relay(
            "owner".to_owned(),
            9,
            Some(relay(9, 1, "source", "terminal_2")),
        );
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert!(frontend.relayed_selection.is_none());
        frontend.accept_selection_relay("restarted-owner".to_owned(), 0, None);
        assert_eq!(frontend.desired_selection.as_ref().unwrap().request_id, 2);
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
    }

    #[test]
    fn owner_epoch_reset_clears_inherited_intent_without_changing_canonical_state() {
        let mut frontend = frontend();
        let canonical = frontend.snapshot.clone();
        frontend.accept_selection_relay(
            "old-owner".to_owned(),
            90,
            Some(relay(90, 4, "source", "terminal_2")),
        );
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
        assert!(frontend.accept_selection_relay("new-owner".to_owned(), 0, None));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert_eq!(frontend.snapshot, canonical);
        assert!(frontend.relayed_selection.is_none());
    }

    #[test]
    fn synchronization_is_delayed_user_only_and_old_timers_do_not_start_new_episodes() {
        let mut frontend = frontend();
        let start = Instant::now();
        frontend.enqueue(Action::Snapshot);
        assert!(frontend.pending_user_action.is_none());
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        frontend.pending_user_action.as_mut().unwrap().started = start;
        assert!(!frontend.synchronizing(start));
        assert!(!frontend.synchronizing(start + Duration::from_millis(1999)));
        assert!(frontend.synchronizing(start + Duration::from_secs(2)));
        assert!(frontend.expire_user_action(start + Duration::from_secs(2)));
        assert!(!frontend.expire_user_action(start + Duration::from_secs(3)));
        frontend.reset_pending_actions();
        frontend.reconcile_cursor();
        assert!(!frontend.expire_user_action(start + Duration::from_secs(3)));
        frontend.enqueue(Action::RenamePane {
            pane_id: "terminal_1".to_owned(),
            label: "renamed".to_owned(),
        });
        frontend.pending_user_action.as_mut().unwrap().started = start + Duration::from_secs(3);
        assert!(!frontend.expire_user_action(start + Duration::from_secs(4)));
        assert!(frontend.expire_user_action(start + Duration::from_secs(5)));
        frontend.queue.clear();
        frontend.finish_user_action();
        assert!(frontend.pending_user_action.is_none());
    }

    #[test]
    fn foreign_client_owner_publication_does_not_reset_inherited_selection() {
        let mut frontend = frontend();
        frontend.accept_selection_relay(
            "owner".to_owned(),
            3,
            Some(relay(3, 7, "source", "terminal_2")),
        );
        assert!(!frontend.accept_selection_relay(String::new(), 0, None));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
        assert_eq!(frontend.relay_epoch, "owner");
        assert_eq!(frontend.relay_revision, 3);
    }

    #[test]
    fn same_canonical_publication_redraws_new_relay_and_failure_tombstone_only_once() {
        let mut frontend = frontend();
        frontend.granted = true;
        let state = snapshot(1, "terminal_1");
        let publication = |revision, selection| native::NativeState {
            client_id: 0,
            snapshot: state.clone(),
            ui_selection: selection,
            ui_selection_revision: revision,
            ui_selection_epoch: "owner".to_owned(),
        };
        assert!(frontend.pipe(native_message(
            native::STATE,
            publication(1, Some(relay(1, 7, "source", "terminal_2"))),
        )));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
        assert!(!frontend.pipe(native_message(
            native::STATE,
            publication(1, Some(relay(1, 7, "source", "terminal_2"))),
        )));
        assert!(frontend.pipe(native_message(native::STATE, publication(2, None))));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert!(!frontend.pipe(native_message(
            native::STATE,
            publication(1, Some(relay(1, 7, "source", "terminal_2"))),
        )));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert!(!frontend.pipe(native_message(native::STATE, publication(2, None))));
    }

    #[test]
    fn modal_selection_is_pinned_and_folded_focus_highlights_its_workspace() {
        let mut frontend = frontend();
        frontend.modal = Some(Modal::Rename {
            item: Item::Pane("terminal_1".to_owned()),
            input: ui::Input::new("Terminal 1".to_owned()),
        });
        frontend.accept_snapshot(snapshot(2, "terminal_2"), true);
        host_focus(&mut frontend, PaneId::Terminal(2));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        frontend.modal = None;
        frontend.collapsed.insert("workspace_2".to_owned());
        frontend.reconcile_cursor();
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_2".to_owned()))
        );
    }

    #[test]
    fn harness_rows_fit_narrow_widths() {
        let mut state = snapshot(1, "terminal_1");
        state.panes[0].name = "Project".to_owned();
        let row = ui::TreeRow {
            item: Some(Item::Pane("terminal_1".to_owned())),
            workspace_id: Some("workspace_1".to_owned()),
            kind: ui::RowKind::Pane {
                index: 0,
                last: true,
            },
        };
        for harness in [
            "claude",
            "claude-code",
            "omp",
            "codex",
            "opencode",
            "agy",
            "antigravity",
            "unknown-app",
        ] {
            state.panes[0].harness = Some(harness.to_owned());
            for columns in 0..=24 {
                let rendered = ui::render_row(
                    &row,
                    &state,
                    &ui::Theme::default(),
                    columns,
                    true,
                    false,
                    true,
                );
                let visible: String = rendered
                    .split('\u{1b}')
                    .map(|part| {
                        part.strip_prefix('[')
                            .and_then(|sgr| sgr.split_once('m').map(|(_, text)| text))
                            .unwrap_or(part)
                    })
                    .collect();
                assert_eq!(ui::width(&visible), columns, "{harness}: {visible:?}");
            }
        }
    }

    #[test]
    fn branch_rows_fit_unicode_cells_and_leave_activity_in_tooltips() {
        let mut state = snapshot(1, "terminal_1");
        state.panes[0].harness = Some("claude".to_owned());
        state.panes[0].status = Status::Blocked;
        state.panes[0].attention = Some(shephrd::Attention::Error);
        state.panes[0].description = "Connection refused while reading source".to_owned();
        for branch in [
            "main",
            "detached @ a1b2c3d",
            "feature/日本語-π-e\u{301}",
            "feature/\u{1b}\n\u{202e}safe",
        ] {
            state.panes[0].branch = Some(branch.to_owned());
            for last in [false, true] {
                let row = ui::TreeRow {
                    item: Some(Item::Pane("terminal_1".to_owned())),
                    workspace_id: Some("workspace_1".to_owned()),
                    kind: ui::RowKind::Detail { index: 0, last },
                };
                for columns in 0..=48 {
                    for (selected, hovered) in [(false, false), (true, false), (false, true)] {
                        let rendered = ui::render_row(
                            &row,
                            &state,
                            &ui::Theme::default(),
                            columns,
                            selected,
                            hovered,
                            selected,
                        );
                        let visible: String = rendered
                            .split('\u{1b}')
                            .map(|part| {
                                part.strip_prefix('[')
                                    .and_then(|sgr| sgr.split_once('m').map(|(_, text)| text))
                                    .unwrap_or(part)
                            })
                            .collect();
                        assert_eq!(ui::width(&visible), columns, "{branch}: {visible:?}");
                        assert!(!visible.contains("Connection refused"));
                        assert!(!visible.contains('\u{202e}'));
                        if columns == 48 {
                            assert!(visible.contains(&format!("\u{f062c} {}", ui::clean(branch))));
                            assert!(rendered.contains(ui::MUTED));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn live_branch_row_changes_preserve_keyboard_selection_and_hover_identity() {
        let mut frontend = frontend();
        frontend.local_selection = true;
        frontend.cursor = Some(Item::Pane("terminal_2".to_owned()));
        frontend.hover = Some(Item::Pane("terminal_1".to_owned()));
        for (revision, branch) in [
            (2, Some("main")),
            (3, Some("feature/switched")),
            (4, Some("detached @ a1b2c3d")),
            (5, None),
        ] {
            let mut state = snapshot(revision, "terminal_1");
            state.panes[0].harness = Some("omp".to_owned());
            state.panes[0].branch = branch.map(str::to_owned);
            assert!(frontend.accept_snapshot(state, true));
            assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
            assert_eq!(frontend.hover, Some(Item::Pane("terminal_1".to_owned())));
            assert_eq!(
                frontend
                    .tree_cache
                    .iter()
                    .filter(|row| matches!(row.kind, ui::RowKind::Detail { index: 0, .. }))
                    .count(),
                1,
            );
        }
    }
}
