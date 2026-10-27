mod animation;
mod finder;
mod finder_entry;
mod focused;
mod graphics;
mod keyguide;
mod native;
mod overlay;
mod palette;
mod pane_search;
mod ui;

use bkpr::{Action, Effect, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Snapshot};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
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

const CONTROL_PIPE: &str = "bkpr-controls";

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

struct QueuedRequest {
    request: Request,
    return_mode: Option<InputMode>,
    view: Option<native::SidebarView>,
}

struct CachedPopup {
    item: Item,
    instance: String,
    revision: u64,
    formatted_at: u64,
    content: ui::PopupContent,
}

struct CachedKeyGuide {
    context: keyguide::Context,
    columns: usize,
    max_rows: usize,
    lines: Vec<String>,
    space: Option<GuideSpace>,
    overlay_ready: bool,
    expanded_x: usize,
    expanded_columns: usize,
    expanded_rows: usize,
    reserved_rows: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GuideSpace {
    x: usize,
    bottom: usize,
    max_columns: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
struct ModeState {
    mode: InputMode,
    base_mode: Option<InputMode>,
}

#[derive(serde::Deserialize)]
struct NativeStateIdentity {
    client_id: u16,
}

#[derive(serde::Deserialize)]
struct NativeResponseIdentity<'a> {
    #[serde(borrow)]
    client: &'a str,
    client_id: u16,
    response: NativeRequestIdentity,
}

#[derive(serde::Deserialize)]
struct NativeRequestIdentity {
    request_id: u64,
}

#[derive(Default)]
struct Frontend {
    configuration: BTreeMap<String, String>,
    id: u32,
    client_id: u16,
    overlay_role: bool,
    guide_role: bool,
    native_owner_role: bool,
    focused_role: bool,
    native_owner: native::Owner,
    native_owner_id: Option<u32>,
    native_client: String,
    control_receipts: VecDeque<(String, String)>,
    control_ready: Vec<(String, String, Instant)>,
    // Explicit keyboard control owns selection; pointer interaction does not own focus.
    local_selection: bool,
    keyboard_control: bool,
    control_has_focus: bool,
    control_terminal: Option<u32>,
    overlay: overlay::Overlay,
    tooltip: overlay::Controller,
    guide_view: overlay::GuideOverlay,
    guide_popup: overlay::GuideController,
    focused_view: focused::Host,
    focused: focused::Controller,
    theme: ui::Theme,
    mode: Option<ModeState>,
    mode_intent: Option<InputMode>,
    keybinds: Option<KeybindsVec>,
    keybinds_payload: String,
    keyguide: Option<CachedKeyGuide>,
    keyguide_dirty: bool,
    granted: bool,
    sidebar_bootstrap: Option<native::SidebarBootstrap>,
    snapshot_is_bootstrap: bool,
    bootstrap_view_pending: bool,
    snapshot: Option<Snapshot>,
    response_state_valid: bool,
    error: Option<String>,
    queue: VecDeque<QueuedRequest>,
    in_flight: Option<Request>,
    in_flight_return_mode: Option<InputMode>,
    in_flight_view: Option<native::SidebarView>,
    request_id: u64,
    manifest: PaneManifest,
    tabs: Vec<TabInfo>,
    native_focus: Option<u32>,
    cursor: Option<Item>,
    desired_selection: Option<DesiredSelection>,
    pending_user_action: Option<PendingUserAction>,
    working_clock: animation::WorkingClock,
    relayed_selection: Option<native::UiSelection>,
    relay_revision: u64,
    relay_epoch: String,
    collapsed: BTreeSet<String>,
    tree_cache: Vec<ui::TreeRow>,
    items: Vec<Item>,
    hover: Option<Item>,
    pointer_position: Option<(isize, usize)>,
    pointer_item: Option<Item>,
    pointer_highlight: bool,
    popup_hover: bool,
    popup_probe: bool,
    popup_probe_revision: Option<u64>,
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
    fn focused_dimensions(&self) -> Option<(usize, usize)> {
        let tab = self.tabs.iter().find(|tab| tab.active)?;
        (tab.display_area_columns > 0 && tab.display_area_rows > 0)
            .then_some((tab.display_area_columns, tab.display_area_rows))
    }

    fn open_focused(&mut self, kind: focused::Kind) {
        if !self.granted || !self.visible || self.modal.is_some() || self.focused.is_open() {
            return;
        }
        let Some((columns, rows)) = self.focused_dimensions() else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        self.focused.context = Some(focused::ReturnContext {
            sidebar: self.keyboard_control,
            terminal: self.control_terminal.or(self.native_focus),
            cursor: self.cursor.clone(),
            mode: self
                .mode
                .as_ref()
                .map_or(InputMode::Normal, |mode| mode.mode),
            ready: false,
            locked: false,
        });
        self.focused
            .open(self.id, &self.configuration, snapshot, kind, columns, rows);
        self.hide_tooltip();
        // Keep ownership of input until the guest acknowledges all warmup keystrokes.
        self.request_mode(InputMode::Locked);
        self.reconcile_viewport(false);
    }

    fn refresh_focused(&mut self) {
        if self.focused.is_open()
            && let Some((columns, rows)) = self.focused_dimensions()
            && let Some(snapshot) = self.snapshot.as_ref()
        {
            self.focused
                .update(self.id, &self.configuration, snapshot, columns, rows);
        }
    }

    fn cancel_focused(&mut self, restore: bool) {
        // A dispatched form submission cannot be unsent. Keep its result correlated
        // rather than claiming Esc aborted it; queued submissions are cancellable.
        if restore
            && self.focused.submission.is_some_and(|id| {
                self.in_flight
                    .as_ref()
                    .is_some_and(|request| request.request_id == id)
            })
        {
            return;
        }
        self.close_focused(restore);
    }

    fn close_focused(&mut self, restore: bool) {
        if let Some(request_id) = self.focused.submission.take() {
            self.queue
                .retain(|queued| queued.request.request_id != request_id);
            self.finish_user_action();
        }
        self.focused.close(self.id, &self.configuration);
        let Some(context) = self.focused.context.take() else {
            return;
        };
        if restore && self.visible {
            let terminal = context.terminal.filter(|id| {
                self.manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| !pane.is_plugin && pane.id == *id)
            });
            if context.sidebar {
                self.enter_control();
                self.control_terminal = terminal;
                self.cursor = context.cursor;
                self.reconcile_cursor();
                self.ensure_cursor_visible();
            } else {
                self.leave_control(false);
                if let Some(id) = terminal {
                    focus_terminal_pane(id, false, false);
                }
            }
            self.request_mode(context.mode);
        }
        self.reconcile_viewport(false);
        self.refresh_tooltip();
    }

    fn focused_pipe(&mut self, message: &PipeMessage) -> bool {
        let Some(notice) = self.focused.pipe(message, self.id, &self.configuration) else {
            return false;
        };
        match notice {
            focused::Notice::Ready => {
                let Some(context) = self.focused.context.as_mut() else {
                    self.focused.close(self.id, &self.configuration);
                    return false;
                };
                context.ready = true;
                self.leave_control(false);
                self.request_mode(InputMode::Locked);
                if let Some(id) = self.focused.id {
                    focus_plugin_pane(id, true, false);
                }
            }
            focused::Notice::Cancelled { restore } => self.cancel_focused(restore),
            focused::Notice::Submitted(action) => {
                self.focused.submission = Some(self.request_id.saturating_add(1));
                self.enqueue(action);
            }
            focused::Notice::Selected { pane_id } => {
                self.focused.context = None;
                self.leave_control(false);
                self.request_mode(InputMode::Locked);
                self.cursor = Some(Item::Pane(pane_id));
                self.activate();
            }
        }
        self.reconcile_viewport(false);
        true
    }

    fn finish_focused_submission(&mut self, request_id: u64, error: Option<String>) {
        if self.focused.submission != Some(request_id) {
            return;
        }
        self.focused.submission = None;
        if let Some(error) = error {
            self.focused
                .submission_failed(error, self.id, &self.configuration);
        } else if matches!(self.focused.kind(), Some(focused::Kind::Rename { .. })) {
            self.close_focused(true);
        } else {
            self.focused.close(self.id, &self.configuration);
            self.focused.context = None;
            self.request_mode(InputMode::Locked);
        }
    }

    fn keyguide_context(&self) -> keyguide::Context {
        if let Some(kind) = self.focused.kind() {
            return match kind {
                focused::Kind::Finder => keyguide::Context::Finder,
                focused::Kind::Workspace => keyguide::Context::Create {
                    multiple_fields: true,
                },
                focused::Kind::Pane { .. } => keyguide::Context::Create {
                    multiple_fields: false,
                },
                focused::Kind::Rename { .. } => keyguide::Context::Edit {
                    multiple_fields: false,
                },
            };
        }
        if !self.keyboard_control {
            return keyguide::Context::Application;
        }
        match &self.modal {
            Some(Modal::Close { .. }) => keyguide::Context::Confirm,
            None => keyguide::Context::Sidebar {
                selected: self.cursor.is_some(),
            },
        }
    }

    fn terminal_colors_message(&mut self, payload: &str) -> bool {
        let Ok(colors) = serde_json::from_str::<palette::TerminalColors>(payload) else {
            return false;
        };
        if self.overlay_role {
            return self.overlay.set_terminal_colors(&colors);
        }
        if self.guide_role {
            return self.guide_view.set_terminal_colors(&colors);
        }
        if self.focused_role {
            return self.focused_view.set_terminal_colors(&colors);
        }
        if self.native_owner_role || !self.theme.update(&colors) {
            return false;
        }
        self.keyguide_dirty = true;
        self.reconcile_viewport(false);
        self.refresh_tooltip();
        true
    }

    fn mode_message(&mut self, payload: &str) -> bool {
        let Ok(mode) = serde_json::from_str::<ModeState>(payload) else {
            return false;
        };
        let previous = self.guide_mode();
        let changed = self.mode != Some(mode);
        self.mode = Some(mode);
        if self.mode_intent == Some(mode.mode) {
            self.mode_intent = None;
        }
        self.mode_changed(mode.mode, changed || previous != self.guide_mode())
    }

    fn keybinds_message(&mut self, payload: String) -> bool {
        if self.keybinds.is_some() && self.keybinds_payload == payload {
            return false;
        }
        let Ok(keybinds) = serde_json::from_str::<KeybindsVec>(&payload) else {
            return false;
        };
        self.keybinds_payload = payload;
        if self.keybinds.as_ref() == Some(&keybinds) {
            return false;
        }
        self.keybinds = Some(keybinds);
        self.keyguide_dirty = true;
        self.reconcile_viewport(false);
        self.refresh_tooltip();
        true
    }

    fn guide_mode(&self) -> Option<InputMode> {
        self.mode_intent.or(self.mode.map(|mode| mode.mode))
    }

    fn extract_sidebar_bootstrap(&mut self, configuration: &mut BTreeMap<String, String>) {
        self.sidebar_bootstrap = configuration
            .remove("sidebar_bootstrap")
            .filter(|payload| payload.len() <= MAX_MESSAGE_BYTES)
            .and_then(|payload| serde_json::from_str::<native::SidebarBootstrap>(&payload).ok())
            .filter(|bootstrap| matches!(self.validate_snapshot(&bootstrap.snapshot), Ok(true)));
    }

    fn apply_sidebar_bootstrap(&mut self) -> bool {
        if !self.granted || self.snapshot.is_some() {
            return false;
        }
        let Some(bootstrap) = self.sidebar_bootstrap.take() else {
            return false;
        };
        if self.keybinds.is_none() {
            let Some((keybinds, payload)) = bootstrap
                .keymap
                .load(&bootstrap.snapshot.instance_id, self.client_id)
            else {
                return false;
            };
            self.keybinds = Some(keybinds);
            self.keybinds_payload = payload;
        }
        // This is only a pre-creation projection, never a live owner acknowledgement.
        self.theme.prepare_snapshot(&bootstrap.snapshot);
        self.snapshot = Some(bootstrap.snapshot);
        self.snapshot_is_bootstrap = true;
        self.response_state_valid = false;
        self.collapsed = bootstrap.view.collapsed;
        self.scroll = bootstrap.view.scroll;
        self.bootstrap_view_pending = true;
        if self.mode.is_none() {
            self.mode = Some(ModeState {
                mode: bootstrap.mode,
                base_mode: bootstrap.base_mode,
            });
        }
        self.keyguide_dirty = true;
        self.rebuild_tree();
        self.reconcile_cursor();
        true
    }

    fn mode_changed(&mut self, mode: InputMode, changed: bool) -> bool {
        let dismiss_focused = self.focused.context.as_mut().is_some_and(|context| {
            if mode == InputMode::Locked {
                context.locked = true;
                false
            } else {
                context.locked
            }
        });
        let leave_control = self.granted
            && self.keyboard_control
            && mode == InputMode::Locked
            && self.mode_intent.is_none()
            && self.modal.is_none()
            && !self.focused.is_open();
        if dismiss_focused {
            self.close_focused(false);
        }
        self.keyguide_dirty |= changed;
        if leave_control {
            self.leave_control(true);
        }
        if changed || leave_control {
            self.reconcile_viewport(false);
            self.refresh_tooltip();
        }
        changed || leave_control
    }

    fn guide_space(&self) -> Option<GuideSpace> {
        if !self.granted
            || !self.visible
            || self.guide_mode()? == InputMode::Locked
            || self.focused.is_open()
        {
            return None;
        }
        let (position, sidebar) = self.manifest.panes.iter().find_map(|(position, panes)| {
            panes
                .iter()
                .find(|pane| pane.is_plugin && pane.id == self.id && !pane.is_suppressed)
                .map(|pane| (*position, pane))
        })?;
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.position == position && tab.active)?;
        let x = sidebar.pane_content_x.saturating_add(
            if matches!(self.keyguide_context(), keyguide::Context::Sidebar { .. }) {
                self.columns
            } else {
                0
            },
        );
        let bottom = sidebar
            .pane_content_y
            .saturating_add(self.rows)
            .min(tab.display_area_rows);
        let max_columns = tab.display_area_columns.saturating_sub(x);
        (max_columns > self.columns && bottom > 0).then_some(GuideSpace {
            x,
            bottom,
            max_columns,
        })
    }

    fn refresh_keyguide(&mut self) -> bool {
        let context = self.keyguide_context();
        let space = self.guide_space();
        let available = self.rows.saturating_sub(2);
        // Leave real tree content above the guide even on short terminals.
        let mut content_rows = available.div_ceil(2).min(4);
        if self.modal.is_some() {
            let modal_rows = match self.modal {
                Some(Modal::Close { .. }) => 4,
                _ => 3,
            };
            content_rows = content_rows.max(modal_rows.min(available.saturating_sub(1).max(1)));
        }
        let max_rows = available.saturating_sub(content_rows);
        if !self.keyguide_dirty
            && self.keyguide.as_ref().is_some_and(|cached| {
                cached.context == context
                    && cached.columns == self.columns
                    && cached.max_rows == max_rows
                    && cached.space == space
                    && cached.overlay_ready == self.guide_popup.ready_for_display()
            })
        {
            return false;
        }
        let old_height = self
            .keyguide
            .as_ref()
            .map_or(0, |cached| cached.reserved_rows);
        let mode = self.guide_mode().map(|input| keyguide::Mode {
            input,
            keybinds: self.keybinds.as_ref(),
        });
        let expanded = space.zip(mode).and_then(|(space, mode)| {
            let guide = keyguide::render_expanded(
                mode,
                context,
                self.columns,
                space.max_columns,
                max_rows.min(space.bottom),
                &self.theme,
            );
            (guide.columns > self.columns && !guide.lines.is_empty()).then_some((space, guide))
        });
        let expanded_columns = expanded.as_ref().map_or(0, |(_, guide)| guide.columns);
        let expanded_x = expanded.as_ref().map_or(0, |(space, _)| space.x);
        let expanded_rows = expanded.as_ref().map_or(0, |(_, guide)| guide.lines.len());
        let (lines, reserved_rows) = if let Some((space, guide)) = expanded {
            let height = guide.lines.len();
            let coordinates = FloatingPaneCoordinates {
                pinned: Some(true),
                borderless: Some(true),
                ..FloatingPaneCoordinates::default()
                    .with_x_fixed(expanded_x)
                    .with_y_fixed(space.bottom - height)
                    .with_width_fixed(guide.columns)
                    .with_height_fixed(height)
            };
            self.guide_popup.update(
                self.id,
                &self.configuration,
                Some((&guide.lines, coordinates)),
            );
            if matches!(context, keyguide::Context::Sidebar { .. }) {
                (Vec::new(), 0)
            } else if self.guide_popup.ready_for_display() {
                (Vec::new(), height)
            } else {
                (
                    keyguide::render(mode, context, self.columns, height, &self.theme),
                    height,
                )
            }
        } else {
            self.guide_popup.update(self.id, &self.configuration, None);
            let lines = keyguide::render(mode, context, self.columns, max_rows, &self.theme);
            let height = lines.len();
            (lines, height)
        };
        let height_changed = old_height != reserved_rows;
        self.keyguide = Some(CachedKeyGuide {
            context,
            columns: self.columns,
            max_rows,
            lines,
            space,
            overlay_ready: self.guide_popup.ready_for_display(),
            expanded_x,
            expanded_columns,
            expanded_rows,
            reserved_rows,
        });
        self.keyguide_dirty = false;
        height_changed
    }

    fn tree_height(&self) -> usize {
        self.rows.saturating_sub(
            2 + self
                .keyguide
                .as_ref()
                .map_or(0, |cached| cached.reserved_rows),
        )
    }

    fn tree_end(&self) -> usize {
        self.rows.min(2 + self.tree_height())
    }

    fn reconcile_viewport(&mut self, reveal: bool) -> bool {
        let height_changed = self.refresh_keyguide();
        let old_scroll = self.scroll;
        self.scroll = self
            .scroll
            .min(self.tree_cache.len().saturating_sub(self.tree_height()));
        if (reveal || height_changed) && !self.bootstrap_view_pending {
            self.ensure_cursor_visible();
        }
        let hover_changed = self.reconcile_pointer();
        if height_changed || self.scroll != old_scroll {
            self.last_click = None;
            return true;
        }
        hover_changed
    }

    fn request_mode(&mut self, input_mode: InputMode) {
        self.mode_intent = Some(input_mode);
        self.keyguide_dirty = true;
        switch_to_input_mode(&input_mode);
    }

    fn enter_control(&mut self) {
        if self.keyboard_control {
            return;
        }
        self.control_terminal = self.native_focus;
        self.keyboard_control = true;
        self.control_has_focus = false;
        self.local_selection = true;
        self.refresh_tooltip();
        set_selectable(true);
        self.request_mode(InputMode::Normal);
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
        if self.hover.is_none() && self.popup_item.is_some() {
            // A probe made while the sidebar owned focus cannot settle dismissal
            // after that ownership ends. Recheck the physical pointer now.
            self.popup_probe = true;
            self.popup_probe_revision = None;
        }
        self.refresh_tooltip();
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
        if self.focused.is_open() && matches!(message.payload.as_deref(), Some("toggle" | "leave"))
        {
            self.close_focused(true);
            return true;
        }
        match message.payload.as_deref() {
            Some("toggle") if !self.keyboard_control => self.enter_control(),
            Some("toggle" | "leave") => {
                self.leave_control(true);
                self.request_mode(InputMode::Locked);
            }
            _ => return false,
        }
        true
    }

    fn enqueue(&mut self, action: Action) {
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
        // New sidebars project the existing post-action application mode.
        let return_mode = matches!(
            action,
            Action::CreateWorkspace { .. }
                | Action::CreatePane { .. }
                | Action::SelectWorkspace { .. }
                | Action::SelectPane { .. }
        )
        .then_some(InputMode::Locked);
        let creates_sidebar = match &action {
            Action::CreateWorkspace { .. } | Action::CreatePane { .. } => true,
            Action::SelectWorkspace { workspace_id } => {
                self.snapshot.as_ref().is_some_and(|state| {
                    !state
                        .panes
                        .iter()
                        .any(|pane| &pane.workspace_id == workspace_id)
                })
            }
            _ => false,
        };
        let view = creates_sidebar.then(|| native::SidebarView {
            collapsed: self.collapsed.clone(),
            scroll: self.scroll,
        });
        if creates_sidebar {
            self.leave_control(false);
            self.request_mode(InputMode::Locked);
        }
        self.queue.push_back(QueuedRequest {
            request: Request::new(self.request_id, action),
            return_mode,
            view,
        });
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
                .any(|queued| Self::user_action(&queued.request.action))
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

    fn timer(&mut self, now: Instant) -> bool {
        let active = self.working_clock_visible();
        let animated = self.working_clock.tick(now, active);
        let expired = self.expire_user_action(now);
        animated || expired
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
        self.in_flight_return_mode = None;
        self.in_flight_view = None;
        self.response_state_valid = false;
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
        let Some(QueuedRequest {
            request,
            return_mode,
            view,
        }) = self.queue.pop_front()
        else {
            return;
        };
        self.response_state_valid = false;
        if let Err(error) = native::send(
            self.native_owner_id,
            &self.native_client,
            self.client_id,
            &request,
            return_mode,
            view.as_ref(),
        ) {
            let error = format!("Cannot encode native request: {error}");
            self.error = Some(error.clone());
            self.finish_focused_submission(request.request_id, Some(error));
            self.finish_selection(request.request_id, true);
        } else {
            self.in_flight = Some(request);
            self.in_flight_return_mode = return_mode;
            self.in_flight_view = view;
        }
    }

    fn startup(&mut self) {
        self.error = None;
        self.enqueue(Action::RegisterFrontend);
        self.enqueue(Action::Snapshot);
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
        let return_mode = self.in_flight_return_mode.take();
        self.in_flight_view = None;
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
                .map(|state| self.accept_snapshot(state))
                .unwrap_or(self.response_state_valid);
            if !valid && self.error.is_none() {
                self.error = Some("Native owner response arrived without valid state".to_owned());
            }
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
                self.finish_focused_submission(request.request_id, None);
                for effect in response.effects {
                    // A superseded click must not move host focus back to an older target.
                    if !obsolete_selection || !matches!(effect, Effect::FocusPane { .. }) {
                        self.effect(effect, return_mode);
                    }
                }
            }
        }
        if failed {
            self.finish_focused_submission(request.request_id, self.error.clone());
        }
        self.finish_selection(request.request_id, failed);
        self.ensure_cursor_visible();
        self.dispatch();
    }

    fn validate_snapshot(&self, state: &Snapshot) -> Result<bool, String> {
        if state.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported native owner protocol {} (need {PROTOCOL_VERSION})",
                state.protocol
            ));
        }
        if state.instance_id.is_empty() {
            return Err("Missing native owner instance identity".to_owned());
        }
        if !self.snapshot_is_bootstrap
            && let Some(current) = &self.snapshot
            && current.instance_id == state.instance_id
        {
            if state.revision < current.revision {
                return Ok(false);
            }
            if state.revision == current.revision {
                return if state == current {
                    Ok(false)
                } else {
                    Err("Conflicting state at the same native owner revision".to_owned())
                };
            }
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
            return Err("Malformed native owner workspace/pane identities".to_owned());
        }
        Ok(true)
    }

    fn accept_snapshot(&mut self, state: Snapshot) -> bool {
        let valid = match self.validate_snapshot(&state) {
            Ok(true) => {
                if !self.snapshot_is_bootstrap
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|current| current.instance_id != state.instance_id)
                {
                    self.reset_pending_actions();
                    self.close_focused(false);
                }
                let follow_focus = self.desired_selection.is_none()
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
                self.theme.prepare_snapshot(&state);
                self.snapshot = Some(state);
                self.snapshot_is_bootstrap = false;
                self.bootstrap_view_pending &= !follow_focus;
                self.rebuild_tree();
                self.reconcile_cursor();
                if follow_focus {
                    self.ensure_cursor_visible();
                }
                self.refresh_tooltip();
                self.refresh_focused();
                true
            }
            Ok(false) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        };
        self.response_state_valid = valid;
        valid
    }

    fn rebuild_tree(&mut self) {
        let Some(state) = &self.snapshot else { return };
        self.collapsed
            .retain(|id| state.workspaces.iter().any(|workspace| &workspace.id == id));
        self.tree_cache = ui::tree(state, &self.collapsed);
        self.items = ui::items(&self.tree_cache);
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
        } else if !self.local_selection && self.modal.is_none() {
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

    fn effect(&mut self, effect: Effect, return_mode: Option<InputMode>) {
        let Effect::FocusPane { zellij_pane_id } = effect;
        let exists = self
            .manifest
            .panes
            .values()
            .flatten()
            .any(|pane| !pane.is_plugin && pane.id == zellij_pane_id);
        if exists {
            let return_mode = return_mode.unwrap_or(InputMode::Locked);
            self.leave_control(false);
            self.request_mode(return_mode);
            focus_terminal_pane(zellij_pane_id, false, false);
        } else {
            self.error = Some(format!(
                "Native pane {zellij_pane_id} is no longer open; select again"
            ));
            self.enqueue(Action::Snapshot);
        }
    }

    fn pane_update(&mut self, manifest: PaneManifest) {
        self.manifest = manifest;
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
                // A tiled pane can remain focused behind the floating layer; passive
                // nonselectable previews are not keyboard focus even when marked focused.
                panes
                    .iter()
                    .find(|pane| {
                        pane.is_focused
                            && pane.is_selectable
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
        let had_focus = self.control_has_focus;
        if self.keyboard_control && !sidebar_focused && focus_known && had_focus {
            self.leave_control(false);
        }
        if focus_known {
            self.control_has_focus = sidebar_focused;
        }
        if had_focus && !self.control_has_focus && self.hover.is_none() {
            self.popup_probe = self.popup_item.is_some();
            self.popup_probe_revision = None;
        }
        if self.local_selection
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
            || self.queue.iter().any(|queued| selects(&queued.request))
    }

    fn control_active(&self) -> bool {
        self.control_has_focus
    }

    fn pointer_columns(&self) -> usize {
        self.manifest
            .panes
            .values()
            .flatten()
            .find(|pane| pane.is_plugin && pane.id == self.id)
            .map_or(self.columns, |pane| {
                self.columns.max(
                    pane.pane_columns
                        .saturating_sub(pane.pane_content_x.saturating_sub(pane.pane_x)),
                )
            })
    }

    fn pointer_item_at(&self, line: isize, column: usize) -> Option<Item> {
        (line >= 2 && (line as usize) < self.tree_end() && column < self.pointer_columns())
            .then(|| self.scroll + line as usize - 2)
            .and_then(|index| ui::item_at(&self.tree_cache, index))
    }

    fn reconcile_pointer(&mut self) -> bool {
        let previous = self.pointer_item.take();
        self.pointer_item = self
            .pointer_position
            .and_then(|(line, column)| self.pointer_item_at(line, column));
        let hover_changed = self.pointer_highlight && self.hover != self.pointer_item;
        if hover_changed {
            self.hover = self.pointer_item.clone();
        }
        if previous.is_some()
            && self.pointer_item.is_none()
            && !self.control_active()
            && self.popup_item.is_some()
        {
            // Native hover reaches the new pane before its old-pane leave event,
            // but plugin pipes are asynchronous. Ask the helper before hiding it.
            self.popup_probe = true;
            self.popup_probe_revision = None;
        }
        hover_changed
    }

    fn hide_tooltip(&mut self) {
        self.hover = None;
        self.pointer_position = None;
        self.pointer_item = None;
        self.pointer_highlight = false;
        self.popup_hover = false;
        self.popup_probe = false;
        self.popup_probe_revision = None;
        self.popup_item = None;
        self.tooltip.update(self.id, &self.configuration, None);
    }

    fn accept_pointer(&mut self, source: &PipeSource, notice: &overlay::PointerNotice) -> bool {
        let PipeSource::Plugin(source) = source else {
            return false;
        };
        if !self.tooltip.accept_pointer(*source, notice) {
            return false;
        }
        let before = (
            self.popup_item.is_some(),
            self.popup_hover,
            self.hover.is_some(),
        );
        if notice.inside {
            // The physical pointer cannot still hover a sidebar row once it is
            // inside the popup. The host's old-pane leave may arrive later.
            self.pointer_position = None;
            self.pointer_item = None;
            self.pointer_highlight = false;
            self.hover = None;
        }
        self.popup_hover = notice.inside;
        self.popup_probe = false;
        self.popup_probe_revision = None;
        self.refresh_tooltip();
        before
            != (
                self.popup_item.is_some(),
                self.popup_hover,
                self.hover.is_some(),
            )
    }

    fn accept_ci_refresh(
        &mut self,
        source: &PipeSource,
        request: &overlay::RefreshRequest,
    ) -> Option<bkpr::ci_contract::CiRefreshTarget> {
        let PipeSource::Plugin(source) = source else {
            return None;
        };
        if !self.granted || !self.visible || self.modal.is_some() {
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

    fn constrain_tooltip_to_guide(
        guide: Option<&CachedKeyGuide>,
        mut placement: overlay::Placement,
    ) -> Option<overlay::Placement> {
        let Some(guide) = guide.filter(|guide| guide.expanded_columns > 0) else {
            return Some(placement);
        };
        let space = guide.space?;
        let x = placement.coordinates.x?.to_fixed(0);
        let columns = placement.coordinates.width?.to_fixed(0);
        if x >= guide.expanded_x.saturating_add(guide.expanded_columns)
            || guide.expanded_x >= x.saturating_add(columns)
        {
            return Some(placement);
        }
        let bottom = space.bottom.saturating_sub(guide.expanded_rows);
        let rows = placement.coordinates.height?.to_fixed(0).min(bottom);
        if rows < 2 {
            return None;
        }
        let y = placement.coordinates.y?.to_fixed(0).min(bottom - rows);
        placement.coordinates = placement
            .coordinates
            .with_y_fixed(y)
            .with_height_fixed(rows);
        Some(placement)
    }

    fn refresh_tooltip(&mut self) {
        if !self.granted || self.overlay_role {
            return;
        }
        self.reconcile_viewport(false);
        if !self.visible || self.modal.is_some() || self.focused.is_open() {
            self.hide_tooltip();
            return;
        }
        let opening = self.popup_item.is_none();
        if let Some(item) = self.hover.as_ref() {
            if self.popup_item.as_ref() != Some(item) {
                self.popup_item = Some(item.clone());
            }
        } else if self.control_active() {
            if self.popup_item.is_none() {
                self.popup_item = self.cursor.clone();
            }
        } else if !self.popup_hover && !self.popup_probe {
            self.popup_item = None;
        }
        if opening && self.popup_item.is_some() {
            let now = Self::popup_clock();
            self.popup_cache.retain(|entry| entry.formatted_at == now);
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
        let height = self.tree_height();
        let tooltip = if self.visible && self.modal.is_none() && !self.focused.is_open() {
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
                    if height == 0
                        || *band.end() < self.scroll
                        || *band.start() >= self.scroll + height
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
                    let placement =
                        Self::constrain_tooltip_to_guide(self.keyguide.as_ref(), placement)?;
                    Some((content, placement))
                })
        } else {
            None
        };
        let previous_revision = self.tooltip.pending.as_ref().map(|state| state.revision);
        self.tooltip.update(self.id, &self.configuration, tooltip);
        let revision = self
            .tooltip
            .pending
            .as_ref()
            .filter(|state| state.visible)
            .map(|state| state.revision);
        if revision.is_none() {
            self.popup_hover = false;
            self.popup_probe = false;
            self.popup_probe_revision = None;
        } else if revision != previous_revision && self.popup_hover {
            // Pointer state belongs to a frame, not to an item across reflow.
            self.popup_hover = false;
            self.popup_probe = true;
            self.popup_probe_revision = None;
        }
        if self.popup_probe && self.popup_probe_revision != revision {
            self.popup_probe_revision = revision;
            if !self.tooltip.probe_pointer(self.id) {
                self.popup_probe = false;
                self.popup_probe_revision = None;
                if !self.control_active() && self.hover.is_none() {
                    self.popup_item = None;
                    self.tooltip.update(self.id, &self.configuration, None);
                }
            }
        }
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
    }

    fn ensure_cursor_visible(&mut self) {
        self.refresh_keyguide();
        let Some(cursor) = &self.cursor else { return };
        let Some(first) = self
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(cursor))
        else {
            return;
        };
        let height = self.tree_height();
        if height == 0 {
            return;
        }
        let visible_rows = height.min(2);
        if first < self.scroll {
            self.scroll = first;
        } else if first + visible_rows > self.scroll + height {
            self.scroll = first + visible_rows - height;
        }
        self.scroll = self
            .scroll
            .min(self.tree_cache.len().saturating_sub(height));
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
        self.rebuild_tree();
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
        if !close {
            self.open_focused(focused::Kind::Rename { item, name });
            return;
        }
        self.modal = Some(Modal::Close { item, name });
        self.hide_tooltip();
        self.request_mode(InputMode::Locked);
    }

    fn modal_key(&mut self, key: KeyWithModifier) {
        if key.bare_key == BareKey::Esc {
            self.modal = None;
            self.request_mode(InputMode::Normal);
            return;
        }
        if matches!(self.modal, Some(Modal::Close { .. })) && key.bare_key == BareKey::Char('n') {
            self.modal = None;
            self.request_mode(InputMode::Normal);
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
                Modal::Close { item, .. } => match item {
                    Item::Workspace(workspace_id) => Action::CloseWorkspace { workspace_id },
                    Item::Pane(pane_id) => Action::ClosePane { pane_id },
                },
            };
            self.request_mode(InputMode::Normal);
            self.enqueue(action);
        }
    }

    fn key(&mut self, key: KeyWithModifier) {
        self.reconcile_viewport(false);
        self.last_click = None;
        if self.focused.is_open() {
            if key.bare_key == BareKey::Esc
                && key.key_modifiers.is_empty()
                && self
                    .focused
                    .context
                    .as_ref()
                    .is_some_and(|context| !context.ready)
            {
                self.cancel_focused(true);
            } else {
                self.focused
                    .input(focused::Input::Key(key), self.id, &self.configuration);
            }
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            self.refresh_tooltip();
            return;
        }
        if key.key_modifiers.contains(&KeyModifier::Ctrl)
            || key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return;
        }
        self.local_selection = true;
        let bare_key = match key.bare_key {
            BareKey::Char(character) if key.key_modifiers.contains(&KeyModifier::Shift) => {
                BareKey::Char(character.to_ascii_uppercase())
            }
            bare_key => bare_key,
        };
        let keyboard_selection = matches!(
            bare_key,
            BareKey::Up
                | BareKey::Down
                | BareKey::PageUp
                | BareKey::PageDown
                | BareKey::Home
                | BareKey::End
                | BareKey::Enter
                | BareKey::Left
                | BareKey::Right
                | BareKey::Char('k' | 'j' | 'g' | 'G' | ' ')
        );
        if keyboard_selection {
            self.pointer_highlight = false;
            self.hover = None;
        }
        match bare_key {
            BareKey::Char('/') if self.keyboard_control => self.open_focused(focused::Kind::Finder),
            BareKey::Up | BareKey::Char('k') => self.navigate(-1),
            BareKey::Down | BareKey::Char('j') => self.navigate(1),
            BareKey::PageUp => self.navigate(-(self.tree_height() as isize / 2).max(1)),
            BareKey::PageDown => self.navigate((self.tree_height() as isize / 2).max(1)),
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
                    let folded = match bare_key {
                        BareKey::Left => Some(true),
                        BareKey::Right => Some(false),
                        _ => None,
                    };
                    self.fold(workspace, folded);
                }
            }
            BareKey::Char('n') => self.open_focused(focused::Kind::Workspace),
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
                    self.open_focused(focused::Kind::Pane { workspace_id });
                }
            }
            BareKey::Char('r') => self.edit_selected(false),
            BareKey::Char('x') | BareKey::Delete => self.edit_selected(true),
            BareKey::Char('R') => {
                self.error = None;
                self.startup();
            }
            _ => {}
        }
        if keyboard_selection {
            self.popup_item = self.cursor.clone();
            self.refresh_tooltip();
        }
    }

    fn popup_clock() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_secs())
    }

    fn mouse(&mut self, mouse: Mouse) -> bool {
        if self.modal.is_some() || self.focused.is_open() {
            return false;
        }
        let viewport_changed = self.reconcile_viewport(false);
        let (line, column) = match mouse {
            Mouse::Hover(line, column) | Mouse::LeftClick(line, column) => (line, column),
            Mouse::ScrollUp(lines) => {
                let previous = self.scroll;
                self.scroll = self.scroll.saturating_sub(lines);
                self.reconcile_pointer();
                if self.scroll != previous {
                    self.refresh_tooltip();
                }
                return viewport_changed || self.scroll != previous;
            }
            Mouse::ScrollDown(lines) => {
                let previous = self.scroll;
                self.scroll = self
                    .scroll
                    .saturating_add(lines)
                    .min(self.tree_cache.len().saturating_sub(self.tree_height()));
                self.reconcile_pointer();
                if self.scroll != previous {
                    self.refresh_tooltip();
                }
                return viewport_changed || self.scroll != previous;
            }
            _ => return viewport_changed,
        };
        if matches!(mouse, Mouse::LeftClick(_, _)) && column >= self.columns {
            return viewport_changed;
        }
        self.pointer_position =
            (line >= 0 && (line as usize) < self.rows && column < self.pointer_columns())
                .then_some((line, column));
        self.pointer_highlight = true;
        let hover_changed = self.reconcile_pointer();
        self.refresh_tooltip();
        let changed = viewport_changed || hover_changed;
        if matches!(mouse, Mouse::Hover(_, _)) {
            return changed;
        }
        let Some(item) = self.pointer_item.clone() else {
            self.last_click = None;
            return changed;
        };
        self.local_selection = true;
        self.cursor = Some(item.clone());
        let now = Instant::now();
        let double = self.last_click.as_ref().is_some_and(|(old, at)| {
            old == &item && now.duration_since(*at) < Duration::from_millis(350)
        });
        if double {
            self.last_click = None;
            let group = self
                .snapshot
                .as_ref()
                .and_then(|state| item.workspace_id(state).map(str::to_owned));
            if let Some(group) = group {
                self.fold(group, None);
            }
        } else {
            self.last_click = Some((item, now));
        }
        self.activate();
        self.refresh_tooltip();
        true
    }

    fn sidebar_geometry(&self) -> Option<[usize; 8]> {
        self.manifest
            .panes
            .values()
            .flatten()
            .find(|pane| pane.is_plugin && pane.id == self.id)
            .map(|pane| {
                [
                    pane.pane_x,
                    pane.pane_y,
                    pane.pane_columns,
                    pane.pane_rows,
                    pane.pane_content_x,
                    pane.pane_content_y,
                    pane.pane_content_columns,
                    pane.pane_content_rows,
                ]
            })
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

    fn working_clock_visible(&self) -> bool {
        if !self.granted || !self.visible || self.modal.is_some() || self.focused.is_open() {
            return false;
        }
        if let Some(tab) = self.tabs.iter().find(|tab| tab.active)
            && !self.manifest.panes.get(&tab.position).is_some_and(|panes| {
                panes.iter().any(|pane| {
                    pane.is_plugin
                        && pane.id == self.id
                        && !pane.is_suppressed
                        && (!pane.is_floating || tab.are_floating_panes_visible)
                })
            })
        {
            return false;
        }
        let Some(state) = self.snapshot.as_ref() else {
            return false;
        };
        let recognized = |pane: &bkpr::Pane| {
            pane.harness
                .as_deref()
                .is_some_and(|harness| bkpr::agent_contract::harness_name(harness).is_some())
        };
        self.tree_cache
            .iter()
            .skip(self.scroll)
            .take(self.tree_height())
            .any(|row| match row.kind {
                ui::RowKind::Workspace {
                    index,
                    collapsed: true,
                } => {
                    let workspace = &state.workspaces[index];
                    // Left gutter, workspace dot and the complete two-cell status badge.
                    self.columns >= 5
                        && workspace.status == bkpr::Status::Working
                        && state
                            .panes
                            .iter()
                            .any(|pane| pane.workspace_id == workspace.id && recognized(pane))
                }
                ui::RowKind::Pane { index, .. } => {
                    let pane = &state.panes[index];
                    // Left gutter, tree lead, project dot and the complete status badge.
                    self.columns >= 8 && pane.status == bkpr::Status::Working && recognized(pane)
                }
                _ => false,
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
        if resized {
            self.refresh_focused();
        }
        let viewport_changed = self.reconcile_viewport(resized);
        if rows > 0 && columns > 0 {
            self.bootstrap_view_pending = false;
        }
        let tree_end = self.tree_end();
        if (resized || viewport_changed) && self.popup_item.is_some() {
            // Host commands must precede the sidebar's rendering bytes.
            self.refresh_tooltip();
        }
        let working_clock_visible = self.working_clock_visible();
        self.working_clock
            .schedule(Instant::now(), working_clock_visible);
        let tree = &self.tree_cache;
        let selected_group = self.cursor.as_ref().and_then(|item| match item {
            Item::Pane(_) => self
                .snapshot
                .as_ref()
                .and_then(|state| item.workspace_id(state)),
            _ => None,
        });
        // nf-md-beekeeper
        let heading = self
            .snapshot
            .as_ref()
            .map(|state| format!("\u{f14e2} bkpr · {}", state.session))
            .unwrap_or_else(|| "\u{f14e2} bkpr".to_owned());
        let status = if let Some(error) = &self.error {
            error.clone()
        } else if !self.granted {
            "Waiting for permissions".to_owned()
        } else if let Some(state) = &self.snapshot {
            if !state.connected {
                state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Native owner unavailable".to_owned())
            } else if state.error.is_some() {
                state.error.clone().unwrap_or_default()
            } else if self.synchronizing(Instant::now()) {
                "Synchronizing…".to_owned()
            } else {
                String::new()
            }
        } else {
            "Connecting to native owner…".to_owned()
        };
        let modal_render = self.modal.as_ref().map(Modal::lines);
        // None means not initialized; Some(None) records an explicit host cursor hide.
        if self.render_cursor != Some(None) {
            self.render_cursor = Some(None);
            show_cursor(None);
        }
        write!(output, "{}", self.theme.begin_graphics_frame()).expect("clearing icon placements");
        for row in 0..rows {
            let line: Cow<'_, str> = if row >= tree_end {
                Cow::Borrowed(
                    self.keyguide
                        .as_ref()
                        .expect("formatted key guide")
                        .lines
                        .get(row - tree_end)
                        .map_or("", String::as_str),
                )
            } else {
                Cow::Owned(if row == 0 {
                    let label = ui::truncate(&heading, columns);
                    let padding = columns.saturating_sub(ui::width(&label));
                    let mut line = String::with_capacity(label.len() + padding + 12);
                    line.push_str("\u{1b}[1m");
                    line.extend(std::iter::repeat_n(' ', padding / 2));
                    line.push_str(&label);
                    line.extend(std::iter::repeat_n(' ', padding - padding / 2));
                    line.push_str(ui::RESET);
                    line
                } else if row == 1 && status.is_empty() {
                    String::new()
                } else if row == 1 {
                    let color = if !status.is_empty()
                        && (self.error.is_some()
                            || self.snapshot.as_ref().is_some_and(|state| !state.connected))
                    {
                        ui::status_color(bkpr::Status::Blocked, &self.theme)
                    } else {
                        self.theme.muted.as_str()
                    };
                    format!("{color}{}{}", ui::fit(&status, columns), ui::RESET)
                } else if let Some(lines) = &modal_render {
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
                        ui::RowStyle {
                            selected: tree_row.item.is_some() && tree_row.item == self.cursor,
                            hovered: tree_row.item.is_some() && tree_row.item == self.hover,
                            selected_group: tree_row
                                .workspace_id
                                .as_deref()
                                .is_some_and(|id| selected_group == Some(id)),
                            clock_frame: self.working_clock.frame(),
                        },
                    )
                } else if row == 2
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|state| state.workspaces.is_empty())
                {
                    ui::fit("No workspaces · n to create", columns)
                } else {
                    String::new()
                })
            };
            // Each host render starts with a fresh grid. EL0 from column one preserves
            // Zellij's current background (EL2 drops it), without parsing padded spaces.
            let line = if line.is_empty() {
                "\u{1b}[0K"
            } else {
                line.as_ref()
            };
            write!(
                output,
                "\u{1b}[{};1H{}{}{line}{}",
                row + 1,
                ui::RESET,
                self.theme.base,
                ui::RESET
            )
            .expect("writing sidebar row");
        }
    }
}

impl ZellijPlugin for Frontend {
    fn load(&mut self, mut configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.id = ids.plugin_id;
        self.client_id = ids.client_id;
        self.tooltip.client_id = ids.client_id;
        self.focused.client_id = ids.client_id;
        self.visible = true;
        self.overlay_role = configuration
            .get("role")
            .is_some_and(|role| role == "overlay");
        self.guide_role = configuration.get("role").map(String::as_str) == Some("guide-overlay");
        self.native_owner_role =
            configuration.get("role").map(String::as_str) == Some("native-owner");
        self.focused_role =
            configuration.get("role").map(String::as_str) == Some("focused-overlay");
        if !self.overlay_role && !self.guide_role && !self.native_owner_role && !self.focused_role {
            self.extract_sidebar_bootstrap(&mut configuration);
            // Host event routing is not part of the sidebar's application configuration.
            configuration.remove("mode_info_format");
            configuration.remove("shared_tiled_width_group");
            configuration.remove("shared_tiled_width_initial");
        } else {
            configuration.remove("sidebar_bootstrap");
        }
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
            self.overlay
                .load(&self.configuration, self.id, ids.client_id);
            return;
        }
        if self.guide_role {
            self.guide_view.load(&self.configuration, self.id);
            return;
        }
        if self.native_owner_role {
            self.native_owner.load(&self.configuration);
            return;
        }
        if self.focused_role {
            self.focused_view
                .load(&self.configuration, self.id, ids.client_id);
            return;
        }
        set_selectable(true);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Key,
            // Subscribe to ModeUpdate dispatch; lossless JSON arrives as a CustomMessage.
            EventType::ModeUpdate,
            EventType::InitialKeybinds,
            EventType::CustomMessage,
            EventType::Mouse,
            EventType::Timer,
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::PaneClosed,
            EventType::Visible,
            EventType::BeforeClose,
            EventType::PastedText,
        ]);
        let valid = native::configuration_valid(&self.configuration, "sidebar");
        if !valid {
            self.error = Some("Invalid native sidebar configuration".to_owned());
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
        if let Event::CustomMessage(name, payload) = &event
            && name == "bkpr_terminal_colors"
            && payload.len() <= MAX_MESSAGE_BYTES
        {
            return self.terminal_colors_message(payload);
        }
        if self.overlay_role {
            return self.overlay.event(event);
        }
        if self.guide_role {
            return self.guide_view.event(event);
        }
        if self.native_owner_role {
            return self.native_owner.event(event);
        }
        if self.focused_role {
            return self.focused_view.event(event);
        }
        if matches!(&event, Event::Timer(_)) {
            native::expire_control_ready(self);
        }
        if matches!(&event, Event::BeforeClose) {
            native::settle_control_ready(
                self,
                Some("Selected sidebar closed before permission readiness"),
            );
        }
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                native::settle_control_ready(self, None);
                set_selectable(false);
                if !self.apply_sidebar_bootstrap() {
                    self.request_mode(InputMode::Locked);
                }
                self.startup();
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Required permissions denied; reload to authorize".to_owned());
                native::settle_control_ready(self, Some("Normal sidebar permissions were denied"));
            }
            Event::PaneUpdate(manifest) if self.granted => {
                let before = (
                    self.native_focus,
                    self.local_selection,
                    self.scroll,
                    self.popup_item.clone(),
                    self.sidebar_geometry(),
                    self.keyguide_context(),
                );
                self.pane_update(manifest);
                return before
                    != (
                        self.native_focus,
                        self.local_selection,
                        self.scroll,
                        self.popup_item.clone(),
                        self.sidebar_geometry(),
                        self.keyguide_context(),
                    );
            }
            Event::TabUpdate(tabs) if self.granted => {
                let before = (
                    self.native_focus,
                    self.local_selection,
                    self.scroll,
                    self.popup_item.clone(),
                    self.keyguide_context(),
                    self.tabs
                        .iter()
                        .find(|tab| tab.active)
                        .map(|tab| [tab.display_area_columns, tab.display_area_rows]),
                );
                if self
                    .tabs
                    .iter()
                    .find(|tab| tab.active)
                    .map(|tab| tab.position)
                    != tabs.iter().find(|tab| tab.active).map(|tab| tab.position)
                {
                    self.popup_hover = false;
                    self.popup_probe = false;
                    self.popup_probe_revision = None;
                    self.close_focused(false);
                }
                self.tabs = tabs;
                self.synchronize_focus();
                self.refresh_tooltip();
                self.refresh_focused();
                return before
                    != (
                        self.native_focus,
                        self.local_selection,
                        self.scroll,
                        self.popup_item.clone(),
                        self.keyguide_context(),
                        self.tabs
                            .iter()
                            .find(|tab| tab.active)
                            .map(|tab| [tab.display_area_columns, tab.display_area_rows]),
                    );
            }
            Event::PaneClosed(PaneId::Plugin(id)) => {
                self.tooltip.forget(id);
                self.guide_popup.forget(id);
                self.keyguide_dirty = true;
                if self.focused.id == Some(id) {
                    self.focused.forget(id);
                    self.close_focused(true);
                }
                if self.native_owner_id == Some(id) && self.granted {
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
            Event::CustomMessage(name, payload)
                if name == "mode_info" && payload.len() <= MAX_MESSAGE_BYTES =>
            {
                return self.mode_message(&payload);
            }
            Event::CustomMessage(name, payload)
                if name == "keybinds_info" && payload.len() <= MAX_MESSAGE_BYTES =>
            {
                return self.keybinds_message(payload);
            }
            Event::Key(key) if self.granted => self.key(key),
            Event::Mouse(mouse) if self.granted => return self.mouse(mouse),
            Event::Timer(_) if self.granted => {
                let now = Instant::now();
                return self.timer(now);
            }
            Event::PastedText(text) if self.granted => {
                if self.focused.is_open() {
                    self.focused
                        .input(focused::Input::Paste(text), self.id, &self.configuration);
                }
            }
            Event::Visible(visible) => {
                if self.visible == visible {
                    return false;
                }
                self.visible = visible;
                self.keyguide_dirty = true;
                if !visible {
                    self.working_clock.schedule(Instant::now(), false);
                    self.close_focused(false);
                    self.leave_control(false);
                    self.control_has_focus = false;
                    self.local_selection = false;
                    self.reconcile_cursor();
                    self.hide_tooltip();
                    self.guide_popup.update(self.id, &self.configuration, None);
                } else {
                    self.synchronize_focus();
                    self.reconcile_cursor();
                    self.ensure_cursor_visible();
                    self.refresh_tooltip();
                }
            }
            Event::BeforeClose if self.granted => {
                self.working_clock.schedule(Instant::now(), false);
                self.close_focused(false);
                self.leave_control(false);
                self.tooltip.close(self.id, &self.configuration);
                self.guide_popup.close(self.id, &self.configuration);
            }
            _ => return false,
        }
        true
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if self.focused_role {
            return self.focused_view.pipe(message);
        }
        // Broadcast CLI pipes must be acknowledged by every loaded replica, not only owners.
        if message.name == native::CONTROL && !self.native_owner_role {
            return native::sidebar_control_pipe(self, &message);
        }
        if self.overlay_role {
            return self.overlay.pipe(message);
        }
        if self.guide_role {
            return self.guide_view.pipe(message);
        }
        if self.native_owner_role {
            return self.native_owner.pipe(message);
        }
        if self.granted
            && matches!(
                message.name.as_str(),
                focused::READY | focused::CANCEL | focused::SELECTED | focused::SUBMIT
            )
        {
            return self.focused_pipe(&message);
        }
        if message.name == CONTROL_PIPE {
            return self.control_pipe(&message);
        }
        if self.granted && matches!(message.name.as_str(), native::RESPONSE | native::STATE) {
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
                let Ok(identity) = serde_json::from_str::<NativeResponseIdentity<'_>>(&payload)
                else {
                    return false;
                };
                if identity.client != self.native_client
                    || identity.client_id != self.client_id
                    || self
                        .in_flight
                        .as_ref()
                        .is_none_or(|request| request.request_id != identity.response.request_id)
                {
                    return false;
                }
                let Ok(envelope) = serde_json::from_str::<native::NativeResponse>(&payload) else {
                    return false;
                };
                self.native_owner_id = Some(owner);
                self.native_response(envelope);
                return true;
            }
            let Ok(identity) = serde_json::from_str::<NativeStateIdentity>(&payload) else {
                return false;
            };
            if identity.client_id != self.client_id {
                return false;
            }
            let Ok(state) = serde_json::from_str::<native::NativeState>(&payload) else {
                self.response_state_valid = false;
                self.error = Some("Malformed native owner state".to_owned());
                return true;
            };
            // Owner broadcasts also reach other client replicas. Only a response
            // acknowledges input; repeated canonical state is not a reason to render.
            let changed = self.snapshot_is_bootstrap
                || self.snapshot.as_ref().is_none_or(|current| {
                    current.instance_id != state.snapshot.instance_id
                        || current.revision < state.snapshot.revision
                });
            let discovered = self.native_owner_id.is_none();
            if !self.accept_snapshot(state.snapshot) {
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
                && let Err(error) = native::send(
                    Some(owner),
                    &self.native_client,
                    self.client_id,
                    request,
                    self.in_flight_return_mode,
                    self.in_flight_view.as_ref(),
                )
            {
                self.error = Some(format!("Cannot encode native request: {error}"));
                return true;
            }
            return changed || relay_changed;
        }
        if message.name == overlay::GUIDE_READY {
            let was_ready = self.guide_popup.ready_for_display();
            if let (PipeSource::Plugin(source), Some(payload)) = (message.source, message.payload)
                && payload.len() <= 4096
                && let Ok(ready) = serde_json::from_str::<overlay::GuideReady>(&payload)
                && ready.id == source
                && self.guide_popup.ready(&ready, self.id, &self.configuration)
                && self.guide_popup.ready_for_display() != was_ready
            {
                self.keyguide_dirty = true;
                self.refresh_tooltip();
                return true;
            }
            return false;
        }
        if message.name == overlay::READY {
            if let (PipeSource::Plugin(source), Some(payload)) = (message.source, message.payload)
                && let Ok(ready) = serde_json::from_str::<overlay::Ready>(&payload)
                && ready.id == source
                && ready.client_id == self.client_id
                && self.tooltip.ready(&ready, self.id, &self.configuration)
            {
                if self.popup_probe {
                    self.popup_probe_revision = None;
                    self.refresh_tooltip();
                }
                return false;
            }
            return false;
        }
        if message.name == overlay::ERROR {
            if let (PipeSource::Plugin(source), Some(payload)) = (message.source, message.payload)
                && payload.len() <= 4096
                && let Ok(fault) = serde_json::from_str::<overlay::FaultNotice>(&payload)
                && self.tooltip.accept_fault(source, &fault)
            {
                self.error = Some(format!("Popup helper: {}", fault.message));
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
        if message.name == overlay::POINTER {
            return message
                .payload
                .as_deref()
                .filter(|payload| payload.len() <= 4096)
                .and_then(|payload| serde_json::from_str::<overlay::PointerNotice>(payload).ok())
                .is_some_and(|notice| self.accept_pointer(&message.source, &notice));
        }
        false
    }

    fn render(&mut self, rows: usize, columns: usize) {
        if self.overlay_role {
            self.overlay.render(rows, columns);
        } else if self.guide_role {
            self.guide_view.render(rows, columns);
        } else if self.focused_role {
            self.focused_view.render(rows, columns);
        } else if !self.native_owner_role {
            self.render_sidebar(rows, columns);
        }
        std::io::stdout().flush().expect("flushing plugin render");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::{Pane, Response, Status, Workspace};

    fn snapshot(revision: u64, focused: &str) -> Snapshot {
        Snapshot {
            protocol: PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision,
            session: "test".to_owned(),
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
                    agent_id: None,
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
            id: 99,
            rows: 20,
            columns: 40,
            ..Frontend::default()
        };
        frontend.accept_snapshot(snapshot(1, "terminal_1"));
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

    fn working_frontend() -> Frontend {
        let mut frontend = popup_frontend();
        let pane = &mut frontend.snapshot.as_mut().unwrap().panes[0];
        pane.harness = Some("omp".to_owned());
        pane.status = Status::Working;
        frontend
    }

    #[test]
    fn sidebar_clock_requires_a_recognized_working_badge() {
        let mut frontend = working_frontend();
        assert!(frontend.working_clock_visible());
        for harness in [None, Some("shell"), Some("unrecognized")] {
            frontend.snapshot.as_mut().unwrap().panes[0].harness = harness.map(str::to_owned);
            assert!(!frontend.working_clock_visible());
            assert!(!frontend.timer(Instant::now()));
        }
        frontend.snapshot.as_mut().unwrap().panes[0].harness = Some("omp".to_owned());
        for status in [Status::Idle, Status::Blocked, Status::Done, Status::Unknown] {
            frontend.snapshot.as_mut().unwrap().panes[0].status = status;
            assert!(!frontend.working_clock_visible());
            assert!(!frontend.timer(Instant::now()));
        }
        frontend.snapshot.as_mut().unwrap().panes[0].status = Status::Working;
        assert!(frontend.working_clock_visible());
    }

    #[test]
    fn sidebar_clock_tracks_viewport_badges_and_collapsed_workspace_aggregate() {
        let mut frontend = working_frontend();
        frontend.keyguide = None;
        frontend.rows = 4;
        assert!(!frontend.working_clock_visible());
        frontend.rows = 5;
        assert!(frontend.working_clock_visible());
        frontend.columns = 7;
        assert!(!frontend.working_clock_visible());
        frontend.columns = 8;
        assert!(frontend.working_clock_visible());
        frontend.rows = 3;
        frontend.scroll = 2;
        assert!(frontend.working_clock_visible());
        frontend.scroll = 3;
        assert!(!frontend.working_clock_visible());

        frontend.rows = 20;
        frontend.columns = 40;
        frontend.scroll = 0;
        frontend.snapshot.as_mut().unwrap().workspaces[0].status = Status::Working;
        frontend.fold("workspace_1".to_owned(), Some(true));
        assert!(frontend.working_clock_visible());
        frontend.columns = 4;
        assert!(!frontend.working_clock_visible());
        frontend.columns = 5;
        assert!(frontend.working_clock_visible());
        // A hidden child's state must not replace the native aggregate badge.
        frontend.snapshot.as_mut().unwrap().workspaces[0].status = Status::Idle;
        assert!(!frontend.working_clock_visible());
        frontend.snapshot.as_mut().unwrap().workspaces[0].status = Status::Working;
        frontend.snapshot.as_mut().unwrap().panes[0].harness = None;
        assert!(!frontend.working_clock_visible());
        frontend.snapshot.as_mut().unwrap().panes[0].harness = Some("omp".to_owned());
        frontend.snapshot.as_mut().unwrap().panes[0].status = Status::Idle;
        frontend.fold("workspace_1".to_owned(), Some(false));
        frontend.columns = 40;
        assert!(!frontend.working_clock_visible());
    }

    #[test]
    fn sidebar_clock_stops_when_hidden_ungranted_modal_or_on_another_tab() {
        let mut frontend = working_frontend();
        assert!(frontend.working_clock_visible());
        frontend.visible = false;
        assert!(!frontend.working_clock_visible());
        frontend.visible = true;
        frontend.granted = false;
        assert!(!frontend.working_clock_visible());
        frontend.granted = true;
        frontend.modal = Some(Modal::Close {
            item: Item::Pane("terminal_1".to_owned()),
            name: "Terminal 1".to_owned(),
        });
        assert!(!frontend.working_clock_visible());
        frontend.modal = None;
        frontend.tabs[0].position = 1;
        assert!(!frontend.working_clock_visible());
        frontend.tabs[0].position = 0;
        frontend.manifest.panes.get_mut(&0).unwrap()[0].is_suppressed = true;
        assert!(!frontend.working_clock_visible());
        frontend.manifest.panes.get_mut(&0).unwrap()[0].is_suppressed = false;
        assert!(frontend.working_clock_visible());
    }

    #[test]
    fn sidebar_animation_and_user_action_timers_share_deadlines_without_consuming_each_other() {
        let start = Instant::now();
        let mut frontend = working_frontend();
        frontend.in_flight = Some(Request::new(
            1,
            Action::RenamePane {
                pane_id: "terminal_1".to_owned(),
                label: "Renamed".to_owned(),
            },
        ));
        frontend.pending_user_action = Some(PendingUserAction {
            started: start,
            shown: false,
        });
        frontend.working_clock.schedule(start, true);
        assert!(!frontend.timer(start + Duration::from_millis(199)));
        assert!(frontend.timer(start + Duration::from_millis(200)));
        assert_eq!(frontend.working_clock.frame(), 1);
        assert!(!frontend.timer(start + Duration::from_millis(200)));
        assert!(!frontend.pending_user_action.as_ref().unwrap().shown);
        assert!(frontend.timer(start + Duration::from_secs(2)));
        assert!(frontend.pending_user_action.as_ref().unwrap().shown);
        assert_eq!(frontend.working_clock.frame(), 2);
        assert!(!frontend.timer(start + Duration::from_secs(2)));

        frontend.visible = false;
        assert!(!frontend.timer(start + Duration::from_millis(2200)));
        frontend.visible = true;
        assert!(!frontend.timer(start + Duration::from_millis(2400)));
        frontend
            .working_clock
            .schedule(start + Duration::from_millis(2400), true);
        assert!(!frontend.timer(start + Duration::from_millis(2500)));
        assert!(frontend.timer(start + Duration::from_millis(2600)));
        assert_eq!(frontend.working_clock.frame(), 3);
    }

    fn guide_mode(mode: InputMode) -> ModeInfo {
        ModeInfo {
            mode,
            keybinds: vec![(
                InputMode::Locked,
                vec![(
                    KeyWithModifier {
                        bare_key: BareKey::Char('g'),
                        key_modifiers: BTreeSet::from([KeyModifier::Ctrl]),
                    },
                    vec![actions::Action::SwitchToMode {
                        input_mode: InputMode::Normal,
                    }],
                )],
            )],
            ..ModeInfo::default()
        }
    }

    fn mode_event(mut mode: ModeInfo) -> Event {
        mode.keybinds.clear();
        Event::CustomMessage(
            "mode_info".to_owned(),
            serde_json::to_string(&mode).unwrap(),
        )
    }

    fn keybinds_event(mode: &ModeInfo) -> Event {
        Event::CustomMessage(
            "keybinds_info".to_owned(),
            serde_json::to_string(&mode.keybinds).unwrap(),
        )
    }

    fn bootstrap_configuration(state: Snapshot, info: ModeInfo) -> BTreeMap<String, String> {
        bootstrap_configuration_with_view(state, info, native::SidebarView::default())
    }

    fn bootstrap_configuration_with_view(
        state: Snapshot,
        info: ModeInfo,
        view: native::SidebarView,
    ) -> BTreeMap<String, String> {
        let keymap = native::test_keymap_cache(&state.instance_id, &info.keybinds);
        BTreeMap::from([(
            "sidebar_bootstrap".to_owned(),
            serde_json::json!({
                "snapshot": state,
                "mode": info.mode,
                "base_mode": info.base_mode,
                "keymap": keymap,
                "view": view,
            })
            .to_string(),
        )])
    }

    #[test]
    fn invalid_keymap_cache_cannot_apply_a_partial_bootstrap_but_sdk_replay_can_replace_it() {
        let info = guide_mode(InputMode::Locked);
        let mut configuration = bootstrap_configuration(snapshot(1, "terminal_1"), info.clone());
        let mut encoded: serde_json::Value =
            serde_json::from_str(&configuration["sidebar_bootstrap"]).unwrap();
        encoded["keymap"]["source"] = serde_json::json!("invalid/source");
        configuration.insert("sidebar_bootstrap".to_owned(), encoded.to_string());
        let mut frontend = Frontend {
            granted: true,
            ..Frontend::default()
        };
        frontend.extract_sidebar_bootstrap(&mut configuration.clone());
        assert!(!frontend.apply_sidebar_bootstrap());
        assert!(frontend.snapshot.is_none());
        assert!(frontend.keybinds.is_none());
        assert!(!frontend.response_state_valid);

        frontend.extract_sidebar_bootstrap(&mut configuration);
        assert!(frontend.update(keybinds_event(&info)));
        assert!(frontend.apply_sidebar_bootstrap());
        assert_eq!(frontend.keybinds.as_ref(), Some(&info.keybinds));
        assert!(frontend.snapshot_is_bootstrap);
        assert!(!frontend.response_state_valid);
    }

    #[test]
    fn bootstrap_waits_for_permission_then_renders_a_complete_unfocused_projection() {
        let mut frontend = Frontend {
            render_cursor: Some(None),
            ..Frontend::default()
        };
        let mut configuration =
            bootstrap_configuration(snapshot(1, "terminal_1"), guide_mode(InputMode::Locked));
        frontend.extract_sidebar_bootstrap(&mut configuration);
        assert!(!configuration.contains_key("sidebar_bootstrap"));
        assert!(!frontend.apply_sidebar_bootstrap());
        assert!(frontend.snapshot.is_none());
        assert!(frontend.tree_cache.is_empty());
        let before: String = sidebar_cells(&mut frontend, 24, 64)
            .iter()
            .flatten()
            .collect();
        assert!(!before.contains("Workspace 1"));
        assert!(!before.contains("Terminal 1"));

        frontend.granted = true;
        assert!(frontend.apply_sidebar_bootstrap());
        assert!(frontend.snapshot_is_bootstrap);
        assert!(!frontend.response_state_valid);
        let warm: String = sidebar_cells(&mut frontend, 24, 64)
            .iter()
            .flatten()
            .collect();
        assert!(warm.contains("Workspace 1"));
        assert!(warm.contains("Terminal 1"));
        assert!(!warm.contains("Connecting to native owner"));
        assert!(!warm.contains("Waiting for native keymap"));
        assert!(!frontend.keyboard_control);
        assert!(!frontend.control_has_focus);
        assert!(frontend.native_focus.is_none());
        assert!(frontend.mode_intent.is_none());
        assert!(frontend.update(mode_event(guide_mode(InputMode::Normal))));
        assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
        assert!(!frontend.keyboard_control);
        assert!(!frontend.control_has_focus);
    }

    #[test]
    fn warm_permission_preserves_an_already_authoritative_mode_and_keymap() {
        let mut frontend = Frontend::default();
        let mut configuration =
            bootstrap_configuration(snapshot(1, "terminal_1"), guide_mode(InputMode::Locked));
        frontend.extract_sidebar_bootstrap(&mut configuration);
        let mut actual = guide_mode(InputMode::Normal);
        actual.keybinds.clear();
        assert!(frontend.update(mode_event(actual.clone())));
        assert!(frontend.update(keybinds_event(&actual)));
        assert!(frontend.update(Event::PermissionRequestResult(PermissionStatus::Granted)));
        assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
        assert_eq!(frontend.keybinds.as_ref(), Some(&actual.keybinds));
        assert!(frontend.mode_intent.is_none());
        assert!(frontend.snapshot_is_bootstrap);
        assert!(!frontend.response_state_valid);
        assert!(matches!(
            frontend.in_flight.as_ref().unwrap().action,
            Action::RegisterFrontend
        ));
        assert!(matches!(
            frontend.queue.front().unwrap().request.action,
            Action::Snapshot
        ));
    }

    #[test]
    fn malformed_or_oversized_bootstrap_keeps_the_real_startup_path() {
        let mut invalid_identity = snapshot(1, "terminal_1");
        invalid_identity.panes[0].workspace_id = "missing".to_owned();
        let mut invalid_protocol = snapshot(1, "terminal_1");
        invalid_protocol.protocol = PROTOCOL_VERSION + 1;
        let mut missing_instance = snapshot(1, "terminal_1");
        missing_instance.instance_id.clear();
        let mut payloads = vec!["not json".to_owned(), " ".repeat(MAX_MESSAGE_BYTES + 1)];
        for state in [invalid_identity, invalid_protocol, missing_instance] {
            payloads.push(
                bootstrap_configuration(state, guide_mode(InputMode::Locked))
                    .remove("sidebar_bootstrap")
                    .unwrap(),
            );
        }
        for payload in payloads {
            let mut frontend = Frontend {
                granted: true,
                ..Frontend::default()
            };
            let mut configuration = BTreeMap::from([
                ("sidebar_bootstrap".to_owned(), payload),
                ("role".to_owned(), "sidebar".to_owned()),
            ]);
            frontend.extract_sidebar_bootstrap(&mut configuration);
            assert_eq!(
                configuration,
                BTreeMap::from([("role".to_owned(), "sidebar".to_owned())])
            );
            assert!(!frontend.apply_sidebar_bootstrap());
            assert!(frontend.snapshot.is_none());
            assert!(!frontend.response_state_valid);
            assert!(frontend.error.is_none());
        }
    }

    #[test]
    fn queued_creation_retains_return_mode_after_control_departure() {
        let mut frontend = popup_frontend();
        frontend.mode = Some(ModeState {
            mode: InputMode::Locked,
            base_mode: Some(InputMode::Normal),
        });
        frontend.enter_control();
        frontend.in_flight = Some(Request::new(7, Action::Snapshot));
        frontend.collapsed.insert("workspace_2".to_owned());
        frontend.scroll = 4;
        frontend.enqueue(Action::CreatePane {
            workspace_id: "workspace_1".to_owned(),
            cwd: None,
        });
        frontend.leave_control(false);
        frontend.collapsed.clear();
        frontend.scroll = 0;
        frontend.mode = Some(ModeState {
            mode: InputMode::Normal,
            base_mode: Some(InputMode::Normal),
        });
        frontend.in_flight = None;
        frontend.dispatch();
        assert!(matches!(
            frontend.in_flight.as_ref().unwrap().action,
            Action::CreatePane { .. }
        ));
        assert_eq!(frontend.in_flight_return_mode, Some(InputMode::Locked));
        let view = frontend.in_flight_view.as_ref().unwrap();
        assert_eq!(view.collapsed, BTreeSet::from(["workspace_2".to_owned()]));
        assert_eq!(view.scroll, 4);
    }

    #[test]
    fn warm_first_frame_preserves_source_folds_and_scroll_before_live_focus_changes() {
        let mut source = populated_frontend();
        let info = guide_mode(InputMode::Locked);
        source.update(mode_event(info.clone()));
        source.update(keybinds_event(&info));
        source.collapsed.insert("workspace_1".to_owned());
        source.rebuild_tree();
        source.reconcile_cursor();
        sidebar_cells(&mut source, 12, 40);
        source.scroll = 8;
        let frame = sidebar_cells(&mut source, 12, 40);
        assert_eq!(source.scroll, 8);

        for live_before_first_frame in [false, true] {
            let mut warm = Frontend {
                granted: true,
                render_cursor: Some(None),
                ..Frontend::default()
            };
            let mut configuration = bootstrap_configuration_with_view(
                source.snapshot.as_ref().unwrap().clone(),
                info.clone(),
                native::SidebarView {
                    collapsed: source.collapsed.clone(),
                    scroll: source.scroll,
                },
            );
            warm.extract_sidebar_bootstrap(&mut configuration);
            assert!(warm.apply_sidebar_bootstrap());
            if live_before_first_frame {
                assert!(warm.accept_snapshot(source.snapshot.as_ref().unwrap().clone()));
            }
            assert_eq!(sidebar_cells(&mut warm, 12, 40), frame);
            assert_eq!(warm.collapsed, source.collapsed);
            assert_eq!(warm.scroll, source.scroll);
            assert!(!warm.bootstrap_view_pending);

            let mut live = source.snapshot.as_ref().unwrap().clone();
            live.revision += 1;
            live.focused_pane_id = Some("terminal_12".to_owned());
            assert!(warm.accept_snapshot(live));
            assert_eq!(warm.cursor, Some(Item::Pane("terminal_12".to_owned())));
            assert!(warm.scroll > source.scroll);
        }
    }

    fn populated_frontend() -> Frontend {
        let mut frontend = popup_frontend();
        let mut state = snapshot(2, "terminal_1");
        let workspace = state.workspaces[0].clone();
        let pane = state.panes[0].clone();
        state.workspaces = (1..=12)
            .map(|id| Workspace {
                id: format!("workspace_{id}"),
                name: format!("Workspace {id}"),
                active_tab_id: format!("tab_{id}"),
                ..workspace.clone()
            })
            .collect();
        state.panes = (1..=12)
            .map(|id| Pane {
                id: format!("terminal_{id}"),
                terminal_id: format!("terminal_{id}"),
                workspace_id: format!("workspace_{id}"),
                tab_id: format!("tab_{id}"),
                name: format!("Terminal {id}"),
                ..pane.clone()
            })
            .collect();
        frontend.accept_snapshot(state);
        frontend
    }

    #[test]
    fn native_modes_and_remaps_repaint_without_claiming_sidebar_control() {
        let mut frontend = popup_frontend();
        frontend.synchronize_focus();
        let focus = frontend.native_focus;
        let cursor = frontend.cursor.clone();
        assert!(frontend.mode.is_none());
        assert!(frontend.update(keybinds_event(&guide_mode(InputMode::Locked))));
        for mode in [
            InputMode::Locked,
            InputMode::Normal,
            InputMode::Resize,
            InputMode::Pane,
        ] {
            let info = guide_mode(mode);
            assert!(frontend.update(mode_event(info.clone())));
            assert!(!frontend.update(mode_event(info)));
            let mut state = frontend.snapshot.clone().unwrap();
            state.revision += 1;
            state.panes[0].status = Status::Working;
            frontend.accept_snapshot(state);
            assert_eq!(frontend.native_focus, focus);
            assert_eq!(frontend.cursor, cursor);
            assert!(!frontend.keyboard_control);
            assert!(!frontend.control_has_focus);
        }
        let mut remapped = guide_mode(InputMode::Pane);
        remapped.keybinds[0].1[0].0.bare_key = BareKey::Char('u');
        assert!(frontend.update(keybinds_event(&remapped)));
        remapped.session_name = Some("new session metadata".to_owned());
        assert!(!frontend.update(mode_event(remapped)));
    }

    #[test]
    fn lossless_keybind_events_preserve_pipe_bindings_and_ignore_partial_sdk_updates() {
        let mut frontend = popup_frontend();
        let pending = sidebar_cells(&mut frontend, 24, 64)
            .into_iter()
            .flatten()
            .collect::<String>();
        assert!(pending.contains("Waiting for native keymap"));
        assert!(!frontend.update(Event::ModeUpdate(guide_mode(InputMode::Locked))));
        assert!(frontend.mode.is_none());

        let mut info = guide_mode(InputMode::Locked);
        info.keybinds[0].1.push((
            "Ctrl Space".parse().unwrap(),
            vec![actions::Action::KeybindPipe {
                name: Some(CONTROL_PIPE.to_owned()),
                payload: Some("toggle".to_owned()),
                args: None,
                plugin: None,
                plugin_id: None,
                configuration: None,
                launch_new: false,
                skip_cache: false,
                floating: None,
                in_place: None,
                cwd: None,
                pane_title: None,
            }],
        ));
        assert!(frontend.update(mode_event(info.clone())));
        assert!(frontend.update(keybinds_event(&info)));
        assert_eq!(frontend.keybinds.as_ref().unwrap(), &info.keybinds);
        let frame = sidebar_cells(&mut frontend, 24, 64);
        let text = frame.iter().flatten().collect::<String>();
        assert!(text.contains("Ctrl+Space"));
        assert!(text.contains("Ctrl+g"));
        assert!(text.contains("sidebar"));

        let mut stripped = guide_mode(InputMode::Normal);
        stripped.keybinds.clear();
        assert!(!frontend.update(Event::ModeUpdate(stripped)));
        assert_eq!(frontend.mode.as_ref().unwrap().mode, InputMode::Locked);
        assert_eq!(frontend.keybinds.as_ref().unwrap(), &info.keybinds);
        assert!(!frontend.keyguide_dirty);
        assert_eq!(sidebar_cells(&mut frontend, 24, 64), frame);

        info.keybinds[0].1[1].0 = "Ctrl x".parse().unwrap();
        assert!(frontend.update(keybinds_event(&info)));
        let remapped = sidebar_cells(&mut frontend, 24, 64)
            .into_iter()
            .flatten()
            .collect::<String>();
        assert!(remapped.contains("Ctrl+x"));
        assert!(!remapped.contains("Ctrl+Space"));
        assert!(!frontend.update(keybinds_event(&info)));
    }

    #[test]
    fn initial_keymap_and_mode_can_arrive_in_either_order_and_empty_remaps_are_authoritative() {
        for keys_first in [false, true] {
            let mut frontend = popup_frontend();
            let cursor = frontend.cursor.clone();
            let info = guide_mode(InputMode::Locked);
            let (first, second) = if keys_first {
                (keybinds_event(&info), mode_event(info.clone()))
            } else {
                (mode_event(info.clone()), keybinds_event(&info))
            };
            assert!(frontend.update(first));
            assert!(frontend.update(second));
            let complete = sidebar_cells(&mut frontend, 24, 64);
            assert!(
                complete
                    .iter()
                    .flatten()
                    .collect::<String>()
                    .contains("Ctrl+g")
            );
            assert!(frontend.update(mode_event(guide_mode(InputMode::Normal))));
            assert_eq!(frontend.keybinds.as_ref().unwrap(), &info.keybinds);
            assert!(frontend.update(mode_event(info.clone())));
            assert_eq!(sidebar_cells(&mut frontend, 24, 64), complete);
            assert!(!frontend.update(Event::InitialKeybinds(Vec::new())));
            assert_eq!(frontend.keybinds.as_ref().unwrap(), &info.keybinds);
            let mut empty = info;
            empty.keybinds.clear();
            assert!(frontend.update(keybinds_event(&empty)));
            let remapped = sidebar_cells(&mut frontend, 24, 64);
            assert!(
                !remapped
                    .iter()
                    .flatten()
                    .collect::<String>()
                    .contains("Ctrl+g")
            );
            assert_eq!(frontend.cursor, cursor);
            assert!(!frontend.keyboard_control);
        }
    }

    #[test]
    fn optimistic_guide_mode_does_not_acknowledge_host_mode_or_sidebar_focus() {
        let mut frontend = popup_frontend();
        frontend.update(keybinds_event(&guide_mode(InputMode::Locked)));
        frontend.update(mode_event(guide_mode(InputMode::Locked)));
        frontend.synchronize_focus();
        frontend.enter_control();
        assert_eq!(frontend.mode.as_ref().unwrap().mode, InputMode::Locked);
        assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
        assert!(!frontend.control_has_focus);
        let immediate = sidebar_cells(&mut frontend, 20, 40);
        assert!(
            immediate
                .iter()
                .flatten()
                .collect::<String>()
                .contains("Unlocked · Sidebar")
        );
        assert!(!frontend.update(mode_event(guide_mode(InputMode::Locked))));
        host_focus(&mut frontend, PaneId::Terminal(1));
        assert!(frontend.keyboard_control);
        assert!(!frontend.control_has_focus);
        assert_eq!(sidebar_cells(&mut frontend, 20, 40), immediate);
        assert!(frontend.update(mode_event(guide_mode(InputMode::Normal))));
        assert!(frontend.mode_intent.is_none());
        assert!(!frontend.control_has_focus);
        host_focus(&mut frontend, PaneId::Plugin(99));
        assert!(frontend.control_has_focus);
        assert!(frontend.update(mode_event(guide_mode(InputMode::Locked))));
        assert!(!frontend.keyboard_control);
    }

    #[test]
    fn invalid_or_unrelated_mode_messages_preserve_the_last_complete_keymap() {
        let mut frontend = popup_frontend();
        assert!(!frontend.update(Event::CustomMessage("mode_info".to_owned(), "{".to_owned(),)));
        assert!(frontend.mode.is_none());
        let info = guide_mode(InputMode::Locked);
        assert!(frontend.update(mode_event(info.clone())));
        assert!(frontend.update(keybinds_event(&info)));
        let frame = sidebar_cells(&mut frontend, 24, 64);
        let mut invalid_mode = serde_json::to_value(&info).unwrap();
        invalid_mode["mode"] = serde_json::json!("unsupported-mode");
        let mut oversized = guide_mode(InputMode::Normal);
        oversized.session_name = Some("x".repeat(MAX_MESSAGE_BYTES));
        for event in [
            Event::CustomMessage(
                "unrelated".to_owned(),
                serde_json::to_string(&guide_mode(InputMode::Normal)).unwrap(),
            ),
            Event::CustomMessage("mode_info".to_owned(), "{".to_owned()),
            Event::CustomMessage("mode_info".to_owned(), "null".to_owned()),
            Event::CustomMessage("mode_info".to_owned(), invalid_mode.to_string()),
            mode_event(oversized),
            Event::CustomMessage("keybinds_info".to_owned(), "{".to_owned()),
            Event::CustomMessage("keybinds_info".to_owned(), "null".to_owned()),
            Event::CustomMessage(
                "keybinds_info".to_owned(),
                "[[\"Locked\",[]],[]]".to_owned(),
            ),
            Event::CustomMessage(
                "keybinds_info".to_owned(),
                " ".repeat(MAX_MESSAGE_BYTES + 1),
            ),
        ] {
            assert!(!frontend.update(event));
            assert_eq!(frontend.mode.as_ref().unwrap().mode, info.mode);
            assert_eq!(frontend.mode.as_ref().unwrap().base_mode, info.base_mode);
            assert_eq!(frontend.keybinds.as_ref().unwrap(), &info.keybinds);
            assert!(!frontend.keyguide_dirty);
            assert_eq!(sidebar_cells(&mut frontend, 24, 64), frame);
        }
    }

    #[test]
    fn terminal_palette_changes_preserve_layout_selection_and_native_keys() {
        let mut frontend = popup_frontend();
        let info = guide_mode(InputMode::Locked);
        frontend.update(mode_event(info.clone()));
        frontend.update(keybinds_event(&info));
        let original = sidebar_cells(&mut frontend, 24, 64);
        let selection = frontend.cursor.clone();
        let colors = palette::TerminalColors {
            truecolor: true,
            foreground: Some([32, 32, 32]),
            background: Some([248, 248, 248]),
            ansi: [None; 16],
            ..Default::default()
        };
        let event = Event::CustomMessage(
            "bkpr_terminal_colors".to_owned(),
            serde_json::to_string(&colors).unwrap(),
        );
        assert!(frontend.update(event));
        assert_eq!(sidebar_cells(&mut frontend, 24, 64), original);
        assert_eq!(frontend.cursor, selection);
        assert_eq!(frontend.keybinds.as_ref(), Some(&info.keybinds));
        for payload in ["{", "null", r#"{"truecolor":true,"foreground":[256,0,0]}"#] {
            assert!(!frontend.update(Event::CustomMessage(
                "bkpr_terminal_colors".to_owned(),
                payload.to_owned(),
            )));
            assert_eq!(sidebar_cells(&mut frontend, 24, 64), original);
        }
    }

    #[test]
    fn locked_mode_still_leaves_controls_but_keeps_a_modal_active() {
        for modal in [false, true] {
            let mut frontend = popup_frontend();
            frontend.keyboard_control = true;
            frontend.control_has_focus = true;
            frontend.local_selection = true;
            if modal {
                frontend.modal = Some(Modal::Close {
                    item: Item::Pane("terminal_1".to_owned()),
                    name: "name".to_owned(),
                });
            }
            assert!(frontend.update(mode_event(guide_mode(InputMode::Locked))));
            assert_eq!(frontend.keyboard_control, modal);
            assert_eq!(frontend.control_has_focus, modal);
            assert_eq!(frontend.modal.is_some(), modal);
        }
    }

    #[test]
    fn changing_guide_height_keeps_keyboard_selection_visible() {
        let mut frontend = populated_frontend();
        // Exercise the inline guide: this display has room for details, but
        // not for a wider shortcut card beyond the sidebar.
        frontend.tabs[0].display_area_columns = 48;
        frontend.update(mode_event(guide_mode(InputMode::Locked)));
        frontend.update(keybinds_event(&guide_mode(InputMode::Locked)));
        sidebar_cells(&mut frontend, 16, 24);
        let application_height = frontend.tree_height();
        frontend.keyboard_control = true;
        frontend.control_has_focus = true;
        assert!(frontend.update(mode_event(guide_mode(InputMode::Normal))));
        let sidebar_height = frontend.tree_height();
        assert!(sidebar_height < application_height);

        frontend.cursor = frontend.items.last().cloned();
        frontend.ensure_cursor_visible();
        frontend.update(mode_event(guide_mode(InputMode::Resize)));
        let selected = frontend
            .tree_cache
            .iter()
            .position(|row| row.item == frontend.cursor)
            .unwrap();
        assert!(selected >= frontend.scroll);
        assert!(selected < frontend.scroll + frontend.tree_height());
        assert!(
            frontend.scroll
                <= frontend
                    .tree_cache
                    .len()
                    .saturating_sub(frontend.tree_height())
        );
    }

    #[test]
    fn guide_rows_never_activate_hidden_entries_and_page_scroll_uses_the_tree() {
        let mut frontend = populated_frontend();
        frontend.tabs[0].display_area_columns = 48;
        frontend.keyboard_control = true;
        frontend.control_has_focus = true;
        frontend.update(mode_event(guide_mode(InputMode::Normal)));
        frontend.update(keybinds_event(&guide_mode(InputMode::Normal)));
        let baseline = sidebar_cells(&mut frontend, 12, 24);
        let item = frontend.tree_cache[..frontend.tree_height()]
            .iter()
            .rev()
            .find_map(|row| row.item.clone())
            .unwrap();
        frontend.popup_item = Some(item);
        frontend.refresh_tooltip();
        assert!(frontend.tooltip.pending.as_ref().unwrap().visible);
        let with_details = sidebar_cells(&mut frontend, 12, 24);
        assert_eq!(
            with_details[frontend.tree_end()..],
            baseline[frontend.tree_end()..]
        );
        frontend.hide_tooltip();
        frontend.granted = false; // Any accidental activation remains observable in the queue.
        let cursor = frontend.cursor.clone();
        let mut covered_entry = false;
        for row in frontend.tree_end()..frontend.rows {
            covered_entry |= ui::item_at(&frontend.tree_cache, frontend.scroll + row - 2).is_some();
            frontend.mouse(Mouse::Hover(row as isize, 4));
            assert!(frontend.hover.is_none());
            frontend.mouse(Mouse::LeftClick(row as isize, 4));
            frontend.mouse(Mouse::RightClick(row as isize, 4));
            assert_eq!(frontend.cursor, cursor);
            assert!(frontend.queue.is_empty());
            assert!(frontend.popup_item.is_none());
        }
        assert!(covered_entry);
        let before = frontend
            .items
            .iter()
            .position(|item| Some(item) == frontend.cursor.as_ref())
            .unwrap();
        let page = (frontend.tree_height() / 2).max(1);
        frontend.key(KeyWithModifier::new(BareKey::PageDown));
        assert_eq!(frontend.cursor, Some(frontend.items[before + page].clone()));
        frontend.mouse(Mouse::ScrollDown(usize::MAX));
        assert_eq!(
            frontend.scroll,
            frontend
                .tree_cache
                .len()
                .saturating_sub(frontend.tree_height())
        );
        frontend.mouse(Mouse::ScrollUp(usize::MAX));
        assert_eq!(frontend.scroll, 0);
    }

    #[test]
    fn tiny_sidebar_keeps_content_and_close_modal_above_the_guide() {
        for rows in 0..=12 {
            for columns in [0, 1, 2, 3, 24] {
                let mut frontend = frontend();
                frontend.render_cursor = Some(None);
                frontend.keyboard_control = true;
                frontend.control_has_focus = true;
                frontend.modal = Some(Modal::Close {
                    item: Item::Pane("terminal_1".to_owned()),
                    name: "Terminal 1".to_owned(),
                });
                frontend.update(mode_event(guide_mode(InputMode::Locked)));
                frontend.update(keybinds_event(&guide_mode(InputMode::Locked)));
                sidebar_cells(&mut frontend, rows, columns);
                assert!(frontend.tree_end() <= rows);
                if rows > 2 {
                    assert!(frontend.tree_height() > 0);
                }
                assert_eq!(frontend.render_cursor, Some(None));
            }
        }
    }

    fn ci_refresh_frontend() -> (Frontend, overlay::RefreshRequest) {
        use bkpr::ci_contract::{
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
            local_head_sha: "fedcba9876543210".to_owned(),
            checked_ref: Some("origin/main".to_owned()),
            status: CiStatus::Passed,
            fetch_state: CiFetchState::Ready,
            checks: Vec::new(),
            change_request: None,
            updated_at: 0,
            stale: false,
            error: None,
            truncated: false,
            commit_missing: false,
        });
        hover_item(&mut frontend, &Item::Pane("terminal_1".to_owned()));
        frontend.refresh_tooltip();
        frontend.tooltip.id = Some(100);
        let state = frontend.tooltip.pending.as_ref().unwrap();
        let request = overlay::RefreshRequest {
            client_id: state.client_id,
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
                        .fetch_state = bkpr::ci_contract::CiFetchState::Loading
                }
                14 => frontend.granted = false,
                15 => {
                    frontend.modal = Some(Modal::Close {
                        item: Item::Pane("terminal_1".to_owned()),
                        name: "Terminal".to_owned(),
                    })
                }
                16 => request.client_id = frontend.client_id + 1,
                _ => unreachable!(),
            }
            assert!(
                frontend.accept_ci_refresh(&source, &request).is_none(),
                "case {case}"
            );
        }
        let (mut frontend, request) = ci_refresh_frontend();
        assert_ne!(
            request.target.head_sha,
            frontend.snapshot.as_ref().unwrap().panes[0]
                .ci
                .as_ref()
                .unwrap()
                .local_head_sha
        );
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
    fn helper_readiness_cannot_bind_a_different_clients_replica() {
        let mut frontend = popup_frontend();
        hover_item(&mut frontend, &Item::Pane("terminal_1".to_owned()));
        let identity = frontend.tooltip.pending.as_ref().unwrap().identity();
        frontend.pipe(native_message(
            overlay::READY,
            overlay::Ready {
                id: 100,
                client_id: frontend.client_id + 1,
                instance: identity.instance.clone(),
                key: identity.key.clone(),
                revision: identity.revision,
            },
        ));
        assert!(frontend.tooltip.id.is_none());
        frontend.pipe(native_message(
            overlay::READY,
            overlay::Ready {
                id: 100,
                client_id: frontend.client_id,
                instance: identity.instance,
                key: identity.key,
                revision: identity.revision,
            },
        ));
        assert_eq!(frontend.tooltip.id, Some(100));
    }

    #[test]
    fn popup_faults_require_the_owning_helper_and_surface_outside_the_card() {
        let mut frontend = popup_frontend();
        hover_item(&mut frontend, &Item::Pane("terminal_1".to_owned()));
        let instance = frontend.tooltip.pending.as_ref().unwrap().instance.clone();
        let fault = overlay::FaultNotice {
            id: 100,
            client_id: frontend.client_id,
            instance,
            message: "Permission denied".to_owned(),
        };
        let mut foreign = fault.clone();
        foreign.client_id += 1;
        assert!(!frontend.pipe(native_message(overlay::ERROR, foreign)));
        assert!(frontend.error.is_none());
        let mut forged = native_message(overlay::ERROR, fault.clone());
        forged.source = PipeSource::Plugin(101);
        assert!(!frontend.pipe(forged));
        assert!(frontend.error.is_none());
        assert!(frontend.pipe(native_message(overlay::ERROR, fault)));
        assert_eq!(
            frontend.error.as_deref(),
            Some("Popup helper: Permission denied")
        );
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
            .fetch_state = bkpr::ci_contract::CiFetchState::Loading;
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
            .fetch_state = bkpr::ci_contract::CiFetchState::Ready;
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

    fn hover_item(frontend: &mut Frontend, item: &Item) {
        let row = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(item))
            .unwrap();
        frontend.mouse(Mouse::Hover((row + 2 - frontend.scroll) as isize, 5));
    }

    fn pointer_notice(frontend: &Frontend, inside: bool) -> overlay::PointerNotice {
        let state = frontend.tooltip.pending.as_ref().unwrap();
        overlay::PointerNotice {
            client_id: state.client_id,
            instance: state.instance.clone(),
            key: state.content.key.clone(),
            revision: state.revision,
            inside,
        }
    }

    fn sidebar_focus(frontend: &mut Frontend, focused: bool) {
        if focused {
            frontend.keyboard_control = true;
            frontend.local_selection = true;
        }
        let mut manifest = frontend.manifest.clone();
        for pane in manifest.panes.get_mut(&0).unwrap() {
            pane.is_focused = if pane.is_plugin { focused } else { !focused };
        }
        frontend.pane_update(manifest);
    }

    #[test]
    fn hover_opens_without_focus_and_single_click_keeps_its_target() {
        let mut frontend = popup_frontend();
        let item = Item::Pane("terminal_2".to_owned());
        let row = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap()
            + 2;
        let cursor = frontend.cursor.clone();
        frontend.mouse(Mouse::RightClick(row as isize, 5));
        assert!(frontend.popup_item.is_none());
        hover_item(&mut frontend, &item);
        assert_popup(&frontend, &item);
        assert_eq!(frontend.cursor, cursor);
        assert!(!frontend.local_selection);
        assert!(!frontend.control_active());
        assert!(frontend.queue.is_empty());
        assert!(frontend.in_flight.is_none());
        assert!(!frontend.mouse(Mouse::RightClick(2, 5)));
        assert_popup(&frontend, &item);
        frontend.granted = false; // Queue selection without invoking the host transport.
        click_item(&mut frontend, item.clone());
        assert_eq!(frontend.popup_item, Some(item.clone()));
        assert_eq!(frontend.cursor, Some(item));
        assert!(frontend.desired_selection.is_some());
    }

    #[test]
    fn focus_and_hover_follow_the_latest_pointer_or_keyboard_highlight() {
        let mut frontend = popup_frontend();
        frontend.update(mode_event(guide_mode(InputMode::Normal)));
        assert!(frontend.popup_item.is_none()); // Input mode alone is not sidebar focus.
        sidebar_focus(&mut frontend, true);
        let initial = frontend.cursor.clone().unwrap();
        assert_popup(&frontend, &initial);
        let pointer = Item::Pane("terminal_2".to_owned());
        hover_item(&mut frontend, &pointer);
        assert_popup(&frontend, &pointer);
        frontend.key(KeyWithModifier::new(BareKey::Home));
        let keyboard = frontend.cursor.clone().unwrap();
        assert_ne!(keyboard, pointer);
        assert!(frontend.hover.is_none());
        assert_eq!(frontend.pointer_item, Some(pointer.clone()));
        assert_popup(&frontend, &keyboard);
        frontend.accept_snapshot(snapshot(2, "terminal_1"));
        assert_popup(&frontend, &keyboard);
        sidebar_focus(&mut frontend, false);
        assert!(frontend.popup_item.is_none()); // Keyboard selection invalidates stale hover.
        hover_item(&mut frontend, &pointer);
        assert_popup(&frontend, &pointer);
        frontend.mouse(Mouse::Hover(0, 5));
        assert!(frontend.popup_item.is_none());
        sidebar_focus(&mut frontend, true);
        assert!(frontend.popup_item.is_some());
        frontend.mouse(Mouse::Hover(0, 5));
        assert!(frontend.popup_item.is_some());
        sidebar_focus(&mut frontend, false);
        assert!(frontend.popup_item.is_none());
    }

    #[test]
    fn sidebar_leave_probes_popup_and_true_leave_closes_without_a_timer() {
        let mut frontend = popup_frontend();
        let item = Item::Pane("terminal_1".to_owned());
        hover_item(&mut frontend, &item);
        frontend.tooltip.id = Some(100);
        // The helper's new-target hover can arrive before the sidebar leave.
        let entered = pointer_notice(&frontend, true);
        assert!(frontend.accept_pointer(&PipeSource::Plugin(100), &entered));
        assert!(frontend.hover.is_none());
        assert!(frontend.pointer_item.is_none());
        frontend.mouse(Mouse::Hover(0, u16::MAX as usize));
        assert_popup(&frontend, &item);
        let entered = pointer_notice(&frontend, true);
        frontend.accept_pointer(&PipeSource::Plugin(100), &entered);
        assert!(!frontend.popup_probe);
        assert_popup(&frontend, &item);
        let left = pointer_notice(&frontend, false);
        assert!(frontend.accept_pointer(&PipeSource::Plugin(100), &left));
        assert!(frontend.popup_item.is_none());
        assert!(!frontend.tooltip.pending.as_ref().unwrap().visible);
    }

    #[test]
    fn popup_exit_dismisses_even_if_native_sidebar_leave_is_missing() {
        let mut frontend = popup_frontend();
        let item = Item::Pane("terminal_1".to_owned());
        hover_item(&mut frontend, &item);
        frontend.tooltip.id = Some(100);
        let entered = pointer_notice(&frontend, true);
        frontend.accept_pointer(&PipeSource::Plugin(100), &entered);
        assert!(frontend.hover.is_none());
        let left = pointer_notice(&frontend, false);
        frontend.accept_pointer(&PipeSource::Plugin(100), &left);
        assert!(frontend.popup_item.is_none());
        assert!(!frontend.tooltip.pending.as_ref().unwrap().visible);
    }

    #[test]
    fn popup_leave_never_overrides_sidebar_focus_or_actual_item_hover() {
        let mut frontend = popup_frontend();
        let item = Item::Pane("terminal_1".to_owned());
        hover_item(&mut frontend, &item);
        frontend.tooltip.id = Some(100);
        let left = pointer_notice(&frontend, false);
        frontend.accept_pointer(&PipeSource::Plugin(100), &left);
        assert_popup(&frontend, &item);
        sidebar_focus(&mut frontend, true);
        frontend.mouse(Mouse::Hover(0, u16::MAX as usize));
        let left = pointer_notice(&frontend, false);
        frontend.accept_pointer(&PipeSource::Plugin(100), &left);
        assert_popup(&frontend, &item);
        sidebar_focus(&mut frontend, false);
        assert!(frontend.popup_probe);
        let left = pointer_notice(&frontend, false);
        frontend.accept_pointer(&PipeSource::Plugin(100), &left);
        assert!(frontend.popup_item.is_none());
    }

    #[test]
    fn pointer_replies_reject_stale_frames_and_reprobe_after_async_content_changes() {
        for case in 0..7 {
            let (mut frontend, _) = ci_refresh_frontend();
            let mut notice = pointer_notice(&frontend, false);
            let mut source = PipeSource::Plugin(100);
            match case {
                0 => source = PipeSource::Plugin(101),
                1 => notice.instance = "old".to_owned(),
                2 => notice.key = "pane:terminal_2".to_owned(),
                3 => notice.revision = notice.revision.saturating_sub(1),
                4 => source = PipeSource::Cli("forged".to_owned()),
                5 => notice.revision += 1,
                6 => notice.client_id = frontend.client_id + 1,
                _ => unreachable!(),
            }
            assert!(!frontend.accept_pointer(&source, &notice));
            assert!(frontend.popup_item.is_some());
        }
        let (mut frontend, _) = ci_refresh_frontend();
        frontend.mouse(Mouse::Hover(0, u16::MAX as usize));
        let old = pointer_notice(&frontend, false);
        assert_eq!(frontend.popup_probe_revision, Some(old.revision));
        frontend.snapshot.as_mut().unwrap().panes[0].description = "metadata arrived".to_owned();
        frontend.snapshot.as_mut().unwrap().revision += 1;
        frontend.refresh_tooltip();
        let current = pointer_notice(&frontend, true);
        assert_ne!(current.revision, old.revision);
        assert_eq!(frontend.popup_probe_revision, Some(current.revision));
        assert!(!frontend.accept_pointer(&PipeSource::Plugin(100), &old));
        frontend.accept_pointer(&PipeSource::Plugin(100), &current);
        assert_popup(&frontend, &Item::Pane("terminal_1".to_owned()));
        hover_item(&mut frontend, &Item::Pane("terminal_2".to_owned()));
        assert!(!frontend.accept_pointer(&PipeSource::Plugin(100), &current));
        assert_popup(&frontend, &Item::Pane("terminal_2".to_owned()));
    }

    #[test]
    fn cached_hover_targets_refresh_only_after_their_async_snapshot_changes() {
        let mut frontend = popup_frontend();
        let first = Item::Pane("terminal_1".to_owned());
        let second = Item::Pane("terminal_2".to_owned());
        hover_item(&mut frontend, &first);
        let original = frontend.tooltip.pending.as_ref().unwrap().content.clone();
        hover_item(&mut frontend, &second);
        hover_item(&mut frontend, &first);
        assert_eq!(frontend.tooltip.pending.as_ref().unwrap().content, original);
        let revision = frontend.tooltip.pending.as_ref().unwrap().revision;
        hover_item(&mut frontend, &first);
        assert_eq!(
            frontend.tooltip.pending.as_ref().unwrap().revision,
            revision
        );
        let mut snapshot = frontend.snapshot.clone().unwrap();
        snapshot.revision += 1;
        snapshot.panes[0].description = "async original target update 日本語".to_owned();
        frontend.accept_snapshot(snapshot);
        let updated = frontend.tooltip.pending.as_ref().unwrap().content.clone();
        assert_ne!(updated, original);
        hover_item(&mut frontend, &second);
        assert_ne!(frontend.tooltip.pending.as_ref().unwrap().content, updated);
        hover_item(&mut frontend, &first);
        assert_eq!(frontend.tooltip.pending.as_ref().unwrap().content, updated);
    }

    #[test]
    fn outer_sidebar_gutter_retains_hover_but_never_activates_an_item() {
        let mut frontend = popup_frontend();
        frontend.manifest.panes.get_mut(&0).unwrap()[0].pane_columns = frontend.columns + 1;
        let item = Item::Pane("terminal_1".to_owned());
        hover_item(&mut frontend, &item);
        let (row, _) = frontend.pointer_position.unwrap();
        frontend.mouse(Mouse::Hover(row, frontend.columns));
        assert_eq!(frontend.hover, Some(item.clone()));
        assert_popup(&frontend, &item);
        let cursor = frontend.cursor.clone();
        assert!(!frontend.mouse(Mouse::LeftClick(row, frontend.columns)));
        assert_eq!(frontend.cursor, cursor);
        assert!(frontend.queue.is_empty());
        frontend.mouse(Mouse::Hover(row, frontend.columns + 1));
        assert!(frontend.popup_item.is_none());
    }

    #[test]
    fn automatic_popup_never_changes_sidebar_or_neighboring_row_glyphs() {
        let mut frontend = popup_frontend();
        let mut state = snapshot(2, "terminal_1");
        state.panes[1].workspace_id = "workspace_1".to_owned();
        state.panes[0].branch = Some("feature/main".to_owned());
        frontend.accept_snapshot(state);
        let baseline = sidebar_cells(&mut frontend, 20, 40);
        hover_item(&mut frontend, &Item::Pane("terminal_1".to_owned()));
        assert!(frontend.tooltip.pending.as_ref().unwrap().visible);
        assert_eq!(sidebar_cells(&mut frontend, 20, 40), baseline);
    }

    #[test]
    fn modal_suppresses_automatic_details_and_restores_only_real_focus() {
        let mut frontend = popup_frontend();
        sidebar_focus(&mut frontend, true);
        hover_item(&mut frontend, &Item::Pane("terminal_2".to_owned()));
        frontend.key(KeyWithModifier::new(BareKey::Char('x')));
        assert!(frontend.modal.is_some());
        assert!(frontend.popup_item.is_none());
        frontend.mouse(Mouse::Hover(2, 5));
        assert!(frontend.pointer_item.is_none());
        assert!(frontend.control_active());
        frontend.key(KeyWithModifier::new(BareKey::Esc));
        assert!(frontend.modal.is_none());
        assert_popup(&frontend, frontend.cursor.as_ref().unwrap());
        sidebar_focus(&mut frontend, false);
        assert!(frontend.popup_item.is_none());
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
                            'K' => {
                                assert_eq!(parameters, "0");
                                assert!(row < rows && column <= columns);
                                cells[row][column..].fill(' ');
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
    fn sidebar_header_centers_the_branded_session_inside_the_content_width() {
        let mut frontend = frontend();
        let title = "\u{f14e2} bkpr · test";
        for columns in [23, 40] {
            let cells = sidebar_cells(&mut frontend, 8, columns);
            let line: String = cells[0].iter().collect();
            assert_eq!(line.trim(), title);
            let left = line
                .chars()
                .take_while(|character| *character == ' ')
                .count();
            let right = line
                .chars()
                .rev()
                .take_while(|character| *character == ' ')
                .count();
            assert_eq!(left, (columns - ui::width(title)) / 2);
            assert!(right == left || right == left + 1);
        }
        for columns in 1..=8 {
            let cells = sidebar_cells(&mut frontend, 8, columns);
            assert_eq!(cells[0].len(), columns);
        }
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
    fn expanded_sidebar_guide_uses_only_space_beyond_the_sidebar() {
        let mut frontend = popup_frontend();
        frontend.mode_intent = Some(InputMode::Normal);
        assert_eq!(frontend.guide_space().unwrap().x, 0);
        frontend.keyboard_control = true;
        let space = frontend.guide_space().unwrap();
        assert_eq!(space.x, 40);
        assert_eq!(space.max_columns, 80);
    }

    #[test]
    fn overflowing_sidebar_guide_stays_adjacent_when_display_grows() {
        let mut frontend = popup_frontend();
        frontend.columns = 16;
        frontend.keyboard_control = true;
        frontend.mode_intent = Some(InputMode::Normal);
        frontend.keybinds = Some(Vec::new());
        frontend.keyguide_dirty = true;
        frontend.refresh_keyguide();
        let guide = frontend.keyguide.as_ref().unwrap();
        assert!(guide.expanded_columns > frontend.columns);
        let anchor = frontend.guide_space().unwrap().x;
        assert_eq!(guide.expanded_x, anchor);
        frontend.tabs[0].display_area_columns *= 2;
        frontend.keyguide_dirty = true;
        frontend.refresh_keyguide();
        assert_eq!(frontend.keyguide.as_ref().unwrap().expanded_x, anchor);
    }

    #[test]
    fn details_respect_anticipated_wide_guide_without_changing_the_tree_viewport() {
        let mut frontend = popup_frontend();
        frontend.keyguide = Some(CachedKeyGuide {
            context: keyguide::Context::Application,
            columns: 40,
            max_rows: 16,
            lines: Vec::new(),
            space: Some(GuideSpace {
                x: 0,
                bottom: 20,
                max_columns: 120,
            }),
            overlay_ready: true,
            expanded_x: 0,
            expanded_columns: 64,
            expanded_rows: 4,
            reserved_rows: 4,
        });
        let tree_height = frontend.tree_height();
        let coordinates = FloatingPaneCoordinates::default()
            .with_x_fixed(41)
            .with_y_fixed(8)
            .with_width_fixed(60)
            .with_height_fixed(24);
        let placement = Frontend::constrain_tooltip_to_guide(
            frontend.keyguide.as_ref(),
            overlay::Placement {
                coordinates: coordinates.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            placement.coordinates,
            coordinates.clone().with_y_fixed(0).with_height_fixed(16)
        );
        assert_eq!(frontend.tree_height(), tree_height);
        let separate = coordinates.clone().with_x_fixed(70);
        assert_eq!(
            Frontend::constrain_tooltip_to_guide(
                frontend.keyguide.as_ref(),
                overlay::Placement {
                    coordinates: separate.clone()
                },
            )
            .unwrap()
            .coordinates,
            separate
        );
        frontend.keyguide.as_mut().unwrap().expanded_rows = 18;
        assert_eq!(
            Frontend::constrain_tooltip_to_guide(
                frontend.keyguide.as_ref(),
                overlay::Placement {
                    coordinates: coordinates.clone()
                },
            )
            .unwrap()
            .coordinates,
            coordinates.clone().with_y_fixed(0).with_height_fixed(2)
        );
        frontend.keyguide.as_mut().unwrap().overlay_ready = false;
        assert_eq!(
            Frontend::constrain_tooltip_to_guide(
                frontend.keyguide.as_ref(),
                overlay::Placement {
                    coordinates: coordinates.clone()
                },
            )
            .unwrap()
            .coordinates,
            coordinates.clone().with_y_fixed(0).with_height_fixed(2)
        );
        frontend.keyguide.as_mut().unwrap().expanded_x = 105;
        assert_eq!(
            Frontend::constrain_tooltip_to_guide(
                frontend.keyguide.as_ref(),
                overlay::Placement {
                    coordinates: coordinates.clone()
                },
            )
            .unwrap()
            .coordinates,
            coordinates
        );
    }

    #[test]
    fn geometry_updates_reposition_an_open_popup_without_moving_selection() {
        let mut frontend = popup_frontend();
        frontend.synchronize_focus();
        let item = Item::Workspace("workspace_1".to_owned());
        hover_item(&mut frontend, &item);
        let focus = frontend.native_focus;
        let cursor = frontend.cursor.clone();
        // The host can render the new size before publishing its updated pane manifest.
        sidebar_cells(&mut frontend, 20, 60);
        assert_eq!(frontend.columns, 40);
        let mut manifest = frontend.manifest.clone();
        manifest.panes.get_mut(&0).unwrap()[0].pane_content_columns = 60;
        assert!(frontend.update(Event::PaneUpdate(manifest)));
        sidebar_cells(&mut frontend, 20, 60);
        assert_eq!(
            frontend.tooltip.pending.as_ref().unwrap().coordinates.x,
            FloatingPaneCoordinates::default().with_x_fixed(60).x
        );
        assert_eq!(frontend.native_focus, focus);
        assert_eq!(frontend.cursor, cursor);
        assert_eq!(frontend.popup_item, Some(item));
        let mut tabs = frontend.tabs.clone();
        tabs[0].display_area_columns = 90;
        assert!(frontend.update(Event::TabUpdate(tabs)));
        assert_eq!(
            frontend.tooltip.pending.as_ref().unwrap().coordinates.width,
            FloatingPaneCoordinates::default()
                .with_width_fixed(30)
                .width
        );
    }

    #[test]
    fn pointer_motion_and_clamped_scroll_only_repaint_visible_changes() {
        let mut frontend = popup_frontend();
        sidebar_cells(&mut frontend, 20, 40);
        let item = Item::Pane("terminal_1".to_owned());
        let row = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap()
            + 2;
        assert!(frontend.update(Event::Mouse(Mouse::Hover(row as isize, 4))));
        assert!(!frontend.update(Event::Mouse(Mouse::Hover(row as isize, 5))));
        assert!(!frontend.update(Event::Mouse(Mouse::ScrollUp(1))));
        assert!(!frontend.update(Event::Mouse(Mouse::RightClick(row as isize, 5))));
        let revision = frontend.tooltip.pending.as_ref().unwrap().revision;
        assert!(!frontend.update(Event::Mouse(Mouse::Hover(row as isize, 6))));
        assert_eq!(
            frontend.tooltip.pending.as_ref().unwrap().revision,
            revision
        );
        assert_popup(&frontend, &item);
        assert!(frontend.update(Event::Mouse(Mouse::Hover(0, 5))));
    }

    #[test]
    fn scrolling_rehits_the_actual_pointer_row_and_clamped_scroll_keeps_its_frame() {
        let mut frontend = populated_frontend();
        sidebar_cells(&mut frontend, 12, 24);
        let first = frontend.items.first().cloned().unwrap();
        hover_item(&mut frontend, &first);
        let (line, column) = frontend.pointer_position.unwrap();
        assert!(frontend.mouse(Mouse::ScrollDown(usize::MAX)));
        let actual = frontend.pointer_item_at(line, column);
        assert_eq!(frontend.pointer_item, actual);
        assert_eq!(frontend.hover, actual);
        if let Some(item) = actual {
            assert_ne!(item, first);
            assert_popup(&frontend, &item);
        }
        let revision = frontend
            .tooltip
            .pending
            .as_ref()
            .map(|state| state.revision);
        assert!(!frontend.mouse(Mouse::ScrollDown(1)));
        assert_eq!(
            frontend
                .tooltip
                .pending
                .as_ref()
                .map(|state| state.revision),
            revision
        );
        frontend.mouse(Mouse::Hover(0, u16::MAX as usize));
        assert!(frontend.popup_item.is_none());
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
    fn pointer_mapping_excludes_headers_gaps_and_footer() {
        let mut frontend = frontend();
        let cursor = frontend.cursor.clone();
        frontend.tree_cache = ui::tree(
            frontend.snapshot.as_ref().unwrap(),
            &BTreeSet::from(["workspace_1".to_owned()]),
        );
        for id in ["workspace_1", "workspace_2"] {
            let workspace = Item::Workspace(id.to_owned());
            let start = *ui::item_band(&frontend.tree_cache, &workspace)
                .unwrap()
                .start();
            frontend.mouse(Mouse::Hover((start - frontend.scroll + 2) as isize, 0));
            assert_eq!(frontend.hover, Some(workspace));
        }
        let gaps: Vec<_> = frontend
            .tree_cache
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.item.is_none().then_some(index + 2))
            .collect();
        for line in (0..2).chain(gaps).chain(frontend.tree_end()..frontend.rows) {
            frontend.mouse(Mouse::Hover(line as isize, 0));
            assert_eq!(frontend.hover, None);
            frontend.mouse(Mouse::LeftClick(line as isize, 0));
            assert_eq!(frontend.cursor, cursor);
        }
    }

    #[test]
    fn passive_floating_popup_does_not_mask_focused_terminal() {
        let mut frontend = popup_frontend();
        frontend.tabs[0].are_floating_panes_visible = true;
        frontend.manifest.panes.get_mut(&0).unwrap().push(PaneInfo {
            id: 200,
            is_plugin: true,
            is_floating: true,
            is_focused: true,
            is_selectable: false,
            ..PaneInfo::default()
        });
        frontend.synchronize_focus();
        assert_eq!(frontend.native_focus, Some(1));
        assert!(!frontend.control_has_focus);
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

    fn take_queued_request(frontend: &mut Frontend) {
        let queued = frontend.queue.pop_front().unwrap();
        frontend.in_flight = Some(queued.request);
        frontend.in_flight_return_mode = queued.return_mode;
        frontend.in_flight_view = queued.view;
    }

    #[test]
    fn bootstrap_does_not_authorize_an_ack_without_live_state() {
        let mut frontend = Frontend {
            granted: true,
            ..Frontend::default()
        };
        let mut configuration =
            bootstrap_configuration(snapshot(1, "terminal_1"), guide_mode(InputMode::Locked));
        frontend.extract_sidebar_bootstrap(&mut configuration);
        assert!(frontend.apply_sidebar_bootstrap());
        frontend.in_flight = Some(Request::new(7, Action::Snapshot));
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: 7,
                state: None,
                effects: vec![Effect::FocusPane { zellij_pane_id: 1 }],
                error: None,
            },
        });
        assert_eq!(
            frontend.error.as_deref(),
            Some("Native owner response arrived without valid state")
        );
        assert!(!frontend.response_state_valid);
        assert!(frontend.mode_intent.is_none());
        assert!(frontend.in_flight.is_none());
    }

    #[test]
    fn live_state_replaces_stale_bootstrap_then_enforces_same_instance_conflicts() {
        let mut frontend = Frontend {
            granted: true,
            native_owner_id: Some(100),
            ..Frontend::default()
        };
        let mut stale = snapshot(99, "terminal_2");
        stale.workspaces[0].name = "Stale projection".to_owned();
        let mut configuration = bootstrap_configuration(stale, guide_mode(InputMode::Locked));
        frontend.extract_sidebar_bootstrap(&mut configuration);
        assert!(frontend.apply_sidebar_bootstrap());
        frontend.in_flight = Some(Request::new(7, Action::Snapshot));
        let live = snapshot(1, "terminal_1");
        assert!(frontend.pipe(native_message(
            native::STATE,
            native::NativeState {
                client_id: frontend.client_id,
                snapshot: live.clone(),
                ui_selection: None,
                ui_selection_revision: 0,
                ui_selection_epoch: String::new(),
            },
        )));
        assert_eq!(frontend.snapshot.as_ref(), Some(&live));
        assert!(!frontend.snapshot_is_bootstrap);
        assert!(frontend.response_state_valid);
        assert_eq!(frontend.in_flight.as_ref().unwrap().request_id, 7);
        assert!(frontend.pipe(native_message(
            native::RESPONSE,
            native::NativeResponse {
                client: frontend.native_client.clone(),
                client_id: frontend.client_id,
                response: Response {
                    protocol: PROTOCOL_VERSION,
                    request_id: 7,
                    state: None,
                    effects: Vec::new(),
                    error: None,
                },
            },
        )));
        assert!(frontend.in_flight.is_none());
        assert!(frontend.error.is_none());
        assert!(!frontend.accept_snapshot(snapshot(1, "terminal_2")));
        assert_eq!(
            frontend.error.as_deref(),
            Some("Conflicting state at the same native owner revision")
        );
        assert_eq!(frontend.snapshot.as_ref(), Some(&live));
        assert!(!frontend.response_state_valid);
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
                            state: None,
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
    fn no_state_acknowledgement_requires_a_valid_publication_before_applying_effects() {
        for invalid in [false, true] {
            let mut frontend = popup_frontend();
            frontend.keyboard_control = true;
            frontend.control_has_focus = true;
            frontend.local_selection = true;
            frontend.in_flight = Some(Request::new(7, Action::Snapshot));
            frontend.response_state_valid = false;
            if invalid {
                assert!(!frontend.accept_snapshot(snapshot(1, "terminal_2")));
            }
            let expected = if invalid {
                "Conflicting state at the same native owner revision"
            } else {
                "Native owner response arrived without valid state"
            };
            frontend.native_response(native::NativeResponse {
                client: frontend.native_client.clone(),
                client_id: frontend.client_id,
                response: Response {
                    protocol: PROTOCOL_VERSION,
                    request_id: 7,
                    state: None,
                    effects: vec![Effect::FocusPane { zellij_pane_id: 1 }],
                    error: None,
                },
            });
            assert_eq!(frontend.error.as_deref(), Some(expected));
            assert!(frontend.keyboard_control);
            assert!(frontend.control_has_focus);
            assert!(frontend.mode_intent.is_none());
            assert!(frontend.in_flight.is_none());
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
            Some("Conflicting state at the same native owner revision"),
        );
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
    }

    #[test]
    fn explicit_control_survives_focus_ack_then_releases_on_departure() {
        let mut frontend = frontend();
        host_focus(&mut frontend, PaneId::Terminal(1));
        frontend.enter_control();
        assert!(matches!(
            frontend.keyguide_context(),
            keyguide::Context::Sidebar { .. }
        ));
        host_focus(&mut frontend, PaneId::Terminal(1));
        assert!(frontend.keyboard_control);
        assert!(!frontend.control_has_focus);
        assert!(matches!(
            frontend.keyguide_context(),
            keyguide::Context::Sidebar { .. }
        ));

        host_focus(&mut frontend, PaneId::Plugin(99));
        frontend.key(KeyWithModifier::new(BareKey::Home));
        frontend.accept_snapshot(snapshot(2, "terminal_1"));
        assert!(frontend.keyboard_control);
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_1".to_owned()))
        );

        host_focus(&mut frontend, PaneId::Terminal(2));
        frontend.accept_snapshot(snapshot(3, "terminal_2"));
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
        source.accept_snapshot(state.clone());
        destination.accept_snapshot(state);
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

        frontend.accept_snapshot(snapshot(2, "terminal_1"));
        host_focus(&mut frontend, PaneId::Plugin(99));
        assert_eq!(
            frontend.cursor,
            Some(Item::Workspace("workspace_1".to_owned()))
        );

        host_focus(&mut frontend, PaneId::Terminal(2));
        frontend.accept_snapshot(snapshot(3, "terminal_2"));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_2".to_owned())));
    }

    #[test]
    fn activation_keeps_optimistic_selection_until_already_broadcast_response() {
        let mut frontend = frontend();
        frontend.render_cursor = Some(None);
        let mut initial = Vec::new();
        frontend.render_sidebar_to(20, 40, &mut initial);
        let item = Item::Pane("terminal_2".to_owned());
        let line = frontend
            .tree_cache
            .iter()
            .position(|row| row.item.as_ref() == Some(&item))
            .unwrap()
            + 2;
        frontend.mouse(Mouse::LeftClick(line as isize, 5));
        assert_eq!(frontend.cursor, Some(item.clone()));
        frontend.hover = None;
        let mut immediate = Vec::new();
        frontend.render_sidebar_to(20, 40, &mut immediate);
        assert_ne!(immediate, initial);
        assert_eq!(
            frontend
                .snapshot
                .as_ref()
                .unwrap()
                .focused_pane_id
                .as_deref(),
            Some("terminal_1"),
        );
        host_focus(&mut frontend, PaneId::Terminal(1));
        assert_eq!(frontend.cursor, Some(item.clone()));
        frontend.accept_snapshot(snapshot(2, "terminal_1"));
        let mut stale = Vec::new();
        frontend.render_sidebar_to(20, 40, &mut stale);
        assert_eq!(stale, immediate);

        take_queued_request(&mut frontend);
        frontend.accept_snapshot(snapshot(3, "terminal_2"));
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: 1,
                state: None,
                effects: Vec::new(),
                error: None,
            },
        });
        assert!(!frontend.local_selection);
        assert_eq!(frontend.cursor, Some(item));
        frontend.accept_snapshot(snapshot(4, "terminal_1"));
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
        take_queued_request(&mut frontend);
        click_item(&mut frontend, latest.clone());
        assert_eq!(frontend.cursor, Some(latest.clone()));
        acknowledge(&mut frontend, snapshot(2, "terminal_2"), None);
        host_focus(&mut frontend, PaneId::Terminal(2));
        assert_eq!(frontend.cursor, Some(latest.clone()));
        assert_eq!(frontend.desired_selection.as_ref().unwrap().request_id, 2);
        assert!(!frontend.desired_selection.as_ref().unwrap().acknowledged);
        take_queued_request(&mut frontend);
        acknowledge(&mut frontend, snapshot(2, "terminal_2"), None);
        assert_eq!(frontend.cursor, Some(latest.clone()));
        assert!(frontend.desired_selection.as_ref().unwrap().acknowledged);
        assert!(frontend.pending_user_action.is_some());
        frontend.accept_snapshot(snapshot(3, "terminal_1"));
        assert_eq!(frontend.cursor, Some(latest));
        assert!(frontend.desired_selection.is_none());
        assert!(frontend.pending_user_action.is_none());
    }

    #[test]
    fn repeated_target_clicks_still_require_the_latest_request_identity() {
        let mut frontend = frontend();
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        take_queued_request(&mut frontend);
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
        take_queued_request(&mut failed);
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
        take_queued_request(&mut removed);
        let mut state = snapshot(2, "terminal_1");
        state.panes.retain(|pane| pane.id != "terminal_2");
        removed.accept_snapshot(state.clone());
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
        destination.accept_snapshot(snapshot(2, "terminal_1"));
        assert_eq!(
            destination.cursor,
            Some(Item::Pane("terminal_2".to_owned()))
        );
        assert!(!destination.accept_selection_relay("owner".to_owned(), 1, None));
        assert_eq!(
            destination.cursor,
            Some(Item::Pane("terminal_2".to_owned()))
        );
        destination.accept_snapshot(snapshot(3, "terminal_2"));
        assert!(destination.relayed_selection.is_none());
        assert!(destination.pending_user_action.is_none());
    }

    #[test]
    fn local_latest_selection_wins_over_older_relay_and_epoch_tombstone() {
        let mut frontend = frontend();
        frontend.native_client = "source".to_owned();
        click_item(&mut frontend, Item::Pane("terminal_2".to_owned()));
        take_queued_request(&mut frontend);
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
        frontend.modal = Some(Modal::Close {
            item: Item::Pane("terminal_1".to_owned()),
            name: "Terminal 1".to_owned(),
        });
        frontend.accept_snapshot(snapshot(2, "terminal_2"));
        host_focus(&mut frontend, PaneId::Terminal(2));
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        frontend.modal = None;
        frontend.fold("workspace_2".to_owned(), Some(true));
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
                    ui::RowStyle {
                        selected: true,
                        hovered: false,
                        selected_group: true,
                        clock_frame: 0,
                    },
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
        state.panes[0].attention = Some(bkpr::Attention::Error);
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
                            ui::RowStyle {
                                selected,
                                hovered,
                                selected_group: selected,
                                clock_frame: 0,
                            },
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
            assert!(frontend.accept_snapshot(state));
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

    #[test]
    fn shifted_terminal_shortcut_keeps_the_existing_workspace() {
        for key in [
            KeyWithModifier::new(BareKey::Char('N')),
            KeyWithModifier {
                bare_key: BareKey::Char('n'),
                key_modifiers: [KeyModifier::Shift].into_iter().collect(),
            },
        ] {
            let mut frontend = popup_frontend();
            let pane = &frontend.snapshot.as_ref().unwrap().panes[0];
            let workspace = pane.workspace_id.clone();
            frontend.cursor = Some(Item::Pane(pane.id.clone()));
            frontend.keyboard_control = true;
            frontend.key(key);
            assert!(matches!(
                frontend.focused.kind(),
                Some(focused::Kind::Pane { workspace_id }) if workspace_id == &workspace
            ));
        }
        let mut frontend = popup_frontend();
        frontend.keyboard_control = true;
        frontend.key(KeyWithModifier::new(BareKey::Char('n')));
        assert!(matches!(
            frontend.focused.kind(),
            Some(focused::Kind::Workspace)
        ));
    }

    #[test]
    fn rename_captures_selected_identity_and_name_and_restores_on_cancel() {
        for item in [
            Item::Pane("terminal_1".to_owned()),
            Item::Workspace("workspace_1".to_owned()),
        ] {
            let mut frontend = popup_frontend();
            frontend.keyboard_control = true;
            frontend.local_selection = true;
            frontend.control_terminal = Some(1);
            frontend.cursor = Some(item.clone());
            frontend.mode = Some(ModeState {
                mode: InputMode::Normal,
                base_mode: Some(InputMode::Normal),
            });
            let state = frontend.snapshot.as_ref().unwrap();
            let expected_name = match &item {
                Item::Pane(id) => state
                    .panes
                    .iter()
                    .find(|pane| &pane.id == id)
                    .unwrap()
                    .name
                    .clone(),
                Item::Workspace(id) => state
                    .workspaces
                    .iter()
                    .find(|workspace| &workspace.id == id)
                    .unwrap()
                    .name
                    .clone(),
            };
            frontend.edit_selected(false);
            assert!(frontend.modal.is_none());
            assert!(matches!(
                frontend.focused.kind(),
                Some(focused::Kind::Rename { item: target, name })
                    if target == &item && name == &expected_name
            ));
            assert_eq!(
                frontend.keyguide_context(),
                keyguide::Context::Edit {
                    multiple_fields: false
                }
            );
            frontend.cursor = Some(Item::Pane("terminal_2".to_owned()));
            assert!(matches!(
                frontend.focused.kind(),
                Some(focused::Kind::Rename { item: target, name })
                    if target == &item && name == &expected_name
            ));
            frontend.key(KeyWithModifier::new(BareKey::Esc));
            assert!(!frontend.focused.is_open());
            assert!(frontend.keyboard_control);
            assert_eq!(frontend.cursor, Some(item));
            assert_eq!(frontend.control_terminal, Some(1));
            assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
        }
    }

    #[test]
    fn successful_rename_restores_sidebar_selection_and_mode() {
        let mut frontend = popup_frontend();
        frontend.keyboard_control = true;
        frontend.local_selection = true;
        frontend.control_terminal = Some(1);
        frontend.cursor = Some(Item::Pane("terminal_1".to_owned()));
        frontend.mode = Some(ModeState {
            mode: InputMode::Normal,
            base_mode: Some(InputMode::Normal),
        });
        frontend.edit_selected(false);
        frontend.focused.submission = Some(10);
        frontend.cursor = Some(Item::Pane("terminal_2".to_owned()));
        frontend.finish_focused_submission(10, None);
        assert!(!frontend.focused.is_open());
        assert!(frontend.keyboard_control);
        assert_eq!(frontend.cursor, Some(Item::Pane("terminal_1".to_owned())));
        assert_eq!(frontend.control_terminal, Some(1));
        assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
    }

    #[test]
    fn focused_creation_cancel_restores_exact_sidebar_selection_and_mode() {
        for kind in [
            focused::Kind::Workspace,
            focused::Kind::Pane {
                workspace_id: "workspace_1".to_owned(),
            },
        ] {
            let mut frontend = popup_frontend();
            frontend.keyboard_control = true;
            frontend.local_selection = true;
            frontend.control_terminal = Some(1);
            frontend.cursor = Some(Item::Pane("terminal_2".to_owned()));
            frontend.mode = Some(ModeState {
                mode: InputMode::Normal,
                base_mode: Some(InputMode::Normal),
            });
            let cursor = frontend.cursor.clone();
            frontend.open_focused(kind);
            assert!(frontend.modal.is_none());
            assert!(frontend.focused.is_open());
            assert_eq!(frontend.guide_mode(), Some(InputMode::Locked));
            frontend.key(KeyWithModifier::new(BareKey::Esc));
            assert!(!frontend.focused.is_open());
            assert!(frontend.keyboard_control);
            assert_eq!(frontend.control_terminal, Some(1));
            assert_eq!(frontend.cursor, cursor);
            assert_eq!(frontend.guide_mode(), Some(InputMode::Normal));
        }
    }

    #[test]
    fn stale_creation_result_cannot_close_a_new_focused_form() {
        let mut frontend = popup_frontend();
        frontend.open_focused(focused::Kind::Workspace);
        frontend.focused.submission = Some(10);
        frontend.close_focused(true);
        frontend.open_focused(focused::Kind::Pane {
            workspace_id: "workspace_1".to_owned(),
        });
        frontend.finish_focused_submission(10, None);
        assert!(matches!(
            frontend.focused.kind(),
            Some(focused::Kind::Pane { .. })
        ));
        frontend.focused.submission = Some(11);
        frontend.finish_focused_submission(11, None);
        assert!(!frontend.focused.is_open());
        assert_eq!(frontend.guide_mode(), Some(InputMode::Locked));
    }

    #[test]
    fn cancelled_focused_creation_never_dispatches_after_an_older_request_completes() {
        let mut frontend = popup_frontend();
        frontend.keyboard_control = true;
        frontend.open_focused(focused::Kind::Workspace);
        frontend.request_id = 17;
        frontend.in_flight = Some(Request::new(17, Action::Snapshot));
        frontend.focused.submission = Some(18);
        frontend.enqueue(Action::CreateWorkspace {
            label: Some("cancelled".to_owned()),
            cwd: None,
        });
        frontend.close_focused(true);
        assert!(
            frontend.queue.is_empty(),
            "cancelled creation remained dispatchable"
        );
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: 17,
                state: Some(snapshot(3, "terminal_1")),
                effects: vec![],
                error: None,
            },
        });
        assert!(frontend.in_flight.is_none());
        assert!(!frontend.user_action_pending());
        assert!(frontend.focused.submission.is_none());
    }

    #[test]
    fn escape_cannot_abandon_a_dispatched_creation_or_its_failure() {
        let mut frontend = popup_frontend();
        frontend.open_focused(focused::Kind::Workspace);
        frontend.focused.submission = Some(18);
        frontend.in_flight = Some(Request::new(
            18,
            Action::CreateWorkspace {
                label: Some("submitted".to_owned()),
                cwd: None,
            },
        ));
        frontend.cancel_focused(true);
        assert!(frontend.focused.is_open());
        assert_eq!(frontend.focused.submission, Some(18));
        frontend.native_response(native::NativeResponse {
            client: frontend.native_client.clone(),
            client_id: frontend.client_id,
            response: Response {
                protocol: PROTOCOL_VERSION,
                request_id: 18,
                state: Some(snapshot(3, "terminal_1")),
                effects: vec![],
                error: Some("creation failed".to_owned()),
            },
        });
        assert!(
            frontend.focused.is_open(),
            "failure must remain available for retry"
        );
        assert!(frontend.focused.submission.is_none());
        assert_eq!(frontend.error.as_deref(), Some("creation failed"));
        frontend.cancel_focused(true);
        assert!(!frontend.focused.is_open());
    }
}
