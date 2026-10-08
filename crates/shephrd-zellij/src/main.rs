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

#[derive(Default)]
struct Frontend {
    configuration: BTreeMap<String, String>,
    id: u32,
    overlay_role: bool,
    overlay: overlay::Overlay,
    tooltip: overlay::Controller,
    theme: ui::Theme,
    granted: bool,
    snapshot: Option<Snapshot>,
    state_buffers: BTreeMap<String, String>,
    error: Option<String>,
    queue: VecDeque<Action>,
    in_flight: Option<Request>,
    close_targets: Vec<u32>,
    request_id: u64,
    panes: BTreeMap<u32, String>,
    manifest: PaneManifest,
    tabs: Vec<TabInfo>,
    native_focus: Option<u32>,
    cursor: Option<Item>,
    collapsed: BTreeSet<String>,
    tree_cache: Vec<ui::TreeRow>,
    items: Vec<Item>,
    hover: Option<Item>,
    hover_row: usize,
    click: Option<(String, Item, Instant)>,
    modal: Option<Modal>,
    scroll: usize,
    rows: usize,
    columns: usize,
    render_cursor: Option<(usize, usize)>,
    visible: bool,
}

register_plugin!(Frontend);

impl Frontend {
    fn enqueue(&mut self, action: Action) {
        // Register/focus notifications may arrive both in the manifest and open callback.
        if matches!(
            &action,
            Action::RegisterPane { .. } | Action::FocusPane { .. } | Action::Snapshot
        ) && (self.queue.contains(&action)
            || self
                .in_flight
                .as_ref()
                .is_some_and(|request| request.action == action))
        {
            return;
        }
        self.queue.push_back(action);
        self.dispatch();
    }

    fn dispatch(&mut self) {
        if !self.granted || self.in_flight.is_some() {
            return;
        }
        let Some(action) = self.queue.pop_front() else {
            return;
        };
        self.close_targets = self
            .panes
            .iter()
            .filter_map(|(native, public)| {
                let closes = match &action {
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
        self.request_id = self.request_id.saturating_add(1);
        let request = Request::new(self.request_id, action);
        let json = match serde_json::to_string(&request) {
            Ok(json) => json,
            Err(error) => {
                self.error = Some(format!("Cannot encode request: {error}"));
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
                self.snapshot = Some(state);
                self.reconcile_cursor();
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
        self.collapsed
            .retain(|id| state.workspaces.iter().any(|w| &w.id == id));
        self.tree_cache = ui::tree(state, &self.collapsed);
        self.items = ui::items(&self.tree_cache);
        if !self
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
        self.dispatch();
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::FocusPane { zellij_pane_id } => {
                let exists = get_pane_info(PaneId::Terminal(zellij_pane_id)).is_some();
                if exists {
                    self.hide_tooltip();
                    switch_to_input_mode(&InputMode::Locked);
                    focus_terminal_pane(zellij_pane_id, false, false);
                } else {
                    self.error = Some(format!(
                        "Native pane {zellij_pane_id} is no longer open; select again"
                    ));
                    self.panes.remove(&zellij_pane_id);
                    self.enqueue(Action::PaneClosed { zellij_pane_id });
                }
            }
            Effect::OpenPane { pane_id, command } => {
                if let Some((&native_id, _)) =
                    self.panes.iter().find(|(_, public)| *public == &pane_id)
                {
                    if get_pane_info(PaneId::Terminal(native_id)).is_some() {
                        self.hide_tooltip();
                        switch_to_input_mode(&InputMode::Locked);
                        focus_terminal_pane(native_id, false, false);
                        return;
                    }
                    self.panes.remove(&native_id);
                    self.enqueue(Action::PaneClosed {
                        zellij_pane_id: native_id,
                    });
                }
                let context = BTreeMap::from([("shephrd_pane".to_owned(), pane_id.clone())]);
                let command = CommandToRun {
                    path: command.executable.into(),
                    args: command.args,
                    cwd: command.cwd.map(Into::into),
                };
                self.hide_tooltip();
                switch_to_input_mode(&InputMode::Locked);
                match open_command_pane_near_plugin(command, context) {
                    Some(PaneId::Terminal(id)) => {
                        rename_terminal_pane(id, format!("shephrd:{pane_id}"));
                        focus_terminal_pane(id, false, false);
                        // The CommandPaneOpened callback is authoritative for registration.
                    }
                    _ => self.error = Some(format!("Zellij could not open attachment {pane_id}")),
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

    fn pane_update(&mut self, manifest: PaneManifest) {
        let initial: Vec<_> = manifest
            .panes
            .values()
            .flatten()
            .filter(|pane| !pane.is_plugin)
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
        let focused = focused.filter(|pane| !pane.is_plugin).map(|pane| pane.id);
        if focused == self.native_focus {
            return;
        }
        self.native_focus = focused;
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

    fn hide_tooltip(&mut self) {
        self.hover = None;
        self.tooltip.update(self.id, &self.configuration, None);
    }

    fn refresh_tooltip(&mut self) {
        if !self.granted || self.overlay_role {
            return;
        }
        let tooltip = if self.visible && self.modal.is_none() {
            self.hover
                .as_ref()
                .zip(self.snapshot.as_ref())
                .and_then(|(item, snapshot)| {
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
                    let lines = ui::tooltip(item, snapshot);
                    if lines.is_empty() {
                        return None;
                    }
                    let coordinates = overlay::geometry(sidebar, tab, self.hover_row, &lines)?;
                    Some((lines, coordinates))
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
        self.hide_tooltip();
        self.reconcile_cursor();
        self.ensure_cursor_visible();
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
        self.click = None;
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

    fn mouse(&mut self, mouse: Mouse) {
        if self.modal.is_some() {
            self.hide_tooltip();
            return;
        }
        match mouse {
            Mouse::ScrollUp(lines) => {
                self.scroll = self.scroll.saturating_sub(lines);
                self.hide_tooltip();
            }
            Mouse::ScrollDown(lines) => {
                self.scroll = self.scroll.saturating_add(lines).min(
                    self.tree_cache
                        .len()
                        .saturating_sub(self.rows.saturating_sub(4)),
                );
                self.hide_tooltip();
            }
            Mouse::Hover(line, column)
            | Mouse::LeftClick(line, column)
            | Mouse::RightClick(line, column) => {
                let tree = &self.tree_cache;
                let row = if line >= 2
                    && (line as usize) < self.rows.saturating_sub(2)
                    && column < self.columns
                {
                    tree.get(self.scroll + line as usize - 2)
                } else {
                    None
                };
                let item = row.and_then(|row| row.item.clone());
                if matches!(mouse, Mouse::Hover(_, _)) {
                    self.hover = item;
                    self.hover_row = line.max(0) as usize;
                    self.refresh_tooltip();
                } else if let Some(item) = item {
                    let group = row.and_then(|row| row.workspace_id.clone());
                    self.cursor = Some(item.clone());
                    self.hide_tooltip();
                    if matches!(mouse, Mouse::RightClick(_, _)) {
                        self.click = None;
                        self.edit_selected(false);
                        return;
                    }
                    let now = Instant::now();
                    let double = group.as_ref().is_some_and(|group| {
                        self.click.as_ref().is_some_and(|(old, _, at)| {
                            old == group && now.duration_since(*at) < Duration::from_millis(350)
                        })
                    });
                    if double {
                        self.click = None;
                        if let Some(group) = group {
                            self.fold(group, None);
                        }
                    } else {
                        self.click = group.map(|group| (group, item, now));
                        set_timeout(0.36);
                    }
                } else {
                    self.hide_tooltip();
                    self.click = None;
                }
            }
            _ => {}
        }
    }

    fn render_sidebar(&mut self, rows: usize, columns: usize) {
        self.rows = rows;
        self.columns = columns;
        let tree = &self.tree_cache;
        let body_height = rows.saturating_sub(4);
        self.scroll = self.scroll.min(tree.len().saturating_sub(body_height));
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
            } else if self.in_flight.is_some() {
                "Synchronizing…".to_owned()
            } else {
                String::new()
            }
        } else {
            "Connecting to companion…".to_owned()
        };
        let modal_render = self.modal.as_ref().map(|modal| modal.lines(columns));
        // Host commands share stdout with rendering; emit them before any terminal bytes.
        let cursor = modal_render
            .as_ref()
            .and_then(|(_, cursor)| *cursor)
            .filter(|(x, y)| *x < columns && y + 2 < rows.saturating_sub(2))
            .map(|(x, y)| (x, y + 2));
        if self.render_cursor != cursor {
            self.render_cursor = cursor;
            show_cursor(cursor);
        }
        for row in 0..rows {
            let line = if row == 0 {
                format!("\u{1b}[1;39m{}{}", ui::fit(&heading, columns), ui::RESET)
            } else if row == 1 {
                let color = if !status.is_empty()
                    && (self.error.is_some()
                        || self.snapshot.as_ref().is_some_and(|state| !state.connected))
                {
                    "\u{1b}[38;5;214m"
                } else {
                    ui::MUTED
                };
                format!("{color}{}{}", ui::fit(&status, columns), ui::RESET)
            } else if row == rows.saturating_sub(2) {
                format!(
                    "{}{}{}",
                    ui::MUTED,
                    ui::fit("↑↓ select · Enter attach · Space fold", columns),
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
                    "\u{1b}[39m{}{}",
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
            print!("\u{1b}[{};1H{}{line}{}", row + 1, ui::RESET, ui::RESET);
        }
    }
}

impl ZellijPlugin for Frontend {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.id = get_plugin_ids().plugin_id;
        self.visible = true;
        self.overlay_role = configuration
            .get("role")
            .is_some_and(|role| role == "overlay");
        self.configuration = configuration;
        if self.overlay_role {
            self.overlay.load(&self.configuration, self.id);
            return;
        }
        set_selectable(true);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Key,
            EventType::Mouse,
            EventType::ModeUpdate,
            EventType::Timer,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::RunCommandResult,
            EventType::CommandPaneOpened,
            EventType::CommandPaneExited,
            EventType::CommandPaneReRun,
            EventType::PaneClosed,
            EventType::Visible,
            EventType::BeforeClose,
            EventType::PastedText,
        ]);
        if self.configuration.get("role").map(String::as_str) != Some("sidebar")
            || self.configuration.len() != 4
            || !self
                .configuration
                .get("socket")
                .is_some_and(|path| Path::new(path).is_absolute())
            || !self
                .configuration
                .get("program")
                .is_some_and(|path| Path::new(path).is_absolute())
            || !self
                .configuration
                .get("plugin_url")
                .is_some_and(|url| url.starts_with("file:"))
        {
            self.error = Some(
                "Invalid sidebar configuration: role/socket/program/plugin_url required".to_owned(),
            );
            return;
        }
        let version = get_zellij_version();
        if version != "0.45.1" {
            self.error = Some(format!("Unsupported Zellij {version}; need 0.45.1"));
            return;
        }
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        if self.overlay_role {
            return self.overlay.event(event);
        }
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                switch_to_input_mode(&InputMode::Locked);
                self.startup();
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Required permissions denied; reload to authorize".to_owned())
            }
            Event::ModeUpdate(mode) => self.theme.update(mode.style),
            Event::Timer(_) => {
                if self
                    .click
                    .as_ref()
                    .is_some_and(|(_, _, at)| at.elapsed() >= Duration::from_millis(350))
                    && let Some((_, item, _)) = self.click.take()
                    && self.modal.is_none()
                    && self.cursor.as_ref() == Some(&item)
                {
                    self.activate();
                }
            }
            Event::RunCommandResult(code, stdout, stderr, context) => {
                self.response(code, stdout, stderr, context)
            }
            Event::PaneUpdate(manifest) if self.granted => self.pane_update(manifest),
            Event::TabUpdate(tabs) if self.granted => {
                self.tabs = tabs;
                self.synchronize_focus();
                self.refresh_tooltip();
            }
            Event::CommandPaneOpened(id, context) | Event::CommandPaneReRun(id, context)
                if self.granted =>
            {
                if let Some(public) = context.get("shephrd_pane") {
                    self.register_pane(id, public.clone());
                    self.synchronize_focus();
                }
            }
            Event::CommandPaneExited(id, code, _) if self.panes.contains_key(&id) => {
                // Held/exited command panes still exist and may be rerun. Never forget or kill the PTY here.
                if code != Some(0) {
                    self.error = Some(format!(
                        "Attachment pane {id} exited {code:?}; rerun it in Zellij or select another pane"
                    ));
                }
            }
            Event::PaneClosed(PaneId::Terminal(id)) if self.granted => {
                if self.panes.remove(&id).is_some() {
                    self.enqueue(Action::PaneClosed { zellij_pane_id: id });
                }
                if self.native_focus == Some(id) {
                    self.native_focus = None;
                }
            }
            Event::PaneClosed(PaneId::Plugin(id)) => self.tooltip.forget(id),
            Event::Key(key) if self.granted => self.key(key),
            Event::Mouse(mouse) if self.granted => self.mouse(mouse),
            Event::PastedText(text) if self.granted => {
                if let Some(input) = self.modal.as_mut().and_then(Modal::input_mut) {
                    input.insert(&text);
                }
            }
            Event::Visible(visible) => {
                self.visible = visible;
                if !visible {
                    self.hide_tooltip();
                }
            }
            Event::BeforeClose if self.granted => self.tooltip.close(self.id, &self.configuration),
            _ => {}
        }
        true
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if self.overlay_role {
            return self.overlay.pipe(message);
        }
        if message.name == overlay::READY {
            if let (PipeSource::Plugin(source), Some(payload)) = (message.source, message.payload)
                && payload.parse::<u32>().ok() == Some(source)
            {
                self.tooltip.ready(source, self.id, &self.configuration);
                return true;
            }
            return false;
        }
        if message.name != "shephrd-state" || !self.granted {
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
        } else {
            self.render_sidebar(rows, columns);
        }
        std::io::stdout().flush().expect("flushing plugin render");
    }
}
