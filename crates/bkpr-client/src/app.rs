use crate::transport::{Endpoint, RegisteredClient};
use crate::tree::{Node as TreeNode, Owner as TreeOwner, Target as TreeTarget, Tree};
use crate::{
    controller::*,
    transport,
    updates::{self, InputPump, Pump, Snapshot},
};
use anyhow::{Context, Result, bail};
use bkpr_protocol::*;
use bkpr_tui_framework::overlay::{
    OverlayStack, Placement as OverlayPlacement, PointerPhase, PointerRoute,
    Policy as OverlayPolicy,
};
use bkpr_tui_framework::{ClientEvent, *};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    os::fd::AsRawFd,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use termwiz::input::{InputEvent, InputParser, Modifiers, MouseButtons};

fn material_caption(frame: &Frame, rect: Rect, bottom: bool) -> Rect {
    let y = rect.y
        + if bottom {
            rect.height.saturating_sub(1)
        } else {
            0
        };
    let width = if rect.width >= 3 && usize::from(y) < frame.rows.len() {
        frame.rows[usize::from(y)].cells
            [usize::from(rect.x + 1)..usize::from(rect.x + rect.width - 1)]
            .iter()
            .rposition(|cell| !matches!(cell.text.as_str(), " " | "─"))
            .map_or(0, |index| index as u16 + 1)
    } else {
        0
    };
    Rect {
        x: rect.x + 1,
        y,
        width,
        height: 1,
    }
}

fn material_border(frame: &mut Frame, rect: Rect, visible: bool, captions: &[Rect]) {
    if rect.width < 3 || rect.height < 3 {
        return;
    }
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;
    let mut paint = |x: u16, y: u16, glyph: &str| {
        if captions
            .iter()
            .any(|caption| caption.y == y && x >= caption.x && x < caption.x + caption.width)
        {
            return;
        }
        let cell = &mut frame.rows[usize::from(y)].cells[usize::from(x)];
        cell.text.clear();
        cell.width = 1;
        cell.hyperlink = None;
        cell.text.push_str(if visible { glyph } else { " " });
    };
    for x in rect.x..=right {
        paint(
            x,
            rect.y,
            if x == rect.x {
                "╭"
            } else if x == right {
                "╮"
            } else {
                "─"
            },
        );
        paint(
            x,
            bottom,
            if x == rect.x {
                "╰"
            } else if x == right {
                "╯"
            } else {
                "─"
            },
        );
    }
    for y in rect.y + 1..bottom {
        paint(rect.x, y, "│");
        paint(right, y, "│");
    }
}

fn raised_material(
    graphics: &crate::chrome_graphics::ChromeGraphics,
    frame: &mut Frame,
    effects: &mut Frame,
    outer: Rect,
    base: u32,
) -> bool {
    let local = Rect {
        x: 0,
        y: 0,
        width: frame.size.cols,
        height: frame.size.rows,
    };
    let captions = [
        material_caption(frame, local, false),
        material_caption(frame, local, true),
    ];
    let body = window::content(frame.size);
    let exclusions = [body].map(|rect| Rect {
        x: outer.x + rect.x,
        y: outer.y + rect.y,
        ..rect
    });
    let ready = local.width >= 3
        && local.height >= 3
        && graphics.decorate_window_in_clipped(effects, outer, true, base, &exclusions);
    material_border(frame, local, !ready, &captions);
    ready
}

#[path = "jobs.rs"]
mod jobs;
#[path = "management.rs"]
mod management;
#[path = "resizing.rs"]
mod resizing;
#[path = "scrollback.rs"]
mod scrollback;
#[path = "search.rs"]
mod search;
#[path = "selection.rs"]
mod selection;
#[path = "sessions.rs"]
mod sessions;
#[path = "theme_switcher.rs"]
mod theme_switcher;
use sessions::{Coordinator, SessionAction};

enum Background {
    Management(management::Completion),
    Resize(management::Completion),
    CiRefresh {
        owner: TreeOwner,
        target: bkpr_protocol::CiRefreshTarget,
        result: Result<()>,
    },
    Attached {
        pane: String,
        attached: Box<Attached>,
    },
}
struct ManagementIntent {
    epoch: u64,
    selection: Option<management::Selection>,
}
enum RetirementPolicy {
    Visible,
    Waiting(updates::Receipt),
    Revoked,
    Failed,
}
struct RetiringCandidate {
    pane: String,
    attached: Attached,
}
struct PointerOrigin {
    pane: String,
    view: String,
}
#[derive(Clone)]
enum PointerCapture {
    Ui,
    Resize,
    SidebarResize { size: Size, offset: i32 },
    Pane(Arc<PointerOrigin>),
    Selection(Arc<PointerOrigin>),
}
struct PressedPointer {
    origin: Arc<PointerOrigin>,
    event: MouseEvent,
    admitted: bool,
    release: Option<Operation>,
}
impl PressedPointer {
    fn release_operation(&self) -> Operation {
        let mut event = self.event;
        event.action = MouseAction::Release;
        Operation::Pane {
            pane_id: self.origin.pane.clone(),
            request: PaneRequest::Mouse {
                view_id: self.origin.view.clone(),
                event,
            },
        }
    }
}

/// Physical-client input, takeover and clipboard-consent preferences.
#[derive(Clone)]
pub struct Options {
    /// Disable pane input and management mutations without changing sizing eligibility.
    pub read_only: bool,
    /// Request takeover when each session attachment activates.
    pub detach_others: bool,
    /// Clipboard consent mode: `"ask"`, `"allow"` or `"deny"`.
    pub clipboard_policy: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            read_only: false,
            detach_others: false,
            clipboard_policy: "ask".into(),
        }
    }
}

struct Attached {
    view: String,
    spec: ViewSpec,
    pump: Pump<Snapshot>,
    metadata: PaneSnapshot,
    clipboard_id: Option<String>,
    frame: Option<Frame>,
    history: bool,
    revision: u64,
    images: HashMap<ImageKey, Arc<Image>>,
    seeded: u64,
    retirement_requested: bool,
    retirement_policy: RetirementPolicy,
    retirement_focus_out: bool,
    retirement_error: Option<anyhow::Error>,
}
impl Attached {
    fn retire(
        &mut self,
        input: &InputPump,
        pane: &str,
        focus_out: bool,
        wait_policy: bool,
        drain_ready: bool,
    ) -> Result<Option<updates::Receipt>> {
        self.retirement_requested = true;
        self.pump.cancel();
        self.retirement_focus_out |= focus_out;
        if matches!(self.retirement_policy, RetirementPolicy::Visible) {
            let mut hidden = self.spec.clone();
            hidden.visible = false;
            hidden.interactive = false;
            hidden.clipboard = false;
            // Previously accepted input still drains under its original grant.
            self.retirement_policy =
                RetirementPolicy::Waiting(input.policy(pane, &self.view, hidden)?);
        }
        if let RetirementPolicy::Waiting(receipt) = &self.retirement_policy {
            match receipt.try_recv() {
                Ok(Ok(())) => self.retirement_policy = RetirementPolicy::Revoked,
                Ok(Err(error)) => {
                    self.retirement_error = Some(error);
                    self.retirement_policy = RetirementPolicy::Failed;
                }
                Err(mpsc::TryRecvError::Empty) if wait_policy => return Ok(None),
                Err(mpsc::TryRecvError::Empty) => {
                    // Shutdown is outside the UI loop. The independent policy
                    // lane must settle before command-lane Detach can revoke it.
                    match receipt.recv() {
                        Ok(Ok(())) => self.retirement_policy = RetirementPolicy::Revoked,
                        Ok(Err(error)) => {
                            self.retirement_error = Some(error);
                            self.retirement_policy = RetirementPolicy::Failed;
                        }
                        Err(_) => {
                            self.retirement_error = Some(transport::mark_outcome_unknown(
                                anyhow::anyhow!(
                                    "pane {pane} view {} policy completion disconnected; outcome unknown; not replayed",
                                    self.view
                                ),
                            ));
                            self.retirement_policy = RetirementPolicy::Failed;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.retirement_error = Some(transport::mark_outcome_unknown(anyhow::anyhow!(
                        "pane {pane} view {} policy completion disconnected; outcome unknown; not replayed",
                        self.view
                    )));
                    self.retirement_policy = RetirementPolicy::Failed;
                }
            }
        }
        if !drain_ready {
            return Ok(None);
        }
        self.pump.cancel();
        input
            .retire(pane, &self.view, self.retirement_focus_out)
            .map(Some)
    }
    fn scene(&mut self, update: scene_wire::DecodedScene) -> Result<()> {
        let retained = self
            .metadata
            .image_refs
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let incoming = update.image_keys().collect::<HashSet<_>>();
        anyhow::ensure!(
            retained
                .iter()
                .all(|key| self.images.contains_key(key) || incoming.contains(key)),
            "missing normalized image generation"
        );
        update.apply(&mut self.revision, &mut self.frame)?;
        let frame = self
            .frame
            .as_mut()
            .expect("a decoded scene supplies a frame");
        if frame.alternate_screen || frame.scrollback_offset == 0 {
            self.history = false;
        }
        for image in frame.images.drain(..) {
            self.images.insert(image.key(), image);
        }
        let resources = &self.metadata.image_refs;
        self.images.retain(|key, _| retained.contains(key));
        frame.images = resources
            .iter()
            .map(|key| {
                self.images
                    .get(key)
                    .cloned()
                    .expect("resource preflight succeeded")
            })
            .collect();
        Ok(())
    }
    fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        let previous_metadata = std::mem::replace(&mut self.metadata, snapshot.metadata);
        if let Some(scene) = snapshot.scene
            && let Err(error) = self.scene(scene)
        {
            self.metadata = previous_metadata;
            return Err(error);
        }
        self.clipboard_id = self
            .metadata
            .clipboard
            .as_ref()
            .map(|p| p.request_id.clone());
        Ok(())
    }
}
struct DeviceConsent {
    pane: String,
    view: String,
    state: Consent,
    reply: Option<Operation>,
    reservation: Option<updates::DeviceReservation>,
}
struct RetiringView {
    attached: Attached,
    receipt: updates::Receipt,
}
struct DeviceResponse {
    pane: String,
    view: String,
    receipt: updates::Receipt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GuideKey {
    context: crate::keyguide::Context,
    cols: u16,
    rows: u16,
    sidebar_columns: u16,
    flags: u16,
    resize_hint: Option<&'static str>,
}
#[derive(Default)]
struct GuideCache {
    key: Option<GuideKey>,
    content: crate::keyguide::Guide,
}
struct OverlayFocus {
    workspace: Option<String>,
    window: Option<String>,
    pane: Option<String>,
    sidebar: bool,
    tree_target: Option<TreeTarget>,
}
struct AppRuntime {
    jobs: jobs::Scope<Background>,
    input: Option<InputPump>,
    tree: Arc<Mutex<Tree>>,
    toasts: Arc<Mutex<toast::ToastStack>>,
}
struct HeldKey {
    pane: String,
    view: String,
    release: KeyEvent,
    pending: bool,
}
struct TitleSweepSpan {
    rect: Rect,
    effect: bkpr_tui_framework::animation::TextSweep,
}

/// Stable, themed title ink is retained only across decorative-only wakeups.
/// Every ordinary presentation rebuild replaces it after selection/hover styling.
struct SidebarTitleAnimation {
    clock: bkpr_tui_framework::animation::AnimationClock,
    spans: Vec<TitleSweepSpan>,
    styles: Vec<Style>,
}
impl SidebarTitleAnimation {
    fn new(now: Instant) -> Self {
        Self {
            clock: bkpr_tui_framework::animation::AnimationClock::new(
                now,
                Duration::from_millis(80),
                Duration::from_millis(1500),
            ),
            spans: Vec::new(),
            styles: Vec::new(),
        }
    }

    fn capture(
        &mut self,
        frame: &bkpr_protocol::Frame,
        titles: &[Rect],
        theme: &Theme,
        truecolor: bool,
        occluded: impl Fn(Rect) -> bool,
        now: Instant,
    ) {
        self.spans.clear();
        self.styles.clear();
        if truecolor && let ColorSource::Rgb(accent) = theme.ink(Role::Warning, false) {
            for &rect in titles {
                if occluded(rect) {
                    continue;
                }
                let Some(row) = frame.rows.get(usize::from(rect.y)) else {
                    continue;
                };
                let left = usize::from(rect.x).min(row.cells.len());
                let right = usize::from(rect.x.saturating_add(rect.width)).min(row.cells.len());
                let Some(first) = row.cells.get(left).filter(|_| left < right) else {
                    continue;
                };
                if !matches!(theme.effective_ink(&first.style), Some(ColorSource::Rgb(_))) {
                    continue;
                }
                self.spans.push(TitleSweepSpan {
                    rect: Rect {
                        width: (right - left) as u16,
                        ..rect
                    },
                    effect: bkpr_tui_framework::animation::TextSweep {
                        highlight: accent,
                        width: 0.4,
                    },
                });
                self.styles
                    .extend(row.cells[left..right].iter().map(|cell| cell.style.clone()));
            }
        }
        self.clock.set_active(!self.spans.is_empty(), now);
        self.clock.advance(now);
    }

    fn restore(&self, frame: &mut bkpr_protocol::Frame) {
        let mut styles = self.styles.iter();
        for span in &self.spans {
            let row = &mut frame.rows[usize::from(span.rect.y)];
            let left = usize::from(span.rect.x);
            let right = left + usize::from(span.rect.width);
            for cell in &mut row.cells[left..right] {
                cell.style
                    .clone_from(styles.next().expect("retained title style"));
            }
        }
    }

    fn apply(&self, frame: &mut bkpr_protocol::Frame) {
        self.restore(frame);
        for span in &self.spans {
            span.effect.apply(frame, span.rect, self.clock.phase());
        }
    }

    fn timeout(&self, deadline: Instant, now: Instant) -> i32 {
        let deadline = self
            .clock
            .next_deadline()
            .map_or(deadline, |next| next.min(deadline));
        let remaining = deadline.saturating_duration_since(now);
        let millis =
            remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        millis.min(i32::MAX as u128) as i32
    }
}

struct App {
    socket: Endpoint,
    options: Options,
    spawn_defaults: Option<SpawnDefaults>,
    session_action: Option<SessionAction>,
    namespace: (u64, u64),
    backgrounded: bool,
    notification_source: Option<String>,
    tree: Arc<Mutex<Tree>>,
    tree_press: Option<TreeTarget>,
    sidebar_target: Option<TreeTarget>,
    sidebar_backend_available: bool,
    sidebar_has_workspace: bool,
    registry: RegistrySnapshot,
    state: Controller,
    views: HashMap<String, Attached>,
    retiring: Vec<RetiringView>,
    retiring_candidates: Vec<RetiringCandidate>,
    jobs: jobs::Scope<Background>,
    management: HashMap<String, ManagementIntent>,
    selection_epoch: u64,
    responses: Vec<DeviceResponse>,
    held: HashMap<String, HeldKey>,
    key_events_report_releases: bool,
    input: InputPump,
    input_failures: HashMap<String, [Option<updates::InputFailure>; 3]>,
    view_failures: HashMap<String, String>,
    consent: Option<DeviceConsent>,
    answered: HashMap<String, String>,
    deferred: RefCell<VecDeque<updates::ReservedInput>>,
    paste_offer: Option<PasteOffer>,
    paste_target: Option<(String, String)>,
    enhanced_supported: Option<bool>,
    colors: ColorProbe,
    color_query_pending: bool,
    last_button: MouseButton,
    pointer_capture: Option<PointerCapture>,
    sidebar_resize_hover: bool,
    pressed_pointers: RefCell<Vec<PressedPointer>>,
    terminal_selection: selection::State,
    resizing: resizing::State,
    search: search::State,
    finder: Option<crate::finder::Finder>,
    theme: crate::themes::ThemeId,
    theme_picker: Option<theme_switcher::Picker>,
    popover: crate::popover::Popover,
    overlays: OverlayStack<OverlayFocus>,
    toasts: Arc<Mutex<toast::ToastStack>>,
    guide: GuideCache,
    chrome_graphics_enabled: bool,
    notified_focus: Option<(String, String)>,
    focus_out_pending: HashSet<(String, String)>,
    lease_pending: HashSet<String>,
    host_focused: bool,
}
impl App {
    fn notify(&mut self, message: String) {
        let message = match self
            .notification_source
            .as_deref()
            .filter(|_| self.backgrounded)
        {
            Some(source) => format!("{source}: {message}"),
            None => message,
        };
        self.toasts
            .lock()
            .unwrap()
            .push(&message, Instant::now(), Duration::from_secs(5));
    }

    fn toast_timeout(&self, timeout: i32, now: Instant) -> i32 {
        let deadline = self.toasts.lock().unwrap().next_deadline();
        deadline.map_or(timeout, |deadline| {
            let remaining = deadline.saturating_duration_since(now);
            let millis =
                remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
            timeout.min(millis.min(i32::MAX as u128) as i32)
        })
    }

    fn screen_geometry(&self, size: Size) -> (Rect, Rect) {
        screen_geometry(size, self.state.sidebar.preferred_columns)
    }
    fn sidebar_resize_blocked(&self) -> bool {
        !self.host_focused
            || self.state.form.is_some()
            || self.picker_open()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
    }
    fn sidebar_resize_handle(&self, size: Size, x: u16, y: u16) -> bool {
        let sidebar = self.screen_geometry(size).0;
        sidebar.width > 0
            && y < size.rows
            && (x == sidebar.width || x.checked_add(1) == Some(sidebar.width))
    }
    fn validate_sidebar_resize(&mut self, size: Size) {
        if matches!(
            self.pointer_capture,
            Some(PointerCapture::SidebarResize { size: original, .. })
                if original != size || self.sidebar_resize_blocked()
        ) {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        if self.sidebar_resize_blocked() {
            self.sidebar_resize_hover = false;
        }
    }
    fn begin_sidebar_resize(&mut self, size: Size, x: u16, y: u16) -> bool {
        if self.sidebar_resize_blocked() || !self.sidebar_resize_handle(size, x, y) {
            return false;
        }
        self.retire_resize("sidebar resized");
        self.clear_tree_pointer();
        self.pointer_capture = Some(PointerCapture::SidebarResize {
            size,
            offset: i32::from(x) - i32::from(self.screen_geometry(size).0.width),
        });
        self.sidebar_resize_hover = true;
        true
    }
    fn resize_overlay_blocked(&self, size: Size, x: u16, y: u16) -> bool {
        let mut blocked = false;
        for layer in self.overlays.layers() {
            if layer.policy.modal {
                return true;
            }
            if layer.policy.pointer && layer.placement.outer.contains(x, y) {
                let rim = !layer.placement.content.contains(x, y)
                    && ((layer.id == crate::popover::SURFACE_ID && x == layer.placement.outer.x)
                        || (layer.id == "ui:guide" && self.sidebar_resize_handle(size, x, y)));
                blocked = !rim;
            }
        }
        blocked
    }

    fn begin_modal(&mut self) {
        self.restore_theme_preview();
        if self.state.form.is_none() && !self.picker_open() {
            self.overlays.remove("ui:modal");
        }
        self.overlays.present(
            "ui:modal",
            OverlayPlacement {
                outer: Rect::default(),
                content: Rect::default(),
            },
            OverlayPolicy::MODAL,
            Some(OverlayFocus {
                workspace: self.state.workspace.clone(),
                window: self.state.window.clone(),
                pane: self.state.focused.clone(),
                sidebar: self.state.sidebar.active,
                tree_target: self.sidebar_target.clone(),
            }),
        );
        self.overlays.remove(crate::popover::SURFACE_ID);
        self.overlays.remove("ui:guide");
    }
    fn cancel_modal(&mut self) {
        // A dispatched or ambiguous operation cannot be unsent. Keep its result
        // correlated rather than claiming that cancellation aborted it.
        if self
            .state
            .form
            .as_ref()
            .is_some_and(|form| form.pending.is_some() || form.ambiguous)
        {
            return;
        }
        self.state.cancel();
        self.restore_theme_preview();
        self.finder = None;
        if let Some(focus) = self.overlays.remove("ui:modal") {
            self.state.workspace = focus.workspace;
            self.state.window = focus.window;
            self.state.focused = focus.pane;
            {
                let mut tree = self.tree.lock().unwrap();
                tree.active = focus.sidebar;
                if let Some(target) = focus.tree_target {
                    tree.restore_focus(target);
                }
            }
        }
        self.state.reconcile(&self.registry);
        self.synchronize_tree_focus();
    }
    fn toggle_chrome(&mut self) {
        self.chrome_graphics_enabled = !self.chrome_graphics_enabled;
        self.notify(
            if self.chrome_graphics_enabled {
                "Fancy chrome requested"
            } else {
                "Normal chrome"
            }
            .into(),
        );
    }
    fn enter_sidebar(&mut self) {
        self.retire_resize("sidebar focused");
        self.selection_focus_lost();
        self.close_search();
        self.tree.lock().unwrap().enter();
        self.synchronize_tree_focus();
    }
    fn tree_owner(&self) -> TreeOwner {
        TreeOwner {
            slot: self.namespace.0.saturating_sub(1) as usize,
            generation: self.namespace.1,
        }
    }
    pub(super) fn synchronize_tree_focus(&mut self) {
        let owner = self.tree_owner();
        let tree = self.tree.lock().unwrap();
        self.sidebar_target = tree.cursor.clone();
        self.state.sidebar.preferred_columns = tree.preferred_columns;
        self.state.sidebar.active = tree.active;
        self.state.sidebar.cursor = tree.cursor.as_ref().and_then(|target| {
            if target.owner != owner {
                return None;
            }
            match &target.node {
                TreeNode::Native(native) => Some(native.clone()),
                TreeNode::Session => None,
            }
        });
        self.state.sidebar.hover = tree.hover.as_ref().and_then(|target| {
            if target.owner != owner {
                return None;
            }
            match &target.node {
                TreeNode::Native(native) => Some(native.clone()),
                TreeNode::Session => None,
            }
        });
        let selected = tree
            .cursor
            .as_ref()
            .and_then(|target| tree.session(target.owner))
            .or_else(|| tree.session(owner));
        self.sidebar_backend_available =
            selected.is_some_and(|session| session.group.status == SessionStatus::Online);
        self.sidebar_has_workspace =
            selected.is_some_and(
                |session| match tree.cursor.as_ref().map(|target| &target.node) {
                    Some(TreeNode::Native(target)) => session
                        .registry
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == target.workspace()),
                    _ => session.workspace.as_ref().is_some_and(|id| {
                        session
                            .registry
                            .workspaces
                            .iter()
                            .any(|workspace| &workspace.id == id)
                    }),
                },
            );
    }
    pub(super) fn leave_sidebar(&mut self) {
        {
            let mut tree = self.tree.lock().unwrap();
            tree.active = false;
            tree.hover(None);
            tree.pointer_row = None;
            tree.follow_active(self.tree_owner());
        }
        self.synchronize_tree_focus();
    }
    pub(super) fn enter_sessions(&mut self) {
        self.enter_sidebar();
        let owner = self.tree_owner();
        self.tree.lock().unwrap().jump_session(owner);
        self.synchronize_tree_focus();
    }
    fn emit_tree_action(&mut self, action: crate::keyguide::Action, size: Size) {
        let target = self.sidebar_target.clone().or_else(|| {
            self.sidebar_backend_available.then(|| TreeTarget {
                owner: self.tree_owner(),
                node: TreeNode::Session,
            })
        });
        if let Some(target) = target {
            self.session_action = Some(SessionAction::Tree {
                target,
                action,
                size,
            });
        }
    }
    fn clear_tree_pointer(&mut self) {
        {
            let mut tree = self.tree.lock().unwrap();
            tree.hover(None);
            tree.pointer_row = None;
        }
        self.synchronize_tree_focus();
    }
    fn defaults(&self) -> Result<&SpawnDefaults> {
        self.spawn_defaults
            .as_ref()
            .context("session has no validated launch defaults")
    }

    fn supports_action(&self, action: crate::keyguide::Action) -> bool {
        use crate::keyguide::Action;
        let backend_available = if self.state.sidebar.active {
            self.sidebar_backend_available
        } else {
            self.spawn_defaults.is_some() && !self.socket.is_revoked()
        };
        if !backend_available {
            return matches!(
                action,
                Action::CommandPrefix
                    | Action::Detach
                    | Action::EnterSessions
                    | Action::ReconnectSession
                    | Action::ToggleSidebar
                    | Action::LeaveSidebar
                    | Action::Navigate(_)
                    | Action::FoldSidebar
                    | Action::UnfoldSidebar
                    | Action::ToggleFoldSidebar
                    | Action::ActivateSelected
                    | Action::OpenThemes
                    | Action::ToggleChrome
                    | Action::Cancel
                    | Action::Submit
                    | Action::Edit(_)
                    | Action::CycleView
                    | Action::CycleField
            );
        }
        if self
            .state
            .form
            .as_ref()
            .is_some_and(|form| form.pending.is_some() || form.ambiguous)
            && !matches!(action, Action::Detach)
        {
            return false;
        }
        match action {
            Action::RefreshCi => {
                !self.options.read_only
                    && self.state.sidebar.active
                    && self.popover.refresh_available(&self.tree.lock().unwrap())
            }
            Action::CommandPrefix => !self.resize_capturing() || self.resize_dragging(),
            Action::Resize { .. } => !self.options.read_only && !self.resize_dragging(),
            Action::CreateTerminal => {
                !self.options.read_only
                    && if self.state.sidebar.active {
                        self.sidebar_has_workspace
                    } else {
                        self.state.current(&self.registry).is_some()
                    }
            }
            Action::CloseSelected => {
                !self.options.read_only
                    && self
                        .sidebar_target
                        .as_ref()
                        .is_some_and(|target| matches!(target.node, TreeNode::Native(_)))
            }
            Action::CreateWorkspace
            | Action::RenameSelected
            | Action::ReopenPane
            | Action::SplitHorizontal
            | Action::SplitVertical
            | Action::RemovePaneFromLayout
            | Action::MovePane(_)
            | Action::StartResize => !self.options.read_only,
            Action::CopySelection => self.options.clipboard_policy != "deny",
            Action::SendPrefix => {
                !self.options.read_only && !self.state.sidebar.active && !self.search_open()
            }
            Action::Submit => self
                .state
                .form
                .as_ref()
                .is_none_or(|form| form.pending.is_none() && !form.ambiguous),
            _ => true,
        }
    }
    fn binding_action(
        &self,
        context: crate::keyguide::Context,
        event: &KeyEvent,
    ) -> Result<Option<crate::keyguide::Action>> {
        let action = crate::keyguide::lookup_with(context, event, |_| true);
        if let Some(action) = action
            && !self.supports_action(action)
        {
            if self.options.read_only {
                bail!(
                    "read-only client: {} is unavailable",
                    action.description(context)
                );
            }
            bail!(
                "{} is unavailable with the current backend/context",
                action.description(context)
            );
        }
        Ok(action)
    }
    fn guide_context(&self) -> crate::keyguide::Context {
        use crate::keyguide::{Context, ContextState, ModalContext};
        let modal = if self
            .consent
            .as_ref()
            .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
        {
            Some(ModalContext::ClipboardConsent)
        } else if let Some(form) = &self.state.form {
            Some(match form.kind {
                FormKind::CreateWorkspace | FormKind::CreateWindow => ModalContext::Create,
                FormKind::RenameWorkspace | FormKind::RenameWindow | FormKind::RenamePane => {
                    ModalContext::Edit
                }
                FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow => {
                    ModalContext::Confirmation
                }
                FormKind::ProjectDetails => ModalContext::Details,
                FormKind::ReopenPane { .. } => ModalContext::Chooser,
            })
        } else if self.finder.is_some() {
            Some(ModalContext::Finder)
        } else if self.theme_picker.is_some() {
            Some(ModalContext::Theme)
        } else {
            None
        };
        if modal.is_none()
            && !self.state.prefix
            && !self.resize_capturing()
            && self.state.sidebar.active
            && self
                .sidebar_target
                .as_ref()
                .is_some_and(|target| matches!(target.node, TreeNode::Session))
        {
            return Context::Sessions;
        }
        ContextState {
            modal,
            resizing: self.resize_capturing() && !self.state.prefix,
            prefix: self.state.prefix,
            searching: self.search_open(),
            sidebar: self
                .state
                .sidebar
                .active
                .then_some(self.sidebar_target.is_some()),
            history: self.focused_history(),
        }
        .context()
    }
    fn refresh_guide(&mut self, size: Size) {
        let context = self.guide_context();
        let flags = u16::from(self.options.read_only)
            | (u16::from(self.options.clipboard_policy == "deny") << 1)
            | (u16::from(self.state.current(&self.registry).is_some()) << 2)
            | (u16::from(
                self.state
                    .form
                    .as_ref()
                    .is_some_and(|form| form.pending.is_some() || form.ambiguous),
            ) << 3)
            | (u16::from(self.focused_history()) << 4)
            | (u16::from(self.search_open()) << 5)
            | (u16::from(self.state.sidebar.active) << 6)
            | (u16::from(self.resize_dragging()) << 7)
            | (u16::from(!self.management.is_empty()) << 8)
            | (u16::from(self.sidebar_backend_available) << 9)
            | (u16::from(self.sidebar_has_workspace) << 10)
            | (u16::from(self.spawn_defaults.is_some() && !self.socket.is_revoked()) << 11)
            | (u16::from(self.supports_action(crate::keyguide::Action::RefreshCi)) << 12);
        let sidebar_columns = self.screen_geometry(size).0.width;
        let key = GuideKey {
            context,
            cols: size.cols,
            rows: size.rows,
            sidebar_columns,
            flags,
            resize_hint: self.resize_hint(),
        };
        if self.guide.key != Some(key) {
            let available = size.rows;
            let tree_rows = available.div_ceil(2).min(4);
            let max_rows = available.saturating_sub(tree_rows).saturating_sub(2) as usize;
            let notice = self.resize_hint().or_else(|| {
                (!self.management.is_empty())
                    .then_some("Operation pending; other panes remain interactive")
            });
            let content = crate::keyguide::render_with(
                context,
                sidebar_columns,
                size.cols,
                max_rows,
                |action| self.supports_action(action),
                notice,
            );
            self.guide = GuideCache {
                key: Some(key),
                content,
            };
        }
    }
    fn guide_geometry(&self, size: Size, modal: bool) -> Option<Rect> {
        if modal
            || !matches!(
                self.guide_context(),
                crate::keyguide::Context::Application
                    | crate::keyguide::Context::CommandPrefix
                    | crate::keyguide::Context::Sidebar { .. }
                    | crate::keyguide::Context::Sessions
                    | crate::keyguide::Context::History
                    | crate::keyguide::Context::Search
                    | crate::keyguide::Context::Resize
            )
            || self.guide.content.expanded.is_empty()
        {
            return None;
        }
        let width = self.guide.content.columns;
        let height = u16::try_from(self.guide.content.expanded.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(size.rows);
        if width < 4 || height < 3 {
            return None;
        }
        let rect = Rect {
            x: self.screen_geometry(size).0.x,
            y: size.rows - height,
            width,
            height,
        };
        Some(rect)
    }
    fn guide_rows(&self, size: Size) -> u16 {
        self.guide_geometry(size, false)
            .map_or(0, |rect| size.rows.saturating_sub(rect.y))
    }

    fn guide_accent(&self) -> Option<u8> {
        match self.guide_context() {
            crate::keyguide::Context::Sidebar { .. } | crate::keyguide::Context::Sessions => {
                Some(4)
            }
            crate::keyguide::Context::CommandPrefix
            | crate::keyguide::Context::Resize
            | crate::keyguide::Context::History
            | crate::keyguide::Context::Search => Some(3),
            _ => None,
        }
    }

    fn local_repeat(&self, event: &KeyEvent) -> bool {
        use crate::keyguide::{self, Action, Context};
        let context = self.guide_context();
        matches!(
            keyguide::lookup_with(context, event, |action| self.supports_action(action)),
            Some(
                Action::Navigate(_) | Action::Edit(_) | Action::History(_) | Action::Resize { .. }
            )
        ) || (matches!(
            context,
            Context::Create | Context::Edit | Context::Finder | Context::Theme
        ) && event.modifiers & 14 == 0
            && event.text.chars().any(|ch| !ch.is_control()))
    }
    fn begin_finder(&mut self) {
        self.begin_modal();
        self.retire_resize("finder opened");
        self.selection_focus_lost();
        self.close_search();
        self.popover.hide();
        self.finder = Some(crate::finder::Finder::new(
            &self.registry,
            self.state.focused.as_deref(),
            |pane| self.views.get(pane).map(|view| &view.metadata),
        ));
    }
    fn finder_intent(&mut self, intent: crate::finder::FinderIntent) -> Result<()> {
        match intent {
            crate::finder::FinderIntent::None => {}
            crate::finder::FinderIntent::Cancel => self.cancel_modal(),
            crate::finder::FinderIntent::Activate(target) => {
                self.finder = None;
                self.overlays.remove("ui:modal");
                if let Some(workspace) = target.workspace_id {
                    self.activate_sidebar(SidebarTarget::Pane {
                        workspace,
                        pane: target.pane_id,
                    })?;
                } else {
                    self.show_existing(target.pane_id)?;
                    self.leave_sidebar();
                }
            }
        }
        Ok(())
    }
    fn modal_geometry(&self, screen: Size) -> (Rect, Rect) {
        let (width, height) = if self.finder.is_some() {
            (
                screen.cols.saturating_mul(4) / 5,
                (screen.rows.saturating_mul(3) / 4).min(36),
            )
        } else if self.theme_picker.is_some() {
            (64, (crate::themes::THEMES.len() as u16).saturating_add(6))
        } else {
            match self.state.form.as_ref().map(|form| &form.kind) {
                Some(FormKind::CreateWorkspace | FormKind::CreateWindow) => (76, 12),
                Some(FormKind::RenameWorkspace | FormKind::RenameWindow | FormKind::RenamePane) => {
                    (76, 8)
                }
                Some(
                    FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow,
                ) => (88, 9),
                Some(FormKind::ReopenPane { .. }) => (100, 12),
                _ => (112, screen.rows.saturating_sub(4)),
            }
        };
        let placement = OverlayPlacement::centered(
            screen,
            width.min(screen.cols.saturating_sub(4).max(1)),
            height.min(screen.rows.saturating_sub(2).max(1)),
        );
        let content = if self
            .finder
            .as_ref()
            .is_some_and(|finder| finder.is_graphical())
        {
            placement.outer
        } else {
            placement.content
        };
        (placement.outer, content)
    }
    fn open_form(&mut self, kind: FormKind, value: String) -> Result<()> {
        self.socket.ensure_live()?;
        self.begin_modal();
        self.finder = None;
        self.popover.hide();
        self.retire_resize("modal opened");
        self.selection_focus_lost();
        self.close_search();
        let target = self
            .state
            .current(&self.registry)
            .and_then(|workspace| match &kind {
                FormKind::CreateWindow | FormKind::RenameWorkspace | FormKind::CloseWorkspace => {
                    Some((
                        SidebarTarget::Workspace(workspace.id.clone()),
                        workspace.revision,
                    ))
                }
                FormKind::RenameWindow | FormKind::CloseWindow => {
                    self.state.current_window(&self.registry).map(|window| {
                        (
                            SidebarTarget::Window {
                                workspace: workspace.id.clone(),
                                window: window.id.clone(),
                            },
                            window.revision,
                        )
                    })
                }
                FormKind::RenamePane | FormKind::TerminatePane => {
                    self.state.focused.as_ref().map(|pane| {
                        (
                            SidebarTarget::Pane {
                                workspace: workspace.id.clone(),
                                pane: pane.clone(),
                            },
                            self.state
                                .current_window(&self.registry)
                                .map_or(0, |window| window.revision),
                        )
                    })
                }
                _ => None,
            });
        let defaults = self.defaults()?;
        let directory = if matches!(kind, FormKind::CreateWindow) {
            self.state
                .focused
                .as_ref()
                .and_then(|id| self.registry.panes.iter().find(|pane| pane.id == *id))
                .map(|pane| pane.foreground_cwd.as_ref().unwrap_or(&pane.cwd).clone())
                .unwrap_or_else(|| defaults.cwd.clone())
        } else {
            defaults.cwd.clone()
        };
        self.state.open(kind, value);
        let form = self.state.form.as_mut().unwrap();
        form.fields.inherit_directory(directory);
        form.target = target;
        Ok(())
    }
    fn focus(&mut self, pane: String) {
        if self.state.focused.as_ref() != Some(&pane) {
            self.retire_resize("pane focus changed");
            self.close_search();
        }
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.state.focus(pane);
        self.select_current_lease();
    }
    fn switch(&mut self, workspace: String) {
        if self.state.workspace.as_ref() != Some(&workspace) {
            self.retire_resize("workspace focus changed");
            self.close_search();
        }
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.state.switch(workspace, &self.registry);
        self.select_current_lease();
    }
    fn switch_window(&mut self, window: String) {
        if self.state.window.as_ref() != Some(&window) {
            self.retire_resize("window focus changed");
            self.close_search();
        }
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.state.switch_window(window, &self.registry);
        self.select_current_lease();
    }
    fn select_current_lease(&mut self) {
        if self.host_focused
            && let Some(pane) = &self.state.focused
        {
            self.lease_pending.insert(pane.clone());
        }
    }
    fn manage(
        &mut self,
        command: management::Command,
        selection: Option<management::Selection>,
    ) -> Result<()> {
        self.socket.ensure_live()?;
        let changes_selection = selection.is_some()
            || matches!(
                &command,
                management::Command::Spawn { .. }
                    | management::Command::Create { .. }
                    | management::Command::CreateWindow { .. }
                    | management::Command::Reopen { .. }
            );
        let scope = match &command {
            management::Command::Spawn { window, .. } => format!("window:{}", window.id),
            management::Command::Reopen { window_id, .. } => format!("window:{window_id}"),
            management::Command::Create { .. } => "create".into(),
            management::Command::CreateWindow { workspace_id, .. } => {
                format!("workspace:{workspace_id}")
            }
            management::Command::Single(operation) => match operation {
                Operation::WorkspaceRename { workspace_id, .. }
                | Operation::WorkspaceClose { workspace_id, .. } => {
                    format!("workspace:{workspace_id}")
                }
                Operation::WindowRename { window_id, .. }
                | Operation::WindowClose { window_id, .. }
                | Operation::LayoutUpdate { window_id, .. } => format!("window:{window_id}"),
                Operation::PaneRename { pane_id, .. } | Operation::PaneTerminate { pane_id } => {
                    format!("pane:{pane_id}")
                }
                _ => bail!("unsupported interactive management operation"),
            },
        };
        let key = format!("manage:{scope}");
        let socket = self.socket.to_owned();
        self.jobs.submit(key.clone(), move || {
            management::execute(&socket, command).map(Background::Management)
        })?;
        if changes_selection {
            self.selection_epoch = self.selection_epoch.wrapping_add(1);
        }
        self.management.insert(
            key.clone(),
            ManagementIntent {
                epoch: self.selection_epoch,
                selection,
            },
        );
        if let Some(form) = &mut self.state.form {
            form.pending = Some(key.clone());
            form.error = None;
        }
        Ok(())
    }
    fn operation(&mut self, operation: Operation) -> Result<()> {
        self.manage(management::Command::Single(operation), None)
    }
    fn refresh_popup_ci(&mut self, point: Option<(u16, u16)>) {
        if self.options.read_only {
            return;
        }
        let refresh = self.popover.refresh_at(&self.tree.lock().unwrap(), point);
        if let Some((owner, target)) = refresh {
            self.session_action = Some(SessionAction::CiRefresh { owner, target });
        }
    }
    pub(super) fn ci_refresh_endpoint(
        &self,
        target: &bkpr_protocol::CiRefreshTarget,
    ) -> Option<Endpoint> {
        if self.options.read_only {
            return None;
        }
        let project = self
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == target.pane_id)?
            .project
            .as_ref()?;
        crate::ci::matches_target(project, target).then(|| self.socket.to_owned())
    }
    pub(super) fn queue_ci_refresh(
        &mut self,
        owner: TreeOwner,
        target: bkpr_protocol::CiRefreshTarget,
        socket: Endpoint,
    ) -> Result<()> {
        let key = format!(
            "ci-refresh:{}:{}:{}",
            owner.slot, owner.generation, target.pane_id
        );
        self.jobs.submit(key, move || {
            let result = socket
                .checked(Operation::CiRefresh {
                    target: target.clone(),
                })
                .map(|_| ());
            Ok(Background::CiRefresh {
                owner,
                target,
                result,
            })
        })
    }
    fn accept_management(
        &mut self,
        result: management::Completion,
        intent: Option<ManagementIntent>,
    ) {
        if result.registry.revision >= self.registry.revision {
            self.observe_resize(&result.registry);
            self.registry = result.registry;
            self.state.reconcile(&self.registry);
        }
        if let Some(intent) = intent
            && intent.epoch == self.selection_epoch
            && self.state.form.is_none()
            && let Some(selection) = intent.selection.or(result.selection)
            && let Some(workspace) = self
                .registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == selection.workspace)
        {
            let mut projected = Vec::new();
            let window = workspace
                .windows
                .iter()
                .find(|window| window.id == selection.window);
            leaves(
                window.and_then(|window| window.layout.as_ref()),
                &mut projected,
            );
            if projected.contains(&selection.pane)
                && self.registry.panes.iter().any(|pane| {
                    pane.id == selection.pane
                        && pane.workspace_id.as_ref() == Some(&selection.workspace)
                        && pane.window_id.as_ref() == Some(&selection.window)
                })
            {
                self.state.switch(selection.workspace, &self.registry);
                self.state.switch_window(selection.window, &self.registry);
                self.state.focus(selection.pane);
                self.state.reconcile(&self.registry);
            }
        }
    }
    fn complete_background(&mut self) -> Result<()> {
        for completion in self.jobs.latest()? {
            if self.owns_resize_job(&completion.key) {
                let result = completion.result.and_then(|background| match background {
                    Background::Resize(result) => Ok(result),
                    _ => bail!("resize job returned an unrelated completion"),
                });
                self.complete_resize(result);
                continue;
            }
            let intent = self.management.remove(&completion.key);
            let form_pending = self
                .state
                .form
                .as_ref()
                .is_some_and(|form| form.pending.as_deref() == Some(completion.key.as_str()));
            match completion.result {
                Ok(Background::CiRefresh {
                    owner,
                    target,
                    result,
                }) => {
                    let known = !result.as_ref().is_err_and(transport::outcome_unknown);
                    self.popover.refresh_receipt(owner, &target, known);
                    if let Err(error) = result {
                        self.notify(format!(
                            "CI refresh failed: {error:#}; no replay was attempted"
                        ));
                    }
                }
                Ok(Background::Management(result)) => {
                    if form_pending {
                        self.state.form = None;
                        if result.selection.is_some() && !self.backgrounded {
                            let owner = self
                                .tree
                                .lock()
                                .unwrap()
                                .cursor
                                .as_ref()
                                .map(|target| target.owner);
                            if owner.is_none_or(|owner| owner == self.tree_owner()) {
                                self.leave_sidebar();
                            }
                        }
                    }
                    self.accept_management(result, intent);
                    self.notify(format!("{} completed", completion.key));
                }
                Ok(Background::Resize(_)) => bail!("resize completion lost its captured intent"),
                Ok(Background::Attached { pane, attached }) => {
                    if self.views.contains_key(&pane) || attached.retirement_requested {
                        self.retiring_candidates.push(RetiringCandidate {
                            pane,
                            attached: *attached,
                        });
                    } else {
                        self.views.insert(pane, *attached);
                    }
                }
                Err(error) => {
                    let unknown = if transport::outcome_unknown(&error) {
                        "; outcome unknown; not replayed"
                    } else {
                        "; not replayed"
                    };
                    let message = format!("{} failed{unknown}: {error:#}", completion.key);
                    if form_pending && let Some(form) = &mut self.state.form {
                        form.pending = None;
                        form.ambiguous = transport::outcome_unknown(&error);
                        form.error = Some(message.clone());
                    }
                    if let Some(pane) = completion.key.strip_prefix("attach:") {
                        self.view_failures.insert(pane.to_owned(), message.clone());
                    }
                    if !form_pending || self.backgrounded {
                        self.notify(message);
                    }
                }
            }
        }
        Ok(())
    }
    fn record_input_failure(&mut self, failure: updates::InputFailure) {
        self.notify(failure.to_string());
        let slot = &mut self
            .input_failures
            .entry(failure.pane_id.clone())
            .or_default()[failure.lane.index()];
        if slot.as_ref().is_none_or(|previous| {
            !previous.stopped
                || (failure.stopped
                    && previous.operation == failure.operation
                    && previous.view_id == failure.view_id)
        }) {
            *slot = Some(failure);
        }
    }
    fn scope_input_result(&mut self, result: Result<()>) -> Result<()> {
        match result {
            Err(error) => {
                if let Some(rejected) = error.downcast_ref::<updates::InputRejected>() {
                    self.record_input_failure(rejected.0.clone());
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Ok(()) => Ok(()),
        }
    }
    fn queue_control(&mut self, pane: &str, request: PaneRequest) -> Result<bool> {
        match self.enqueue(pane, request) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.scope_input_result(Err(error))?;
                Ok(false)
            }
        }
    }
    fn input_enabled(&self, pane: &str) -> bool {
        !self.view_failures.contains_key(pane)
            && self
                .views
                .get(pane)
                .is_none_or(|view| !view.retirement_requested && view.spec.visible)
            && self.input_failures.get(pane).is_none_or(|failures| {
                [0, 2].into_iter().all(|index| {
                    failures[index]
                        .as_ref()
                        .is_none_or(|failure| !failure.stopped)
                })
            })
    }
    fn poll_views(&mut self) {
        let mut failed = Vec::new();
        for (pane, view) in &mut self.views {
            if self.view_failures.contains_key(pane) {
                continue;
            }
            let result = view.pump.latest().and_then(|snapshot| {
                if let Some(snapshot) = snapshot {
                    view.apply(snapshot)?;
                }
                Ok(())
            });
            if let Err(error) = result {
                failed.push((pane.clone(), view.view.clone(), format!("{error:#}")));
                view.frame = None;
                view.images.clear();
            }
        }
        for (pane, view, error) in failed {
            self.notify(format!("pane {pane} view {view} poll failed: {error}"));
            self.view_failures.insert(pane, error);
        }
    }
    fn barrier_target(&self) -> Option<(&str, &str)> {
        self.consent
            .as_ref()
            .filter(|consent| consent.state.paste && (consent.state.sent || consent.state.allowed))
            .map(|consent| (consent.pane.as_str(), consent.view.as_str()))
            .or_else(|| {
                self.paste_offer
                    .as_ref()
                    .and(self.paste_target.as_ref())
                    .map(|(pane, view)| (pane.as_str(), view.as_str()))
            })
    }
    fn pointer_origin(
        &self,
        pane: &str,
        view: &str,
        event: MouseEvent,
    ) -> Option<Arc<PointerOrigin>> {
        if !matches!(
            event.button,
            MouseButton::Left | MouseButton::Middle | MouseButton::Right
        ) {
            return None;
        }
        self.pressed_pointers
            .borrow()
            .iter()
            .find(|pressed| {
                pressed.origin.pane == pane
                    && pressed.origin.view == view
                    && pressed.event.button == event.button
            })
            .map(|pressed| pressed.origin.clone())
            .or_else(|| {
                (event.action == MouseAction::Press).then(|| {
                    Arc::new(PointerOrigin {
                        pane: pane.into(),
                        view: view.into(),
                    })
                })
            })
    }
    fn record_pointer(&self, origin: Arc<PointerOrigin>, event: MouseEvent) {
        let mut pointers = self.pressed_pointers.borrow_mut();
        let found = pointers.iter().position(|pressed| {
            pressed.origin.pane == origin.pane
                && pressed.origin.view == origin.view
                && pressed.event.button == event.button
        });
        if event.action == MouseAction::Release {
            if let Some(index) = found {
                pointers.swap_remove(index);
            }
        } else if let Some(index) = found {
            pointers[index].event = event;
            pointers[index].admitted = true;
        } else if event.action == MouseAction::Press {
            pointers.push(PressedPointer {
                origin,
                event,
                admitted: true,
                release: None,
            });
        }
    }
    fn escrow_pointer_release(&self, pane: &str, view: &str, event: MouseEvent) {
        let mut pointers = self.pressed_pointers.borrow_mut();
        let release = Operation::Pane {
            pane_id: pane.into(),
            request: PaneRequest::Mouse {
                view_id: view.into(),
                event,
            },
        };
        if let Some(pressed) = pointers.iter_mut().find(|pressed| {
            pressed.origin.pane == pane
                && pressed.origin.view == view
                && pressed.event.button == event.button
        }) {
            pressed.release = Some(release);
        } else {
            // The press was accepted into the enhanced-paste suffix but has
            // not reached its native lane. Wait for record_pointer(Press).
            pointers.push(PressedPointer {
                origin: Arc::new(PointerOrigin {
                    pane: pane.into(),
                    view: view.into(),
                }),
                event,
                admitted: false,
                release: Some(release),
            });
        }
    }
    fn dispatch_reserved(&self, reserved: updates::ReservedInput) -> Result<()> {
        let pointer = reserved.mouse().and_then(|(pane, view, event)| {
            self.pointer_origin(pane, view, event)
                .map(|origin| (origin, event))
        });
        self.input.send_reserved(reserved)?;
        if let Some((origin, event)) = pointer {
            self.record_pointer(origin, event);
        }
        Ok(())
    }
    fn cancel_pointer(&mut self, pane: &str, view: &str) {
        self.cancel_selection(pane, view);
        if matches!(&self.pointer_capture, Some(PointerCapture::Pane(origin))
            if origin.pane == pane && origin.view == view)
        {
            self.pointer_capture = Some(PointerCapture::Ui);
        }
        for pressed in self
            .pressed_pointers
            .borrow_mut()
            .iter_mut()
            .filter(|pressed| pressed.origin.pane == pane && pressed.origin.view == view)
        {
            if pressed.release.is_none() {
                pressed.release = Some(pressed.release_operation());
            }
        }
    }
    fn flush_pointer_releases(&self) -> Result<()> {
        let mut result = Ok(());
        let barrier = self.barrier_target();
        let mut pointers = self.pressed_pointers.borrow_mut();
        let mut index = 0;
        while index < pointers.len() {
            if self
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == pointers[index].origin.pane)
                .is_none_or(|pane| pane.status != PaneStatus::Running)
            {
                pointers.swap_remove(index);
                continue;
            }
            if !pointers[index].admitted
                || pointers[index].release.is_none()
                || barrier.is_some_and(|(pane, view)| {
                    pointers[index].origin.pane == pane && pointers[index].origin.view == view
                })
            {
                index += 1;
                continue;
            }
            match self
                .input
                .send_owned_operation(&mut pointers[index].release)
            {
                Ok(()) => {
                    pointers.swap_remove(index);
                }
                Err(error) => {
                    // An immediate dispatcher error is local non-admission;
                    // asynchronous native errors are reported by InputPump.
                    if pointers[index].release.is_none() {
                        pointers[index].release = Some(pointers[index].release_operation());
                    }
                    accumulate_cleanup(&mut result, Err(error));
                    index += 1;
                }
            }
        }
        result
    }
    fn host_focus_changed(&mut self, focused: bool) -> Result<()> {
        self.host_focused = focused;
        if !focused {
            let releases = self.cancel_held_keys();
            self.scope_input_result(releases)?;
            self.retire_resize("terminal focus lost");
            self.clear_resize_hover();
            self.sidebar_resize_hover = false;
            if matches!(
                self.pointer_capture,
                Some(PointerCapture::SidebarResize { .. })
            ) {
                self.pointer_capture = Some(PointerCapture::Ui);
            }
            self.selection_focus_lost();
            self.close_search();
        }
        if focused && let Some((pane, _)) = self.active() {
            self.lease_pending.insert(pane);
        }
        self.notify_focus()
    }
    fn cancel_held_key(&mut self, key: &str) -> Result<()> {
        if let Some(held) = self.held.get_mut(key) {
            held.pending = true;
            self.flush_key_releases()
        } else {
            Ok(())
        }
    }
    fn cancel_held_keys(&mut self) -> Result<()> {
        for held in self.held.values_mut() {
            held.pending = true;
        }
        self.flush_key_releases()
    }
    fn flush_key_releases(&mut self) -> Result<()> {
        let mut result = Ok(());
        let mut held = std::mem::take(&mut self.held);
        held.retain(|_, held| {
            if self.execution_completed(&held.pane)
                || self
                    .views
                    .get(&held.pane)
                    .is_none_or(|view| view.view != held.view)
            {
                return false;
            }
            if !held.pending
                || self
                    .barrier_target()
                    .is_some_and(|(pane, view)| pane == held.pane && view == held.view)
            {
                return true;
            }
            match self.enqueue(
                &held.pane,
                PaneRequest::Key {
                    view_id: held.view.clone(),
                    event: held.release.clone(),
                },
            ) {
                Ok(()) => false,
                Err(error) => {
                    accumulate_cleanup(&mut result, Err(error));
                    true
                }
            }
        });
        self.held = held;
        result
    }
    fn enqueue(&self, id: &str, request: PaneRequest) -> Result<()> {
        let lane = updates::InputLane::request(&request);
        if !updates::cleanup(&request)
            && let Some(failure) = self
                .input_failures
                .get(id)
                .and_then(|failures| failures[lane.index()].as_ref())
                .filter(|failure| failure.stopped)
        {
            return Err(updates::InputRejected(failure.clone()).into());
        }
        if !updates::cleanup(&request)
            && self.held.values().any(|held| {
                held.pending
                    && held.pane == id
                    && matches!(&request,
                    PaneRequest::Key { view_id, .. }
                    | PaneRequest::Paste { view_id, .. }
                    | PaneRequest::Mouse { view_id, .. }
                    | PaneRequest::Focus { view_id, .. }
                    | PaneRequest::Scroll { view_id, .. } if view_id == &held.view)
            })
        {
            return Err(updates::InputFailure::admission(
                &Operation::Pane {
                    pane_id: id.into(),
                    request,
                },
                "previous key release is awaiting input admission",
            )
            .into());
        }
        let target = self.barrier_target();
        let ordered = match &request {
            PaneRequest::Key { view_id, .. }
            | PaneRequest::Paste { view_id, .. }
            | PaneRequest::Mouse { view_id, .. }
            | PaneRequest::Focus { view_id, .. }
            | PaneRequest::Scroll { view_id, .. } => {
                target.is_some_and(|(pane, view)| pane == id && view_id == view)
            }
            _ => false,
        };
        let operation = Operation::Pane {
            pane_id: id.into(),
            request,
        };
        if ordered {
            let mut deferred = self.deferred.borrow_mut();
            if deferred.len() >= 64 {
                return Err(updates::InputFailure::admission(
                    &operation,
                    "pane-local enhanced paste input barrier full",
                )
                .into());
            }
            deferred.push_back(self.input.reserve(operation)?);
            Ok(())
        } else {
            self.input.send(operation)
        }
    }
    fn execution_completed(&self, pane: &str) -> bool {
        self.registry
            .panes
            .iter()
            .any(|info| info.id == pane && info.status == PaneStatus::Exited)
            || self.views.get(pane).is_some_and(|attached| {
                attached.metadata.error.is_none()
                    && matches!(
                        attached.metadata.lifecycle.as_str(),
                        "exited" | "terminated"
                    )
            })
    }
    fn active(&self) -> Option<(String, String)> {
        let id = self.state.focused.as_ref()?;
        if !self.input_enabled(id) || self.execution_completed(id) {
            return None;
        }
        if !self
            .registry
            .panes
            .iter()
            .any(|p| p.id == *id && p.status == PaneStatus::Running)
        {
            return None;
        }
        let attached = self.views.get(id)?;
        Some((id.clone(), attached.view.clone()))
    }
    fn spec(&self, size: Size) -> ViewSpec {
        ViewSpec {
            size,
            interactive: true,
            visible: true,
            input: !self.options.read_only,
            clipboard: !self.options.read_only && self.options.clipboard_policy != "deny",
        }
    }
    fn rects(&self, size: Size) -> Vec<(String, Rect)> {
        let (_, content) = self.screen_geometry(size);
        let mut rects = Vec::new();
        if let Some(window) = self.state.current_window(&self.registry) {
            layout_rects(window.layout.as_ref(), content, &mut rects);
        }
        rects
    }
    fn pane_retiring(&self, pane: &str) -> bool {
        self.retiring
            .iter()
            .any(|retired| retired.attached.metadata.pane_id == pane)
            || self
                .retiring_candidates
                .iter()
                .any(|candidate| candidate.pane == pane)
    }
    fn placeholder(&self, pane: &str) -> String {
        if let Some(error) = self.view_failures.get(pane) {
            return error.clone();
        }
        if let Some(failure) = self
            .input_failures
            .get(pane)
            .and_then(|failures| failures.iter().flatten().find(|failure| failure.stopped))
        {
            return failure.to_string();
        }
        match self
            .registry
            .panes
            .iter()
            .find(|info| info.id == pane)
            .map(|info| &info.status)
        {
            None | Some(PaneStatus::Stopped) => {
                if self.options.read_only {
                    return "Execution unavailable. Read-only client cannot open a new shell or change this layout.".into();
                }
                return "Execution unavailable. Ctrl-b R opens a new shell here after confirmation; Ctrl-b n splits a new pane.".into();
            }
            Some(PaneStatus::Exited) => {
                if self.options.read_only {
                    return "Execution exited; any retained scene is read-only. This client cannot open a new shell.".into();
                }
                return "Execution exited; any retained scene is read-only. Ctrl-b R opens a new shell here after confirmation.".into();
            }
            Some(PaneStatus::Running) => {}
        }
        if self
            .views
            .get(pane)
            .is_some_and(|view| view.retirement_requested)
        {
            return "Previous view retiring; accepted operations are still draining.".into();
        }
        if self.pane_retiring(pane) {
            return "Previous view retiring; accepted operations are still draining.".into();
        }
        "Attaching pane; waiting for its initial scene or a background worker slot.".into()
    }
    fn badge(&self, pane: &str) -> Option<&'static str> {
        if self.view_failures.contains_key(pane) {
            return Some("VIEW ERROR");
        }
        if let Some(failures) = self.input_failures.get(pane) {
            if [0, 2].into_iter().any(|index| {
                failures[index]
                    .as_ref()
                    .is_some_and(|failure| failure.stopped)
            }) {
                return Some("INPUT ERROR");
            }
            if failures[1].as_ref().is_some_and(|failure| failure.stopped) {
                return Some("CLIPBOARD ERROR");
            }
        }
        if let Some(info) = self.registry.panes.iter().find(|info| info.id == pane) {
            match info.status {
                PaneStatus::Stopped => return Some("STOPPED"),
                PaneStatus::Exited => {
                    return Some(
                        if self
                            .views
                            .get(pane)
                            .is_some_and(|view| view.frame.is_some())
                        {
                            "EXITED SCENE"
                        } else {
                            "EXITED"
                        },
                    );
                }
                PaneStatus::Running => {}
            }
        }
        match self.views.get(pane) {
            Some(view) if view.retirement_requested => Some("RETIRING"),
            Some(view) if view.frame.is_some() => None,
            _ => Some("UNPROJECTED"),
        }
    }
    fn device_busy(&self, pane: &str, view: &str) -> bool {
        self.responses
            .iter()
            .any(|response| response.pane == pane && response.view == view)
            || self.consent.as_ref().is_some_and(|consent| {
                consent.pane == pane
                    && consent.view == view
                    && (consent.reply.is_some() || consent.state.sent)
            })
            || self
                .paste_target
                .as_ref()
                .is_some_and(|(origin, target)| origin == pane && target == view)
    }
    fn reap_responses(&mut self) {
        let mut index = 0;
        while index < self.responses.len() {
            match self.responses[index].receipt.try_recv() {
                Ok(result) => {
                    let response = self.responses.swap_remove(index);
                    if let Err(error) = result {
                        self.notify(format!(
                            "pane {} view {} clipboard response failed: {error}",
                            response.pane, response.view
                        ));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => index += 1,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let response = self.responses.swap_remove(index);
                    self.notify(format!(
                        "pane {} view {} clipboard completion disconnected; outcome unknown; not replayed",
                        response.pane, response.view
                    ));
                }
            }
        }
    }
    fn retire_view(&mut self, pane: &str, wait_policy: bool) -> Result<()> {
        let Some(attached) = self.views.get(pane) else {
            return Ok(());
        };
        let target = (pane.to_owned(), attached.view.clone());
        let had_retirement_error = attached.retirement_error.is_some();
        self.cancel_pointer(pane, &target.1);
        let cancellation = self.flush_pointer_releases();
        let completed = self.execution_completed(pane);
        if completed {
            // No execution remains to receive a previously deferred focus-out.
            self.views.get_mut(pane).unwrap().retirement_focus_out = false;
        }
        let focus_out = !completed
            && !self.options.read_only
            && (self.notified_focus.as_ref() == Some(&target)
                || self.focus_out_pending.contains(&target));
        if wait_policy && self.device_busy(pane, &target.1) && !self.backgrounded {
            let attached = self.views.get_mut(pane).unwrap();
            attached.retirement_requested = true;
            attached.retirement_focus_out |= focus_out;
            return cancellation;
        }
        let ready = !self
            .pressed_pointers
            .borrow()
            .iter()
            .any(|pressed| pressed.origin.pane == pane && pressed.origin.view == target.1)
            && (!wait_policy
                || (!self.device_busy(pane, &target.1)
                    && !self
                        .held
                        .values()
                        .any(|held| held.pane == pane && held.view == target.1)));
        let receipt = match self.views.get_mut(pane).unwrap().retire(
            &self.input,
            pane,
            focus_out,
            wait_policy,
            ready,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                let mut result = cancellation;
                accumulate_cleanup(&mut result, Err(error));
                return result;
            }
        };
        if !had_retirement_error && let Some(error) = &self.views[pane].retirement_error {
            self.notify(format!("{error:#}"));
        }
        let Some(receipt) = receipt else {
            return cancellation;
        };
        self.focus_out_pending.remove(&target);
        self.lease_pending.remove(pane);
        if self.notified_focus.as_ref() == Some(&target) {
            self.notified_focus = None;
        }
        let attached = self.views.remove(pane).unwrap();
        self.held.retain(|_, held| held.pane != pane);
        self.retiring.push(RetiringView { attached, receipt });
        cancellation
    }
    fn reap_retiring(&mut self) -> Result<()> {
        let mut index = 0;
        while index < self.retiring.len() {
            match self.retiring[index].receipt.try_recv() {
                Ok(result) => {
                    let retired = self.retiring.swap_remove(index);
                    if let Err(error) = result {
                        let pane = retired.attached.metadata.pane_id.clone();
                        let message = format!(
                            "pane {pane} view {} detach failed: {error}; retirement not confirmed",
                            retired.attached.view
                        );
                        self.view_failures.insert(pane, message.clone());
                        self.notify(message);
                    }
                    drop(retired);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let retired = self.retiring.swap_remove(index);
                    let pane = retired.attached.metadata.pane_id.clone();
                    let message = format!(
                        "pane {pane} view {} retirement completion unknown",
                        retired.attached.view
                    );
                    self.view_failures.insert(pane, message.clone());
                    self.notify(message);
                    drop(retired);
                }
            }
        }
        Ok(())
    }
    fn sync_views(&mut self, size: Size, rects: &[(String, Rect)]) -> Result<()> {
        let visible = rects
            .iter()
            .filter(|(_, r)| content(*r).height > 0)
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        let removed = self
            .views
            .keys()
            .filter(|id| {
                !visible.contains(*id)
                    || self.views[*id].retirement_requested
                    || self.view_failures.contains_key(*id)
                    || self
                        .registry
                        .panes
                        .iter()
                        .find(|p| p.id == **id)
                        .is_none_or(|p| p.status == PaneStatus::Stopped)
            })
            .cloned()
            .collect::<Vec<_>>();
        if self
            .consent
            .as_ref()
            .is_some_and(|c| removed.contains(&c.pane) && (!self.backgrounded || !c.state.sent))
        {
            let result = self.finish_consent(ClipboardResponse::denied());
            self.scope_input_result(result)?;
        }
        if !self.backgrounded
            && self
                .paste_target
                .as_ref()
                .is_some_and(|(pane, _)| removed.contains(pane))
        {
            let result = self.abort_listing("paste origin left the visible layout");
            self.scope_input_result(result)?;
        }
        for id in removed {
            let result = self.retire_view(&id, true);
            self.scope_input_result(result)?;
        }
        let mut index = 0;
        while index < self.retiring_candidates.len() {
            let candidate = &mut self.retiring_candidates[index];
            let result = candidate
                .attached
                .retire(&self.input, &candidate.pane, false, true, true);
            match result {
                Ok(Some(receipt)) => {
                    let candidate = self.retiring_candidates.swap_remove(index);
                    self.retiring.push(RetiringView {
                        attached: candidate.attached,
                        receipt,
                    });
                }
                Ok(None) => index += 1,
                Err(error) => {
                    self.scope_input_result(Err(error))?;
                    index += 1;
                }
            }
        }
        let focused = rects
            .iter()
            .position(|(id, _)| Some(id) == self.state.focused.as_ref());
        for index in focused
            .into_iter()
            .chain((0..rects.len()).filter(|index| Some(*index) != focused))
        {
            let (id, rect) = &rects[index];
            let rect = content(*rect);
            if rect.width == 0 || rect.height == 0 {
                continue;
            }
            let local = rect.size(size);
            let mut spec = self.spec(local);
            if self
                .input_failures
                .get(id)
                .and_then(|failures| failures[1].as_ref())
                .is_some_and(|failure| failure.stopped)
            {
                spec.clipboard = false;
            }
            if !self
                .registry
                .panes
                .iter()
                .any(|p| p.id == *id && p.status == PaneStatus::Running)
            {
                spec.input = false;
                spec.interactive = false;
                spec.clipboard = false;
            }
            if let Some(attached) = self.views.get(id) {
                if attached.retirement_requested {
                    continue;
                }
                if attached.spec.clipboard
                    && !spec.clipboard
                    && self.device_busy(id, &attached.view)
                {
                    continue;
                }
                if attached.spec.size != local
                    || !attached.spec.visible
                    || attached.spec.interactive != spec.interactive
                    || attached.spec.input != spec.input
                    || attached.spec.clipboard != spec.clipboard
                    || self.lease_pending.contains(id)
                {
                    let view = attached.view.clone();
                    if self.queue_control(
                        id,
                        PaneRequest::Update {
                            view_id: view,
                            spec: spec.clone(),
                        },
                    )? {
                        self.views.get_mut(id).unwrap().spec = spec;
                        self.lease_pending.remove(id);
                    }
                }
            } else if !self.view_failures.contains_key(id)
                && self
                    .registry
                    .panes
                    .iter()
                    .any(|p| p.id == *id && p.status != PaneStatus::Stopped)
            {
                let key = format!("attach:{id}");
                if !self.jobs.contains(&key) && !self.jobs.is_full() && !self.pane_retiring(id) {
                    let socket = self.socket.to_owned();
                    let pane = id.clone();
                    if let Err(error) = self.jobs.submit(key, move || {
                        attach(&socket, &pane, spec).map(|attached| Background::Attached {
                            pane,
                            attached: Box::new(attached),
                        })
                    }) {
                        let message = format!("pane {id} attach not admitted: {error:#}");
                        self.view_failures.insert(id.clone(), message.clone());
                        self.notify(message);
                    }
                }
            }
        }
        self.notify_focus()
    }
    fn notify_focus(&mut self) -> Result<()> {
        let focused = if self.host_focused
            && self.state.form.is_none()
            && !self.picker_open()
            && !self.state.sidebar.active
            && !self.search_open()
            && !self
                .consent
                .as_ref()
                .is_some_and(|c| !c.state.allowed && !c.state.sent)
        {
            self.active()
        } else {
            None
        };
        if focused != self.notified_focus
            && let Some(previous) = self.notified_focus.take()
        {
            self.focus_out_pending.insert(previous);
        }
        let pending = self.focus_out_pending.iter().cloned().collect::<Vec<_>>();
        for (pane, view) in pending {
            if !self.views.contains_key(&pane)
                || self.options.read_only
                || self.execution_completed(&pane)
                || self
                    .views
                    .get(&pane)
                    .is_some_and(|attached| attached.retirement_requested)
                || self.queue_control(
                    &pane,
                    PaneRequest::Focus {
                        view_id: view.clone(),
                        focused: false,
                    },
                )?
            {
                self.focus_out_pending.remove(&(pane, view));
            }
        }
        if focused != self.notified_focus
            && let Some((pane, view)) = &focused
            && (self.options.read_only
                || self.queue_control(
                    pane,
                    PaneRequest::Focus {
                        view_id: view.clone(),
                        focused: true,
                    },
                )?)
        {
            self.notified_focus = focused;
        }
        Ok(())
    }
    fn confirm_reopen(&mut self) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot open a new shell");
        }
        let workspace = self.state.current(&self.registry).context("no workspace")?;
        let window = self
            .state
            .current_window(&self.registry)
            .context("no window")?;
        let pane_id = self.state.focused.as_ref().context("no focused pane")?;
        let pane = self
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == *pane_id)
            .context("focused pane record is unavailable")?;
        if pane.status == PaneStatus::Running {
            bail!("focused execution is still live; opening a new shell cannot replace it");
        }
        let mut projected = Vec::new();
        leaves(window.layout.as_ref(), &mut projected);
        if !projected.contains(pane_id)
            || pane.workspace_id.as_ref() != Some(&workspace.id)
            || pane.window_id.as_ref() != Some(&window.id)
        {
            bail!("focused pane is no longer owned and projected in this window");
        }
        self.selection_epoch = self.selection_epoch.wrapping_add(1);
        self.open_form(
            FormKind::ReopenPane {
                workspace_id: workspace.id.clone(),
                window_id: window.id.clone(),
                expected_revision: window.revision,
                pane_id: pane_id.clone(),
                selection_epoch: self.selection_epoch,
                cwd: pane.foreground_cwd.as_ref().unwrap_or(&pane.cwd).clone(),
            },
            self.defaults()?.cwd.display().to_string(),
        )?;
        Ok(())
    }
    fn contextual_environment(&self) -> Result<(PathBuf, bool)> {
        if self.socket.is_remote()
            && let Some(pane) = self
                .state
                .focused
                .as_ref()
                .and_then(|id| self.registry.panes.iter().find(|pane| pane.id == *id))
        {
            Ok((pane.home.clone(), pane.isolated_environment))
        } else {
            let defaults = self.defaults()?;
            Ok((defaults.home.clone(), defaults.isolated_environment))
        }
    }
    fn spawn(&mut self, size: Size, axis: Axis) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot create panes");
        }
        let workspace = self
            .state
            .current(&self.registry)
            .cloned()
            .context("create a workspace first (Ctrl-b c)")?;
        let window = self
            .state
            .current_window(&self.registry)
            .cloned()
            .context("no selected window")?;
        let parent = self
            .state
            .focused
            .as_ref()
            .and_then(|id| self.registry.panes.iter().find(|p| p.id == *id));
        let defaults = self.defaults()?;
        let spec = PaneSpawnSpec {
            argv: defaults.argv.clone(),
            cwd: parent
                .map(|p| {
                    if self.socket.is_remote() {
                        p.foreground_cwd.as_ref().unwrap_or(&p.cwd).clone()
                    } else {
                        p.cwd.clone()
                    }
                })
                .unwrap_or_else(|| defaults.cwd.clone()),
            home: parent
                .map(|p| p.home.clone())
                .unwrap_or_else(|| defaults.home.clone()),
            size,
            title: None,
            isolated_environment: parent
                .map(|p| p.isolated_environment)
                .unwrap_or(defaults.isolated_environment),
        };
        let target = self.state.focused.clone().unwrap_or_default();
        self.manage(
            management::Command::Spawn {
                workspace,
                window,
                target,
                axis,
                spec,
            },
            None,
        )
    }
    fn show_existing(&mut self, pane: String) -> Result<()> {
        let owner = self
            .registry
            .panes
            .iter()
            .find(|entry| entry.id == pane)
            .context("terminal no longer exists")?;
        let workspace_id = owner
            .workspace_id
            .clone()
            .context("administrative terminal has no owning workspace")?;
        let window_id = owner
            .window_id
            .clone()
            .context("terminal has no owning window")?;
        let window = self
            .registry
            .workspaces
            .iter()
            .find(|entry| entry.id == workspace_id)
            .and_then(|entry| entry.windows.iter().find(|window| window.id == window_id))
            .cloned()
            .context("terminal's owning window no longer exists")?;
        if self.state.workspace.as_ref() != Some(&workspace_id) {
            self.switch(workspace_id.clone());
        }
        if self.state.window.as_ref() != Some(&window_id) {
            self.switch_window(window_id.clone());
        }
        let mut visible = Vec::new();
        leaves(window.layout.as_ref(), &mut visible);
        if !visible.contains(&pane) {
            if self.options.read_only {
                bail!("read-only client cannot restore a removed terminal projection");
            }
            let target = self.state.focused.as_deref().unwrap_or("");
            let layout = split_leaf(window.layout, target, &pane, Axis::Horizontal);
            return self.manage(
                management::Command::Single(Operation::LayoutUpdate {
                    workspace_id: workspace_id.clone(),
                    window_id: window_id.clone(),
                    expected_revision: window.revision,
                    layout,
                }),
                Some(management::Selection {
                    workspace: workspace_id,
                    window: window_id,
                    pane,
                }),
            );
        }
        self.focus(pane);
        Ok(())
    }
    fn fail_paste(
        &mut self,
        pane: &str,
        view: &str,
        error: anyhow::Error,
        extra: usize,
    ) -> anyhow::Error {
        self.cancel_pointer(pane, view);
        let mut deferred = self.deferred.borrow_mut();
        let not_dispatched = deferred.len() + extra;
        deferred.clear();
        self.pressed_pointers.borrow_mut().retain(|pressed| {
            pressed.admitted || pressed.origin.pane != pane || pressed.origin.view != view
        });
        let prior = error.downcast_ref::<updates::InputRejected>();
        let unknown_outcome = prior.is_some_and(|rejected| rejected.0.unknown_outcome);
        let stopped = not_dispatched > 0 || prior.is_some_and(|rejected| rejected.0.stopped);
        updates::InputRejected(updates::InputFailure {
            pane_id: pane.into(),
            view_id: Some(view.into()),
            operation: "paste_contents",
            lane: updates::InputLane::Commands,
            stopped,
            unknown_outcome,
            not_dispatched,
            error: format!("{error:#}; following pane input was not dispatched"),
        })
        .into()
    }
    fn abort_listing(&mut self, reason: &str) -> Result<()> {
        let listing = self.paste_offer.take().is_some();
        let target = self.paste_target.take();
        if listing || !self.deferred.borrow().is_empty() {
            let (pane, view) = target.context("enhanced paste barrier lost its origin")?;
            return Err(self.fail_paste(&pane, &view, anyhow::anyhow!("{reason}"), 0));
        }
        Ok(())
    }
    fn finish_consent(&mut self, response: ClipboardResponse) -> Result<()> {
        let Some(mut consent) = self.consent.take() else {
            return Ok(());
        };
        if consent.state.paste {
            if response.result != ClipboardResult::Success && !self.deferred.borrow().is_empty() {
                return Err(self.fail_paste(
                    &consent.pane,
                    &consent.view,
                    anyhow::anyhow!("enhanced paste failed: {:?}", response.result),
                    0,
                ));
            }
            if response.result == ClipboardResult::Success {
                let result = self.enqueue(
                    &consent.pane,
                    PaneRequest::PasteContents {
                        view_id: consent.view.clone(),
                        contents: response.contents,
                        location: consent.state.request.location,
                        allow_unsafe: false,
                    },
                );
                if let Err(error) = result {
                    return Err(self.fail_paste(&consent.pane, &consent.view, error, 0));
                }
            }
            loop {
                let operation = self.deferred.borrow_mut().pop_front();
                let Some(operation) = operation else {
                    break;
                };
                if let Err(error) = self.dispatch_reserved(operation) {
                    return Err(self.fail_paste(&consent.pane, &consent.view, error, 1));
                }
            }
        } else {
            let result = if let Some(reservation) = consent.reservation.take() {
                self.input.send_response(reservation, response)
            } else {
                if consent.reply.is_none() {
                    consent.reply = Some(Operation::Pane {
                        pane_id: consent.pane.clone(),
                        request: PaneRequest::Clipboard {
                            view_id: consent.view.clone(),
                            request_id: consent.state.id.clone(),
                            response,
                        },
                    });
                }
                self.input.send_owned(&mut consent.reply)
            };
            match result {
                Ok(receipt) => {
                    self.responses.push(DeviceResponse {
                        pane: consent.pane.clone(),
                        view: consent.view.clone(),
                        receipt,
                    });
                    self.answered.insert(consent.pane, consent.state.id);
                }
                Err(error) => {
                    let retained =
                        consent.reply.is_some()
                            && error.downcast_ref::<updates::InputRejected>().is_some_and(
                                |rejected| !rejected.0.stopped && !rejected.0.unknown_outcome,
                            );
                    if retained {
                        self.consent = Some(consent);
                    } else {
                        self.answered.insert(consent.pane, consent.state.id);
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn begin_consent(&mut self, out: &mut impl Write) -> Result<()> {
        let Some(consent) = self.consent.as_mut() else {
            return Ok(());
        };
        if !consent.state.paste && consent.reservation.is_none() {
            match self
                .input
                .reserve_response(&consent.pane, &consent.view, &consent.state.id)
            {
                Ok(reservation) => consent.reservation = Some(reservation),
                Err(error) => {
                    if error
                        .downcast_ref::<updates::InputRejected>()
                        .is_some_and(|rejected| rejected.0.stopped || rejected.0.unknown_outcome)
                    {
                        let consent = self.consent.take().unwrap();
                        self.answered.insert(consent.pane, consent.state.id);
                    }
                    return Err(error);
                }
            }
        }
        let native_id = std::mem::take(&mut consent.state.id);
        consent.state.id = format!("a{}g{}_{}", self.namespace.0, self.namespace.1, native_id);
        let answer = begin_clipboard(out, &mut consent.state);
        consent.state.id = native_id;
        let answer = answer?;
        if let Some(answer) = answer {
            self.finish_consent(answer)?;
        }
        Ok(())
    }
    fn drain_responses(&mut self) -> Result<()> {
        let mut result = Ok(());
        for response in self.responses.drain(..) {
            accumulate_cleanup(
                &mut result,
                response
                    .receipt
                    .recv()
                    .context("device reply worker stopped before completion")
                    .and_then(|result| result),
            );
        }
        result
    }
    fn clipboard(&mut self, out: &mut impl Write) -> Result<()> {
        self.answered.retain(|pane, id| {
            self.views
                .get(pane)
                .is_some_and(|view| view.clipboard_id.as_ref() == Some(&*id))
        });
        if self.consent.as_ref().is_some_and(|consent| {
            !consent.state.paste
                && self.views.get(&consent.pane).is_none_or(|view| {
                    self.view_failures.contains_key(&consent.pane)
                        || view.clipboard_id.as_ref() != Some(&consent.state.id)
                })
        }) {
            let consent = self.consent.take().unwrap();
            self.notify(format!(
                "pane {} view {} clipboard grant expired or was revoked",
                consent.pane, consent.view
            ));
            if let Some(reply) = consent.reply {
                return Err(updates::InputFailure::admission(
                    &reply,
                    "clipboard grant expired before reply admission; response was not replayed",
                )
                .into());
            }
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| consent.reply.is_some())
        {
            // Retry only local non-admission, preserving the original bytes.
            self.finish_consent(ClipboardResponse::denied())?;
        }
        let pending = self
            .views
            .iter()
            .filter(|(id, view)| {
                !view.retirement_requested
                    && view.spec.clipboard
                    && !self.view_failures.contains_key(*id)
                    && view.metadata.clipboard.is_some()
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for pane in pending {
            let attached = self.views.get_mut(&pane).unwrap();
            let view = attached.view.clone();
            let pending = attached.metadata.clipboard.take().unwrap();
            if pending.view_id != view {
                let error =
                    format!("pane {pane} clipboard grant does not belong to attached view {view}");
                self.view_failures.insert(pane, error.clone());
                self.notify(error);
                continue;
            }
            let id = &pending.request_id;
            if self.answered.get(&pane) == Some(id) {
                continue;
            }
            if self
                .consent
                .as_ref()
                .is_some_and(|c| c.pane == pane && c.state.id == *id)
            {
                continue;
            }
            if self.consent.is_some()
                || self.paste_offer.is_some()
                || self.state.form.is_some()
                || self.picker_open()
            {
                let mut response = Some(Operation::Pane {
                    pane_id: pane.clone(),
                    request: PaneRequest::Clipboard {
                        view_id: view.clone(),
                        request_id: id.clone(),
                        response: ClipboardResponse::denied(),
                    },
                });
                match self.input.send_owned(&mut response) {
                    Ok(receipt) => {
                        self.responses.push(DeviceResponse {
                            pane: pane.clone(),
                            view,
                            receipt,
                        });
                        self.answered.insert(pane, id.clone());
                    }
                    Err(error) => {
                        if response.is_some()
                            && error.downcast_ref::<updates::InputRejected>().is_some_and(
                                |rejected| !rejected.0.stopped && !rejected.0.unknown_outcome,
                            )
                        {
                            self.views.get_mut(&pane).unwrap().metadata.clipboard = Some(pending);
                        } else {
                            self.answered.insert(pane, id.clone());
                        }
                        return Err(error);
                    }
                }
                continue;
            }
            self.consent = Some(DeviceConsent {
                pane,
                view,
                reply: None,
                reservation: None,
                state: Consent {
                    id: pending.request_id,
                    request: pending.request,
                    started: Instant::now(),
                    sent: false,
                    allowed: self.options.clipboard_policy == "allow",
                    contents: Vec::new(),
                    available: Vec::new(),
                    enhanced_supported: self.enhanced_supported,
                    paste: false,
                    password: None,
                },
            });
        }
        if let Some(consent) = self.consent.as_mut() {
            consent.state.enhanced_supported = self.enhanced_supported;
            if self.options.clipboard_policy == "allow" {
                consent.state.allowed = true;
            }
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| consent.state.allowed && !consent.state.sent)
        {
            self.begin_consent(out)?;
        }
        Ok(())
    }
    fn paste_reply(&mut self, response: &[u8]) -> Result<()> {
        let listing = self.paste_offer.is_some();
        match paste_offer_packet(&mut self.paste_offer, response) {
            Ok(Some(consent)) => {
                let (pane, view) = self
                    .paste_target
                    .take()
                    .context("enhanced paste listing lost its origin")?;
                self.consent = Some(DeviceConsent {
                    pane,
                    view,
                    state: consent,
                    reply: None,
                    reservation: None,
                });
            }
            Ok(None) if !listing && self.paste_offer.is_some() => {
                if self.options.read_only || self.state.sidebar.active || self.picker_open() {
                    self.paste_offer = None;
                    return Ok(());
                }
                self.paste_target = self.active();
                if self.paste_target.is_none() {
                    self.paste_offer = None;
                    self.notify(
                        "Enhanced paste has no focused pane; no input was dispatched".into(),
                    );
                }
            }
            Ok(None) if listing && self.paste_offer.is_none() => {
                let (pane, view) = self
                    .paste_target
                    .take()
                    .context("completed enhanced paste listing lost its origin")?;
                // Empty clipboard is a successful no-op, not a broken input
                // stream. Release the already accepted suffix in original order.
                loop {
                    let operation = self.deferred.borrow_mut().pop_front();
                    let Some(operation) = operation else {
                        break;
                    };
                    if let Err(error) = self.dispatch_reserved(operation) {
                        return Err(self.fail_paste(&pane, &view, error, 1));
                    }
                }
                self.notify("Clipboard has no paste content; following input released".into());
            }
            Ok(None) => {}
            Err(error) => {
                self.paste_offer = None;
                if let Some((pane, view)) = self.paste_target.take().or_else(|| self.active()) {
                    return Err(self.fail_paste(&pane, &view, error, 0));
                }
                self.notify(format!("Enhanced paste offer rejected: {error:#}"));
            }
        }
        Ok(())
    }
    fn apply_form(&mut self, size: Size) -> Result<()> {
        let Some(form) = self.state.form.as_ref().cloned() else {
            return Ok(());
        };
        if form.pending.is_some() || form.ambiguous {
            bail!(
                "submission already pending or unconfirmed; inspect the result before submitting again"
            );
        }
        match form.kind {
            FormKind::ProjectDetails => self.state.cancel(),
            FormKind::ReopenPane {
                workspace_id,
                window_id,
                expected_revision,
                pane_id,
                selection_epoch,
                cwd,
            } => {
                if self.options.read_only {
                    bail!("read-only client cannot open a new shell");
                }
                if selection_epoch != self.selection_epoch
                    || self.state.workspace.as_ref() != Some(&workspace_id)
                    || self.state.window.as_ref() != Some(&window_id)
                    || self.state.focused.as_ref() != Some(&pane_id)
                {
                    bail!("selection changed while confirming the new shell; reopen confirmation");
                }
                if self
                    .registry
                    .panes
                    .iter()
                    .any(|pane| pane.id == pane_id && pane.status == PaneStatus::Running)
                {
                    bail!("focused execution is still live; it was not replaced");
                }
                let (home, isolated_environment) = self.contextual_environment()?;
                self.manage(
                    management::Command::Reopen {
                        workspace_id,
                        window_id,
                        expected_revision,
                        pane_id,
                        spec: PaneSpawnSpec {
                            argv: self.defaults()?.argv.clone(),
                            cwd: if form.selection == 1 {
                                self.defaults()?.cwd.clone()
                            } else {
                                cwd
                            },
                            home,
                            size,
                            title: None,
                            isolated_environment,
                        },
                    },
                    None,
                )?;
            }
            FormKind::CreateWorkspace => {
                if self.options.read_only {
                    bail!("read-only client cannot create workspaces");
                }
                let cwd = form.fields.directory();
                self.manage(
                    management::Command::Create {
                        name: (!form.fields.primary.value.trim().is_empty())
                            .then_some(form.fields.primary.value),
                        spec: PaneSpawnSpec {
                            argv: self.defaults()?.argv.clone(),
                            cwd,
                            home: self.defaults()?.home.clone(),
                            size,
                            title: None,
                            isolated_environment: self.defaults()?.isolated_environment,
                        },
                    },
                    None,
                )?;
            }
            FormKind::CreateWindow => {
                if self.options.read_only {
                    bail!("read-only client cannot create windows");
                }
                let workspace = self.form_workspace(form.target.as_ref())?;
                let cwd = form.fields.directory();
                let (home, isolated_environment) = self.contextual_environment()?;
                self.manage(
                    management::Command::CreateWindow {
                        workspace_id: workspace.id,
                        expected_revision: workspace.revision,
                        name: (!form.fields.primary.value.trim().is_empty())
                            .then_some(form.fields.primary.value),
                        spec: PaneSpawnSpec {
                            argv: self.defaults()?.argv.clone(),
                            cwd,
                            home,
                            size,
                            title: None,
                            isolated_environment,
                        },
                    },
                    None,
                )?;
            }
            FormKind::RenameWindow => {
                if self.options.read_only {
                    bail!("read-only client cannot rename windows");
                }
                if form.fields.primary.value.trim().is_empty() {
                    bail!("window name cannot be empty");
                }
                let (workspace_id, window) = self.form_window(form.target.as_ref())?;
                self.operation(Operation::WindowRename {
                    workspace_id,
                    window_id: window.id,
                    expected_revision: window.revision,
                    name: form.fields.primary.value,
                })?;
            }
            FormKind::RenameWorkspace => {
                if self.options.read_only {
                    bail!("read-only client cannot rename workspaces");
                }
                if form.fields.primary.value.trim().is_empty() {
                    bail!("workspace name cannot be empty");
                }
                let workspace = self.form_workspace(form.target.as_ref())?;
                self.operation(Operation::WorkspaceRename {
                    workspace_id: workspace.id,
                    expected_revision: workspace.revision,
                    name: form.fields.primary.value,
                })?;
            }
            FormKind::RenamePane => {
                if self.options.read_only {
                    bail!("read-only client cannot rename panes");
                }
                if form.fields.primary.value.trim().is_empty() {
                    bail!("terminal name cannot be empty");
                }
                self.operation(Operation::PaneRename {
                    pane_id: self.form_pane(form.target.as_ref())?,
                    title: form.fields.primary.value,
                })?;
            }
            FormKind::TerminatePane => {
                if form.confirmed {
                    if self.options.read_only {
                        bail!("read-only client cannot terminate panes");
                    }
                    self.operation(Operation::PaneTerminate {
                        pane_id: self.form_pane(form.target.as_ref())?,
                    })?;
                }
            }
            FormKind::CloseWindow => {
                if form.confirmed {
                    if self.options.read_only {
                        bail!("read-only client cannot close windows");
                    }
                    let (workspace_id, window) = self.form_window(form.target.as_ref())?;
                    self.operation(Operation::WindowClose {
                        workspace_id,
                        window_id: window.id,
                        expected_revision: window.revision,
                    })?;
                }
            }
            FormKind::CloseWorkspace => {
                if form.confirmed {
                    if self.options.read_only {
                        bail!("read-only client cannot close workspaces");
                    }
                    let workspace = self.form_workspace(form.target.as_ref())?;
                    self.operation(Operation::WorkspaceClose {
                        workspace_id: workspace.id,
                        expected_revision: workspace.revision,
                    })?;
                }
            }
        }
        Ok(())
    }
    fn directional_target<'a>(&self, key: &str, rects: &'a [(String, Rect)]) -> Option<&'a str> {
        let (_, current) = rects
            .iter()
            .find(|(id, _)| Some(id) == self.state.focused.as_ref())?;
        let cx = i64::from(current.x) * 2 + i64::from(current.width);
        let cy = i64::from(current.y) * 2 + i64::from(current.height);
        rects
            .iter()
            .filter(|(id, _)| Some(id) != self.state.focused.as_ref())
            .filter_map(|(id, r)| {
                let dx = i64::from(r.x) * 2 + i64::from(r.width) - cx;
                let dy = i64::from(r.y) * 2 + i64::from(r.height) - cy;
                let valid = match key {
                    "ArrowLeft" => dx < 0,
                    "ArrowRight" => dx > 0,
                    "ArrowUp" => dy < 0,
                    "ArrowDown" => dy > 0,
                    _ => false,
                };
                valid.then_some((id, dx * dx + dy * dy))
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(id, _)| id.as_str())
    }
    fn directional(&mut self, key: &str, rects: &[(String, Rect)]) {
        if let Some(id) = self.directional_target(key, rects).map(str::to_owned) {
            self.focus(id);
        }
    }
    fn form_workspace(&self, target: Option<&(SidebarTarget, u64)>) -> Result<Workspace> {
        if let Some((SidebarTarget::Workspace(id), revision)) = target {
            let mut workspace = self
                .registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == *id)
                .context("selected workspace no longer exists")?
                .clone();
            workspace.revision = *revision;
            Ok(workspace)
        } else {
            bail!("workspace was unavailable when the form opened; reopen the form")
        }
    }
    fn form_window(&self, target: Option<&(SidebarTarget, u64)>) -> Result<(String, Window)> {
        let Some((SidebarTarget::Window { workspace, window }, revision)) = target else {
            bail!("window was unavailable when the form opened; reopen the form");
        };
        let mut document = self
            .registry
            .workspaces
            .iter()
            .find(|entry| entry.id == *workspace)
            .and_then(|entry| entry.windows.iter().find(|entry| entry.id == *window))
            .cloned()
            .context("selected window no longer exists")?;
        document.revision = *revision;
        Ok((workspace.clone(), document))
    }
    fn form_pane(&self, target: Option<&(SidebarTarget, u64)>) -> Result<String> {
        match target {
            Some((SidebarTarget::Pane { pane, .. }, _)) => Ok(pane.clone()),
            Some((SidebarTarget::Workspace(_) | SidebarTarget::Window { .. }, _)) => {
                bail!("selected item is not a terminal")
            }
            None => self
                .state
                .form
                .as_ref()
                .and_then(|form| form.restore.clone())
                .context("original terminal is unavailable"),
        }
    }
    fn edit_sidebar(&mut self, close: bool) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot rename or close items");
        }
        let target = self
            .state
            .sidebar
            .cursor
            .clone()
            .context("no selected sidebar item")?;
        let workspace = self
            .registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target.workspace())
            .context("selected workspace no longer exists")?;
        let mut revision = workspace.revision;
        let (kind, value) = match &target {
            SidebarTarget::Workspace(_) if close => (FormKind::CloseWorkspace, String::new()),
            SidebarTarget::Workspace(_) => (FormKind::RenameWorkspace, workspace.name.clone()),
            SidebarTarget::Window { window, .. } => {
                let window = workspace
                    .windows
                    .iter()
                    .find(|entry| entry.id == *window)
                    .context("selected window no longer exists")?;
                revision = window.revision;
                if close {
                    (FormKind::CloseWindow, String::new())
                } else {
                    (FormKind::RenameWindow, window.name.clone())
                }
            }
            SidebarTarget::Pane { pane, .. } => {
                let pane = self
                    .registry
                    .panes
                    .iter()
                    .find(|entry| entry.id == *pane)
                    .context("selected terminal no longer exists")?;
                if pane.workspace_id.as_ref() != Some(&workspace.id) {
                    bail!("selected terminal no longer belongs to this workspace");
                }
                revision = workspace
                    .windows
                    .iter()
                    .find(|window| pane.window_id.as_ref() == Some(&window.id))
                    .context("selected terminal's owning window no longer exists")?
                    .revision;
                if close {
                    (FormKind::TerminatePane, String::new())
                } else {
                    (FormKind::RenamePane, pane.title.clone())
                }
            }
        };
        self.open_form(kind, value)?;
        self.state.form.as_mut().unwrap().target = Some((target, revision));
        Ok(())
    }
    fn activate_sidebar(&mut self, target: SidebarTarget) -> Result<()> {
        match target {
            SidebarTarget::Workspace(id) => self.switch(id),
            SidebarTarget::Window { workspace, window } => {
                if !self.registry.workspaces.iter().any(|entry| {
                    entry.id == workspace && entry.windows.iter().any(|entry| entry.id == window)
                }) {
                    bail!("selected window no longer exists");
                }
                if self.state.workspace.as_ref() != Some(&workspace) {
                    self.switch(workspace);
                }
                self.switch_window(window);
            }
            SidebarTarget::Pane { pane, .. } => self.show_existing(pane)?,
        }
        self.leave_sidebar();
        Ok(())
    }
    fn tree_dispatch_action(action: crate::keyguide::Action) -> bool {
        use crate::keyguide::Action;
        matches!(
            action,
            Action::ActivateSelected
                | Action::CreateWorkspace
                | Action::CreateTerminal
                | Action::RenameSelected
                | Action::CloseSelected
                | Action::ReopenPane
                | Action::SplitHorizontal
                | Action::SplitVertical
                | Action::RemovePaneFromLayout
                | Action::MovePane(_)
                | Action::FocusPane(_)
                | Action::PreviousWorkspace
                | Action::NextWorkspace
                | Action::NextPane
                | Action::StartResize
                | Action::OpenFinder
                | Action::ProjectDetails
                | Action::ReconnectSession
        )
    }
    pub(super) fn dispatch_tree_action(
        &mut self,
        target: Option<&SidebarTarget>,
        action: crate::keyguide::Action,
        size: Size,
    ) -> Result<()> {
        use crate::keyguide::Action;
        if let Some(target) = target {
            if !self
                .registry
                .workspaces
                .iter()
                .any(|workspace| workspace.id == target.workspace())
            {
                bail!("selected workspace no longer exists");
            }
            self.state.switch(target.workspace().into(), &self.registry);
            match target {
                SidebarTarget::Workspace(_) => {}
                SidebarTarget::Window { window, .. } => {
                    if !self.state.current(&self.registry).is_some_and(|workspace| {
                        workspace.windows.iter().any(|entry| entry.id == *window)
                    }) {
                        bail!("selected window no longer exists");
                    }
                    self.state.switch_window(window.clone(), &self.registry);
                }
                SidebarTarget::Pane { workspace, pane } => {
                    let pane = self
                        .registry
                        .panes
                        .iter()
                        .find(|entry| {
                            entry.id == *pane && entry.workspace_id.as_ref() == Some(workspace)
                        })
                        .context("selected terminal no longer exists")?;
                    let window = pane
                        .window_id
                        .clone()
                        .context("selected terminal has no window")?;
                    if !self.state.current(&self.registry).is_some_and(|workspace| {
                        workspace.windows.iter().any(|entry| entry.id == window)
                    }) {
                        bail!("selected terminal's owning window no longer exists");
                    }
                    self.state.switch_window(window, &self.registry);
                    self.state.focus(pane.id.clone());
                }
            }
            self.state.sidebar.select(target.clone());
            self.selection_epoch = self.selection_epoch.wrapping_add(1);
        }
        match action {
            Action::ActivateSelected => {
                let target = target.context("no selected native tree target")?;
                self.activate_sidebar(target.clone())?;
            }
            Action::RenameSelected => self.edit_sidebar(false)?,
            Action::CloseSelected => self.edit_sidebar(true)?,
            Action::CreateWorkspace => self.open_form(FormKind::CreateWorkspace, String::new())?,
            Action::CreateTerminal => self.open_form(FormKind::CreateWindow, String::new())?,
            Action::ProjectDetails => self.open_form(FormKind::ProjectDetails, String::new())?,
            Action::ReopenPane => self.confirm_reopen()?,
            Action::SplitHorizontal => self.spawn(size, Axis::Horizontal)?,
            Action::SplitVertical => self.spawn(size, Axis::Vertical)?,
            Action::RemovePaneFromLayout => {
                if self.options.read_only {
                    bail!("read-only client cannot change layouts");
                }
                let workspace = self
                    .state
                    .current(&self.registry)
                    .context("no workspace")?
                    .id
                    .clone();
                let window = self
                    .state
                    .current_window(&self.registry)
                    .cloned()
                    .context("no window")?;
                let pane = self
                    .state
                    .focused
                    .as_deref()
                    .context("no focused terminal")?;
                self.operation(Operation::LayoutUpdate {
                    workspace_id: workspace,
                    window_id: window.id,
                    expected_revision: window.revision,
                    layout: remove_leaf(window.layout, pane),
                })?;
            }
            Action::MovePane(direction) => self.move_directional(direction.key(), size)?,
            Action::FocusPane(direction) => {
                let rects = self.rects(size);
                self.directional(direction.key(), &rects);
            }
            Action::PreviousWorkspace | Action::NextWorkspace => {
                self.retire_resize("workspace focus changed");
                self.selection_epoch = self.selection_epoch.wrapping_add(1);
                self.state.cycle_workspace(
                    &self.registry,
                    if action == Action::PreviousWorkspace {
                        -1
                    } else {
                        1
                    },
                );
                self.select_current_lease();
            }
            Action::NextPane => {
                self.retire_resize("pane focus changed");
                self.selection_epoch = self.selection_epoch.wrapping_add(1);
                self.state.cycle_pane(&self.registry);
                self.select_current_lease();
            }
            Action::StartResize => self.begin_keyboard_resize(size)?,
            Action::OpenFinder => self.begin_finder(),
            _ => bail!("action is not available for a native tree target"),
        }
        Ok(())
    }
    fn sidebar_key(&mut self, event: &KeyEvent, size: Size) -> Result<bool> {
        use crate::keyguide::{Action, Context, Navigation};
        if self.state.prefix {
            return Ok(false);
        }
        let context = if self.state.sidebar.active {
            self.guide_context()
        } else {
            Context::Application
        };
        let action = self.binding_action(context, event)?;
        if action == Some(Action::ToggleSidebar) {
            if self.state.sidebar.active {
                self.leave_sidebar();
            } else {
                self.enter_sidebar();
            }
            self.state.suppressed.insert(event.key.clone());
            return Ok(true);
        }
        if !self.state.sidebar.active
            || matches!(action, Some(Action::CommandPrefix | Action::Detach))
        {
            return Ok(false);
        }
        self.state.suppressed.insert(event.key.clone());
        self.refresh_guide(size);
        let page = (self.state.sidebar_rows(size, self.guide_rows(size)) / 2).max(1) as isize;
        match action {
            Some(Action::LeaveSidebar) => self.leave_sidebar(),
            Some(Action::EnterSessions) => self.enter_sessions(),
            Some(Action::Navigate(direction)) => {
                self.tree.lock().unwrap().navigate(match direction {
                    Navigation::Previous => -1,
                    Navigation::Next => 1,
                    Navigation::First => isize::MIN,
                    Navigation::Last => isize::MAX,
                    Navigation::PagePrevious => -page,
                    Navigation::PageNext => page,
                });
            }
            Some(Action::FoldSidebar) => self.tree.lock().unwrap().fold(Some(true)),
            Some(Action::UnfoldSidebar) => self.tree.lock().unwrap().fold(Some(false)),
            Some(Action::ToggleFoldSidebar) => self.tree.lock().unwrap().fold(None),
            Some(Action::RefreshCi) => self.refresh_popup_ci(None),
            Some(
                action @ (Action::ActivateSelected
                | Action::CreateWorkspace
                | Action::CreateTerminal
                | Action::RenameSelected
                | Action::CloseSelected
                | Action::OpenFinder
                | Action::ReconnectSession),
            ) => self.emit_tree_action(action, size),
            _ => {}
        }
        self.synchronize_tree_focus();
        Ok(true)
    }
    fn move_directional(&mut self, key: &str, size: Size) -> Result<()> {
        if self.options.read_only {
            bail!("read-only client cannot rearrange panes");
        }
        let rects = self.rects(size);
        let Some(target) = self.directional_target(key, &rects) else {
            self.notify("No pane in that direction; layout unchanged".into());
            return Ok(());
        };
        let workspace = self.state.current(&self.registry).context("no workspace")?;
        let window = self
            .state
            .current_window(&self.registry)
            .context("no window")?;
        if self.jobs.contains(&format!("manage:window:{}", window.id)) {
            bail!("Pane move waiting for pending window management; not queued");
        }
        let focused = self.state.focused.as_deref().context("no focused pane")?;
        let mut layout = window.layout.clone().context("window has no layout")?;
        exchange_leaves(&mut layout, focused, target);
        let operation = Operation::LayoutUpdate {
            workspace_id: workspace.id.clone(),
            window_id: window.id.clone(),
            expected_revision: window.revision,
            layout: Some(layout),
        };
        self.retire_resize("pane rearrangement");
        self.selection_focus_lost();
        self.operation(operation)
    }
    fn dispatch_key(
        &mut self,
        event: KeyEvent,
        out: &mut impl Write,
        size: Size,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        let local = self.state.prefix
            || self.state.form.is_some()
            || self.picker_open()
            || self.state.sidebar.active
            || self.focused_history()
            || self.resize_capturing()
            || self.search_open();
        match self.key(event, out, size, rects) {
            Err(error) if error.downcast_ref::<updates::InputRejected>().is_some() => {
                self.scope_input_result(Err(error))?;
                Ok(false)
            }
            Err(error) if local => {
                self.notify(format!("{error:#}"));
                Ok(false)
            }
            result => result,
        }
    }
    fn key(
        &mut self,
        event: KeyEvent,
        out: &mut impl Write,
        size: Size,
        rects: &[(String, Rect)],
    ) -> Result<bool> {
        match event.action {
            KeyAction::Release if self.state.suppressed.remove(&event.key) => {
                self.search_release(&event.key);
                return Ok(false);
            }
            KeyAction::Repeat
                if self.state.suppressed.contains(&event.key)
                    && !self.local_repeat(&event)
                    && !self.search_repeat(&event.key) =>
            {
                return Ok(false);
            }
            KeyAction::Press => {
                self.cancel_held_key(&event.key)?;
                if self.held.get(&event.key).is_some_and(|held| held.pending) {
                    self.notify("Previous key release is awaiting input admission".into());
                    return Ok(false);
                }
                self.state.suppressed.remove(&event.key);
            }
            _ => {}
        }
        if event.action == KeyAction::Release {
            self.search_release(&event.key);
            if let Some(held) = self.held.get_mut(&event.key) {
                held.release = event;
                held.pending = true;
            }
            self.flush_key_releases()?;
            return Ok(false);
        }
        if event.action == KeyAction::Repeat
            && self.held.get(&event.key).is_some_and(|held| held.pending)
        {
            return Ok(false);
        }
        let was_resizing = self.resize_capturing();
        self.validate_resize(size);
        if was_resizing && !self.resize_capturing() {
            if !self.held.contains_key(&event.key) {
                self.state.suppressed.insert(event.key);
            }
            return Ok(false);
        }
        if self.guide_context() == crate::keyguide::Context::Resize
            && self.resize_key(&event, size)?
        {
            return Ok(false);
        }
        if self.state.prefix {
            // Kitty reports modifier key transitions separately from the key
            // carrying the command's text (e.g. ShiftLeft before '%').
            // Neither those transitions nor repeats consume the prefix.
            if event.action != KeyAction::Press {
                return Ok(false);
            }
            if matches!(
                event.key.as_str(),
                "ShiftLeft"
                    | "ShiftRight"
                    | "ControlLeft"
                    | "ControlRight"
                    | "AltLeft"
                    | "AltRight"
                    | "MetaLeft"
                    | "MetaRight"
                    | "SuperLeft"
                    | "SuperRight"
                    | "HyperLeft"
                    | "HyperRight"
                    | "CapsLock"
                    | "NumLock"
                    | "ScrollLock"
            ) {
                if !self.held.contains_key(&event.key) {
                    self.state.suppressed.insert(event.key);
                }
                return Ok(false);
            }
        }
        self.validate_sidebar_resize(size);
        if matches!(
            self.pointer_capture,
            Some(PointerCapture::SidebarResize { .. })
        ) {
            if event.key == "Escape" {
                self.pointer_capture = Some(PointerCapture::Ui);
            }
            if !self.held.contains_key(&event.key) {
                self.state.suppressed.insert(event.key);
            }
            return Ok(false);
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| !consent.state.sent && !consent.state.allowed)
        {
            use crate::keyguide::{self, Action, Context};
            self.state.suppressed.insert(event.key.clone());
            match keyguide::lookup(Context::ClipboardConsent, &event) {
                Some(Action::AllowClipboard) => {
                    self.consent.as_mut().unwrap().state.allowed = true;
                    self.begin_consent(out)?;
                }
                Some(Action::DenyClipboard) => self.finish_consent(ClipboardResponse::denied())?,
                _ => {}
            }
            return Ok(false);
        }
        if self.state.form.is_some() || self.picker_open() {
            if self.state.prefix {
                self.state.prefix = false;
                self.state.suppressed.insert(event.key.clone());
                if self.binding_action(crate::keyguide::Context::CommandPrefix, &event)?
                    == Some(crate::keyguide::Action::ToggleChrome)
                {
                    self.toggle_chrome();
                }
                return Ok(false);
            }
            if event.action == KeyAction::Press && event.key == "KeyB" && event.modifiers & 2 != 0 {
                self.state.prefix = true;
                self.state.suppressed.insert(event.key);
                return Ok(false);
            }
        }
        if self.overlays.escape_target() == Some("ui:modal")
            && crate::keyguide::lookup(self.guide_context(), &event)
                == Some(crate::keyguide::Action::Cancel)
        {
            self.state.suppressed.insert(event.key.clone());
            self.cancel_modal();
            return Ok(false);
        }
        if self.state.form.is_some() {
            use crate::keyguide::{self, Action, Context, Navigation};
            self.selection_focus_lost();
            self.state.suppressed.insert(event.key.clone());
            let form = self.state.form.as_ref().unwrap();
            let context = form.context();
            let action = keyguide::lookup(context, &event);
            if action == Some(Action::Cancel) {
                self.cancel_modal();
                return Ok(false);
            }
            if form.pending.is_some() || form.ambiguous {
                return Ok(false);
            }
            if action == Some(Action::Submit) {
                if context == Context::Confirmation {
                    self.state.form.as_mut().unwrap().confirmed = true;
                }
                if let Err(error) = self.apply_form(size) {
                    let message = format!("{error:#}");
                    if let Some(form) = &mut self.state.form {
                        form.error = Some(message);
                    }
                }
                return Ok(false);
            }
            let form_size = self.modal_geometry(size).1.size(size);
            let form = self.state.form.as_mut().unwrap();
            if let Some(Action::Navigate(navigation)) = action {
                let count = match form.kind {
                    FormKind::ProjectDetails => {
                        project_detail_lines(form_size, form, &self.registry).len()
                    }
                    FormKind::ReopenPane { .. } => 2,
                    _ => 0,
                };
                let last = count.saturating_sub(1);
                let page = form_size.rows.saturating_sub(4).max(1) as usize;
                form.selection = match navigation {
                    Navigation::Previous => form.selection.saturating_sub(1),
                    Navigation::Next => form.selection.saturating_add(1).min(last),
                    Navigation::First => 0,
                    Navigation::Last => last,
                    Navigation::PagePrevious => form.selection.saturating_sub(page),
                    Navigation::PageNext => form.selection.saturating_add(page).min(last),
                };
            } else if matches!(context, Context::Create | Context::Edit) {
                form.error = None;
                form.fields.edit(&event, context == Context::Create);
            }
            return Ok(false);
        }
        if self.theme_picker.is_some() {
            let local = self.modal_geometry(size).1.size(size);
            let intent = self
                .theme_picker
                .as_mut()
                .unwrap()
                .list
                .handle_key(&event, local);
            self.state.suppressed.insert(event.key.clone());
            self.theme_picker_intent(intent);
            return Ok(false);
        }
        if self.finder.is_some() {
            let (_, content) = self.modal_geometry(size);
            let intent = self
                .finder
                .as_mut()
                .unwrap()
                .handle_key(&event, content.size(size));
            self.state.suppressed.insert(event.key.clone());
            self.finder_intent(intent)?;
            return Ok(false);
        }
        if self.search_open()
            && !self.state.prefix
            && !(event.action == KeyAction::Press
                && event.key == "KeyB"
                && event.modifiers & 2 != 0)
            && self.search_key(&event)
        {
            return Ok(false);
        }
        if self.sidebar_key(&event, size)? {
            return Ok(false);
        }
        if event.action == KeyAction::Press && event.key == "KeyB" && event.modifiers & 2 != 0 {
            self.state.suppressed.insert(event.key);
            if self.state.prefix {
                self.state.prefix = false;
                if !self.options.read_only
                    && !self.state.sidebar.active
                    && !self.search_open()
                    && let Some((pane, view)) = self.active()
                {
                    if self.selection_active(&pane) {
                        self.clear_selection(&pane)?;
                    }
                    for action in [KeyAction::Press, KeyAction::Release] {
                        self.enqueue(
                            &pane,
                            PaneRequest::Key {
                                view_id: view.clone(),
                                event: KeyEvent {
                                    key: "KeyB".into(),
                                    modifiers: 2,
                                    text: if action == KeyAction::Press {
                                        "b".into()
                                    } else {
                                        String::new()
                                    },
                                    action,
                                    unshifted_codepoint: 98,
                                },
                            },
                        )?;
                    }
                }
            } else {
                self.state.prefix = true;
            }
            return Ok(false);
        }
        if self.state.prefix {
            if event.action != KeyAction::Press {
                return Ok(false);
            }
            self.state.prefix = false;
            self.state.suppressed.insert(event.key.clone());
            use crate::keyguide::{Action, Context};
            let action = self.binding_action(Context::CommandPrefix, &event)?;
            if !matches!(action, Some(Action::StartSearch | Action::CopySelection)) {
                self.close_search();
            }
            if self.state.sidebar.active
                && let Some(action) = action
                && Self::tree_dispatch_action(action)
            {
                self.emit_tree_action(action, size);
                return Ok(false);
            }
            match action {
                Some(Action::Detach) => return Ok(true),
                Some(Action::ToggleChrome) => self.toggle_chrome(),
                Some(Action::EnterSessions) => self.enter_sessions(),
                Some(Action::OpenThemes) => self.begin_theme_picker(),
                Some(Action::StartSearch) => self.begin_search()?,
                Some(Action::StartHistory) => self.enter_history(rects)?,
                Some(Action::StartResize) => self.begin_keyboard_resize(size)?,
                Some(Action::CopySelection) => self.copy_focused_selection()?,
                Some(Action::ProjectDetails) => {
                    self.open_form(FormKind::ProjectDetails, String::new())?
                }
                Some(Action::ReopenPane) => self.confirm_reopen()?,
                Some(Action::SplitHorizontal) => self.spawn(size, Axis::Horizontal)?,
                Some(Action::SplitVertical) => self.spawn(size, Axis::Vertical)?,
                Some(Action::RemovePaneFromLayout) => {
                    if self.options.read_only {
                        bail!("read-only client cannot change layouts");
                    }
                    let workspace = self
                        .state
                        .current(&self.registry)
                        .context("no workspace")?
                        .clone();
                    let window = self
                        .state
                        .current_window(&self.registry)
                        .cloned()
                        .context("no window")?;
                    let target = self.state.focused.as_deref().context("no focused pane")?;
                    let layout = remove_leaf(window.layout, target);
                    self.operation(Operation::LayoutUpdate {
                        workspace_id: workspace.id,
                        window_id: window.id,
                        expected_revision: window.revision,
                        layout,
                    })?;
                }
                Some(Action::PreviousWorkspace) => {
                    self.retire_resize("workspace focus changed");
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_workspace(&self.registry, -1);
                    self.select_current_lease();
                }
                Some(Action::NextWorkspace) => {
                    self.retire_resize("workspace focus changed");
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_workspace(&self.registry, 1);
                    self.select_current_lease();
                }
                Some(Action::NextPane) => {
                    self.retire_resize("pane focus changed");
                    self.selection_epoch = self.selection_epoch.wrapping_add(1);
                    self.state.cycle_pane(&self.registry);
                    self.select_current_lease();
                }
                Some(Action::MovePane(direction)) => {
                    self.move_directional(direction.key(), size)?
                }
                Some(Action::FocusPane(direction)) => self.directional(direction.key(), rects),
                Some(Action::Cancel) => {}
                _ => self.notify("Unknown Ctrl-b command; press Ctrl-b for bindings".into()),
            };
            if self.state.form.is_some() {
                self.retire_resize("modal opened");
                self.selection_focus_lost();
                self.close_search();
            }
            return Ok(false);
        }
        if self.selection_key(&event)? {
            return Ok(false);
        }
        if self.history_key(&event, rects)? {
            return Ok(false);
        }
        if !self.options.read_only {
            self.notify_focus()?;
            let target = if event.action == KeyAction::Repeat {
                self.held
                    .get(&event.key)
                    .map(|held| (held.pane.clone(), held.view.clone()))
                    .or_else(|| self.active())
            } else {
                self.active()
            };
            if let Some((pane, view)) = target {
                let held = (self.key_events_report_releases && event.action == KeyAction::Press)
                    .then(|| {
                        let mut release = event.clone();
                        release.action = KeyAction::Release;
                        release.modifiers = 0;
                        (
                            event.key.clone(),
                            HeldKey {
                                pane: pane.clone(),
                                view: view.clone(),
                                release,
                                pending: false,
                            },
                        )
                    });
                self.enqueue(
                    &pane,
                    PaneRequest::Key {
                        view_id: view,
                        event,
                    },
                )?;
                if let Some((key, target)) = held {
                    self.held.insert(key, target);
                }
            } else if let Some(pane) = &self.state.focused {
                self.notify(
                    self.view_failures
                        .get(pane)
                        .cloned()
                        .or_else(|| {
                            self.input_failures.get(pane).and_then(|failures| {
                                failures
                                    .iter()
                                    .flatten()
                                    .find(|failure| failure.stopped)
                                    .map(ToString::to_string)
                            })
                        })
                        .unwrap_or_else(|| self.placeholder(pane)),
                );
            }
        }
        Ok(false)
    }
    fn mouse(
        &mut self,
        mouse: termwiz::input::MouseEvent,
        physical: Option<(MouseButton, MouseAction)>,
        size: Size,
        rects: &[(String, Rect)],
        workspace_hits: &[(TreeTarget, u16)],
        hits: &HitMap,
    ) -> Result<()> {
        self.validate_sidebar_resize(size);
        self.validate_resize(size);
        let button = if mouse.mouse_buttons.contains(MouseButtons::VERT_WHEEL) {
            if mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            }
        } else if mouse.mouse_buttons.contains(MouseButtons::HORZ_WHEEL) {
            if mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                MouseButton::WheelLeft
            } else {
                MouseButton::WheelRight
            }
        } else if mouse.mouse_buttons.contains(MouseButtons::LEFT) {
            MouseButton::Left
        } else if mouse.mouse_buttons.contains(MouseButtons::MIDDLE) {
            MouseButton::Middle
        } else if mouse.mouse_buttons.contains(MouseButtons::RIGHT) {
            MouseButton::Right
        } else {
            MouseButton::None
        };
        let wheel = matches!(
            button,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        );
        let (event_button, action) = physical.unwrap_or_else(|| {
            let action = if wheel {
                MouseAction::Press
            } else if button == MouseButton::None && self.last_button != MouseButton::None {
                MouseAction::Release
            } else if button == self.last_button {
                MouseAction::Motion
            } else {
                MouseAction::Press
            };
            let event_button = if action == MouseAction::Release {
                self.last_button
            } else {
                button
            };
            (event_button, action)
        });
        let captured = self.pointer_capture.clone();
        // Existing pane, selection, divider and UI gestures retain ownership
        // even when a new toast appears underneath their pointer.
        let phase = if wheel {
            PointerPhase::Wheel
        } else {
            match action {
                MouseAction::Press => PointerPhase::Press,
                MouseAction::Release => PointerPhase::Release,
                MouseAction::Motion => PointerPhase::Motion,
            }
        };
        let toast_route = {
            let mut toasts = self.toasts.lock().unwrap();
            if captured.is_none() || toasts.pointer_captured() {
                toasts.route_pointer(mouse.x, mouse.y, phase, event_button)
            } else {
                toast::PointerRoute::Pass
            }
        };
        if !matches!(toast_route, toast::PointerRoute::Pass) {
            if !wheel {
                self.last_button = button;
            }
            // ToastStack owns this physical gesture across session switches;
            // do not leave a session-local UI capture behind on its old App.
            self.sidebar_resize_hover = false;
            self.update_resize_hover(size, None);
            return Ok(());
        }
        let resize_point = !wheel
            && button == MouseButton::None
            && captured.is_none()
            && !self.sidebar_resize_blocked()
            && !self.state.prefix
            && !self.resize_overlay_blocked(size, mouse.x, mouse.y)
            && hits
                .hit_test(mouse.x, mouse.y)
                .is_some_and(|id| !matches!(id, "ui:modal" | "ui:search" | "ui:window"));
        self.sidebar_resize_hover =
            resize_point && self.sidebar_resize_handle(size, mouse.x, mouse.y);
        self.update_resize_hover(
            size,
            (resize_point && !self.sidebar_resize_hover).then_some((mouse.x, mouse.y)),
        );
        if !wheel {
            self.last_button = button;
            if action == MouseAction::Release {
                self.pointer_capture = None;
            } else if action == MouseAction::Press {
                self.tree_press = None;
                self.pointer_capture = Some(PointerCapture::Ui);
            }
        }
        if let Some(PointerCapture::SidebarResize { offset, .. }) = captured {
            if !wheel {
                self.tree.lock().unwrap().preferred_columns = Some(
                    (i32::from(mouse.x) - offset).clamp(3, i32::from(size.cols.saturating_sub(3)))
                        as u16,
                );
                self.synchronize_tree_focus();
                self.sidebar_resize_hover = action != MouseAction::Release
                    || self.sidebar_resize_handle(size, mouse.x, mouse.y);
            }
            return Ok(());
        }
        if matches!(&captured, Some(PointerCapture::Resize)) {
            if !wheel {
                self.resize_pointer(mouse.x, mouse.y, action == MouseAction::Release)?;
                if action == MouseAction::Release {
                    self.update_resize_hover(size, Some((mouse.x, mouse.y)));
                }
            }
            return Ok(());
        }
        if !wheel && let Some(PointerCapture::Selection(origin)) = &captured {
            if let Some((_, rect)) = rects.iter().find(|(pane, _)| pane == &origin.pane) {
                let action = if action == MouseAction::Release {
                    SelectionAction::Release
                } else {
                    SelectionAction::Drag
                };
                return self.select_pointer(origin.clone(), action, mouse, content(*rect));
            }
            self.cancel_selection(&origin.pane, &origin.view);
            return Ok(());
        }
        let origin = if wheel {
            None
        } else {
            match &captured {
                Some(PointerCapture::Pane(origin)) => Some(origin.as_ref()),
                _ => None,
            }
        };
        if origin.is_none()
            && !self.sidebar_resize_blocked()
            && !self.resize_overlay_blocked(size, mouse.x, mouse.y)
            && !matches!(&captured, Some(PointerCapture::Ui))
            && button == MouseButton::Left
            && action == MouseAction::Press
            && hits
                .hit_test(mouse.x, mouse.y)
                .is_some_and(|id| !matches!(id, "ui:modal" | "ui:search" | "ui:window"))
            && (self.begin_sidebar_resize(size, mouse.x, mouse.y)
                || self.begin_divider_resize(size, mouse.x, mouse.y)?)
        {
            return Ok(());
        }
        if origin.is_none() {
            let phase = if wheel {
                PointerPhase::Wheel
            } else {
                match action {
                    MouseAction::Press => PointerPhase::Press,
                    MouseAction::Release => PointerPhase::Release,
                    MouseAction::Motion => PointerPhase::Motion,
                }
            };
            let route = self.overlays.route_pointer(mouse.x, mouse.y, phase);
            if !matches!(
                route,
                PointerRoute::Target {
                    id: crate::popover::SURFACE_ID,
                    ..
                }
            ) && mouse.x >= self.screen_geometry(size).0.width
            {
                self.clear_tree_pointer();
            }
            match route {
                PointerRoute::Dismiss("ui:modal") => {
                    self.cancel_modal();
                    return Ok(());
                }
                PointerRoute::Target {
                    id: "ui:modal",
                    content: true,
                } if self.theme_picker.is_some()
                    && !self
                        .consent
                        .as_ref()
                        .is_some_and(|consent| !consent.state.allowed && !consent.state.sent) =>
                {
                    let rect = self.overlays.layer("ui:modal").unwrap().placement.content;
                    let event = MouseEvent {
                        x: f32::from(mouse.x - rect.x),
                        y: f32::from(mouse.y - rect.y),
                        button: event_button,
                        action,
                        modifiers: u16::from(selection::mouse_modifiers(mouse.modifiers)),
                    };
                    let intent = self
                        .theme_picker
                        .as_mut()
                        .unwrap()
                        .list
                        .handle_mouse(&event, rect.size(size));
                    self.theme_picker_intent(intent);
                    return Ok(());
                }
                PointerRoute::Target {
                    id: "ui:modal",
                    content: true,
                } if self.finder.is_some()
                    && !self
                        .consent
                        .as_ref()
                        .is_some_and(|consent| !consent.state.allowed && !consent.state.sent) =>
                {
                    let rect = self.overlays.layer("ui:modal").unwrap().placement.content;
                    let event = MouseEvent {
                        x: f32::from(mouse.x - rect.x),
                        y: f32::from(mouse.y - rect.y),
                        button: event_button,
                        action,
                        modifiers: u16::from(selection::mouse_modifiers(mouse.modifiers)),
                    };
                    let intent = self
                        .finder
                        .as_mut()
                        .unwrap()
                        .handle_mouse(&event, rect.size(size));
                    self.finder_intent(intent)?;
                    return Ok(());
                }
                PointerRoute::Target {
                    id: crate::popover::SURFACE_ID,
                    ..
                } => {
                    match button {
                        MouseButton::WheelUp => self.popover.scroll_by(-3),
                        MouseButton::WheelDown => self.popover.scroll_by(3),
                        MouseButton::Left if action == MouseAction::Press => {
                            self.refresh_popup_ci(Some((mouse.x, mouse.y)))
                        }
                        _ => {}
                    }
                    return Ok(());
                }
                PointerRoute::Pass => {}
                _ => return Ok(()),
            }
        }
        if matches!(&captured, Some(PointerCapture::Ui)) && !wheel {
            if action == MouseAction::Release
                && let Some(target) = self.tree_press.take()
                && mouse.x < self.screen_geometry(size).0.width
                && workspace_hits
                    .iter()
                    .any(|(hit, row)| *row == mouse.y && hit == &target)
            {
                let accepted = {
                    let mut tree = self.tree.lock().unwrap();
                    if tree
                        .rows
                        .iter()
                        .any(|row| row.target.as_ref() == Some(&target))
                    {
                        tree.enter();
                        let double = tree.click(target.clone(), Instant::now());
                        if double {
                            tree.fold(None);
                        }
                        true
                    } else {
                        false
                    }
                };
                self.synchronize_tree_focus();
                if accepted {
                    self.session_action = Some(SessionAction::Tree {
                        target,
                        action: crate::keyguide::Action::ActivateSelected,
                        size,
                    });
                }
            }
            return Ok(());
        }
        let modal = self.state.form.is_some()
            || self.picker_open()
            || self
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent);
        if modal && !(origin.is_some() && action == MouseAction::Release) {
            return Ok(());
        }
        if origin.is_none()
            && hits
                .hit_test(mouse.x, mouse.y)
                .is_none_or(|id| matches!(id, "ui:modal" | "ui:search" | "ui:guide" | "ui:window"))
        {
            return Ok(());
        }
        let (sidebar, _) = self.screen_geometry(size);
        if origin.is_none() && mouse.x < sidebar.width {
            let target = workspace_hits
                .iter()
                .find(|(_, row)| *row == mouse.y)
                .map(|(target, _)| target);
            {
                let mut tree = self.tree.lock().unwrap();
                tree.pointer_row = Some(mouse.y);
                tree.hover(target);
                if matches!(button, MouseButton::WheelUp | MouseButton::WheelDown) {
                    tree.scroll_by(if button == MouseButton::WheelUp {
                        -3
                    } else {
                        3
                    });
                }
            }
            if event_button == MouseButton::Left
                && action == MouseAction::Press
                && captured.is_none()
            {
                self.tree_press = target.cloned();
            }
            self.synchronize_tree_focus();
            return Ok(());
        }
        if origin.is_none() {
            self.clear_tree_pointer();
        }
        let target = if let Some(origin) = origin {
            rects.iter().find(|(pane, _)| *pane == origin.pane)
        } else {
            rects
                .iter()
                .find(|(_, rect)| rect.contains(mouse.x, mouse.y))
        };
        let Some((pane, rect)) = target else {
            return Ok(());
        };
        if origin.is_none()
            && action == MouseAction::Motion
            && event_button == MouseButton::None
            && (self.options.read_only
                || self.views.get(pane).is_some_and(|view| {
                    view.metadata.capabilities.native_mouse_reporting
                        && view
                            .frame
                            .as_ref()
                            .is_some_and(|frame| !frame.mouse_reporting)
                }))
        {
            return Ok(());
        }
        if origin.is_none() && button == MouseButton::Left && action == MouseAction::Press {
            self.leave_sidebar();
            self.focus(pane.clone());
        }
        self.notify_focus()?;
        if !(self.input_enabled(pane) || origin.is_some() && action == MouseAction::Release) {
            self.notify(self.placeholder(pane));
            return Ok(());
        }
        let rect = content(*rect);
        if rect.width == 0
            || rect.height == 0
            || (origin.is_none() && !rect.contains(mouse.x, mouse.y))
        {
            return Ok(());
        }
        let Some(attached) = self.views.get(pane) else {
            return Ok(());
        };
        if origin.is_some_and(|origin| origin.view != attached.view) {
            return Ok(());
        }
        let Some(frame) = attached.frame.as_ref() else {
            return Ok(());
        };
        let vertical = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
        let shifted = mouse.modifiers.contains(Modifiers::SHIFT);
        if vertical
            && (shifted
                || self.search_open() && !frame.alternate_screen
                || attached.metadata.capabilities.native_mouse_reporting
                    && !frame.mouse_reporting
                    && !frame.alternate_screen)
        {
            let delta = if button == MouseButton::WheelUp {
                3
            } else {
                -3
            };
            let enter = shifted || delta > 0 || self.history(pane).is_some();
            return self.scroll_history(pane, delta, enter);
        }
        if !wheel
            && event_button == MouseButton::Left
            && action == MouseAction::Press
            && (shifted
                || self.history(pane).is_some()
                || self.search_open()
                || attached.metadata.capabilities.native_mouse_reporting && !frame.mouse_reporting)
        {
            let origin = Arc::new(PointerOrigin {
                pane: pane.clone(),
                view: attached.view.clone(),
            });
            return self.select_pointer(origin, SelectionAction::Press, mouse, rect);
        }
        // Historical cells cannot start a new application gesture. Complete
        // accepted releases and preserve application-owned wheel reporting.
        if self.options.read_only
            || (self.search_open() && !(origin.is_some() && action == MouseAction::Release))
            || (!wheel
                && self.history(pane).is_some()
                && !(origin.is_some() && action == MouseAction::Release))
            || !self
                .registry
                .panes
                .iter()
                .any(|info| info.id == *pane && info.status == PaneStatus::Running)
        {
            return Ok(());
        }
        // A pinned or asynchronously resizing native pane can be smaller than
        // its client rectangle. Complete captured gestures at the last actual
        // native cell rather than losing their release in the letterbox.
        let cols = if origin.is_some() {
            rect.width.min(frame.size.cols)
        } else {
            rect.width
        };
        let rows = if origin.is_some() {
            rect.height.min(frame.size.rows)
        } else {
            rect.height
        };
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        let col = mouse.x.saturating_sub(rect.x).min(cols - 1);
        let row = mouse.y.saturating_sub(rect.y).min(rows - 1);
        let Some((x, y)) = mouse_position(col, row, rect.size(size), frame.size) else {
            return Ok(());
        };
        let view = &attached.view;
        let modifiers = selection::mouse_modifiers(mouse.modifiers);
        let event = MouseEvent {
            x,
            y,
            button: event_button,
            action,
            modifiers: u16::from(modifiers),
        };
        let deferred = self
            .barrier_target()
            .is_some_and(|(target_pane, target_view)| target_pane == pane && target_view == view);
        if let Err(error) = self.enqueue(
            pane,
            PaneRequest::Mouse {
                view_id: view.clone(),
                event,
            },
        ) {
            if origin.is_some() && action == MouseAction::Release {
                self.escrow_pointer_release(pane, view, event);
            }
            return Err(error);
        }
        let pointer = if deferred {
            None
        } else {
            self.pointer_origin(pane, view, event)
        };
        if !wheel && action == MouseAction::Press {
            let origin = pointer.clone().unwrap_or_else(|| {
                Arc::new(PointerOrigin {
                    pane: pane.clone(),
                    view: view.clone(),
                })
            });
            self.pointer_capture = Some(PointerCapture::Pane(origin));
        }
        if let Some(origin) = pointer {
            self.record_pointer(origin, event);
        }
        Ok(())
    }
    fn color_reply(&mut self, response: &[u8]) {
        if self.colors.theme_changed(response) {
            self.color_query_pending = true;
        }
        self.colors.feed(response);
    }

    fn flush_color_query(&mut self, out: &mut impl Write) -> Result<()> {
        if self.color_query_pending {
            out.write_all(&ColorProbe::query())?;
            out.flush()?;
            self.color_query_pending = false;
        }
        Ok(())
    }

    fn reply(
        &mut self,
        response: &[u8],
        out: &mut impl Write,
        terminal: &mut TerminalGuard,
    ) -> Result<()> {
        if let Some(graphics) = response.strip_prefix(b"graphics;") {
            if self.colors.graphics.consume_tagged(response) {
                return Ok(());
            }
            if let Some(status) = graphics.splitn(2, |b| *b == b';').nth(1)
                && status != b"OK"
            {
                bail!(
                    "physical graphics projection rejected: {}",
                    safe_text(&String::from_utf8_lossy(status))
                );
            }
            return Ok(());
        }
        if response.starts_with(b"input-error;") {
            bail!(
                "bounded physical input rejected: {}",
                String::from_utf8_lossy(response)
            );
        }
        if let Some(response) = response.strip_prefix(b"clipboard-cap;") {
            if let Some(status) = std::str::from_utf8(response)?
                .strip_prefix("?5522;")
                .and_then(|s| s.strip_suffix('$'))
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| *n <= 4)
            {
                let supported = (1..=4).contains(&status);
                self.enhanced_supported = Some(supported);
                if supported {
                    terminal.observe_enhanced_paste(matches!(status, 1 | 3));
                }
            }
            return Ok(());
        }
        if response.starts_with(b"keyboard;") {
            if !terminal.kitty_keyboard {
                write!(out, "\x1b[>31u")?;
                out.flush()?;
                terminal.kitty_keyboard = true;
            }
            return Ok(());
        }
        if response.starts_with(b"focus;") {
            return self.host_focus_changed(response == b"focus;in");
        }
        let response = terminal.observe_background_reply(response, out)?;
        let response = response.as_ref();
        terminal.theme_reply(response, out)?;
        self.color_reply(response);
        self.device_reply(response)?;
        Ok(())
    }
}

// Runs exclusively on the bounded background pool. Attach starts ineligible:
// a stale or slow initial poll must never acquire hidden size/device authority.
fn attach(socket: &Endpoint, pane: &str, mut spec: ViewSpec) -> Result<Attached> {
    spec.visible = false;
    spec.interactive = false;
    spec.clipboard = false;
    let payload = socket.checked(Operation::Pane {
        pane_id: pane.into(),
        request: PaneRequest::Attach { spec: spec.clone() },
    })?;
    let result: AttachResult = serde_json::from_value(payload)
        .map_err(|error| transport::mark_outcome_unknown(error.into()))?;
    let view = result.view_id;
    let mut pump = match updates::pane(socket, pane, &view) {
        Ok(pump) => pump,
        Err(error) => return Err(detach_failed_attach(socket, pane, &view, error)),
    };
    let Snapshot { metadata, scene } = match pump.initial() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            pump.cancel();
            let error = detach_failed_attach(socket, pane, &view, error);
            drop(pump);
            return Err(error);
        }
    };
    let clipboard_id = metadata
        .clipboard
        .as_ref()
        .map(|pending| pending.request_id.clone());
    let mut attached = Attached {
        view,
        spec,
        pump,
        metadata,
        clipboard_id,
        frame: None,
        history: false,
        revision: 0,
        images: HashMap::new(),
        seeded: 0,
        retirement_requested: false,
        retirement_policy: RetirementPolicy::Visible,
        retirement_focus_out: false,
        retirement_error: None,
    };
    if let Some(scene) = scene
        && let Err(error) = attached.scene(scene)
    {
        attached.pump.cancel();
        let error = detach_failed_attach(socket, pane, &attached.view, error);
        drop(attached);
        return Err(error);
    }
    Ok(attached)
}
fn detach_failed_attach(
    socket: &Endpoint,
    pane: &str,
    view: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match socket.checked(Operation::Pane {
        pane_id: pane.into(),
        request: PaneRequest::Detach {
            view_id: view.into(),
        },
    }) {
        Ok(_) => error.context(format!(
            "pane {pane} admitted view {view} was detached after initial snapshot failure"
        )),
        Err(cleanup) if transport::shutdown_not_dispatched(&cleanup) => error,
        Err(cleanup) => {
            let unknown =
                transport::outcome_unknown(&error) || transport::outcome_unknown(&cleanup);
            // Neither failure may inherit the other's safe cancellation marker.
            let combined = anyhow::anyhow!(
                "pane {pane} admitted view {view} initial snapshot failed: {error:#}; detach failed: {cleanup:#}"
            );
            if unknown {
                transport::mark_outcome_unknown(combined)
            } else {
                combined
            }
        }
    }
}

impl App {
    fn new(
        socket: Endpoint,
        options: Options,
        spawn_defaults: Option<SpawnDefaults>,
        registry: RegistrySnapshot,
        initial_size: Size,
        runtime: AppRuntime,
    ) -> Result<Self> {
        let AppRuntime {
            jobs,
            input,
            tree,
            toasts,
        } = runtime;
        let state = Controller::new(&registry);
        let input = match input {
            Some(input) => input,
            None => InputPump::new(&socket)?,
        };
        Ok(Self {
            socket,
            options,
            spawn_defaults,
            session_action: None,
            namespace: (0, 0),
            tree,
            tree_press: None,
            sidebar_target: None,
            sidebar_backend_available: false,
            sidebar_has_workspace: false,
            backgrounded: false,
            notification_source: None,
            registry,
            state,
            views: HashMap::new(),
            retiring: Vec::new(),
            retiring_candidates: Vec::new(),
            jobs,
            management: HashMap::new(),
            selection_epoch: 0,
            responses: Vec::new(),
            held: HashMap::new(),
            key_events_report_releases: false,
            input,
            input_failures: HashMap::new(),
            view_failures: HashMap::new(),
            consent: None,
            answered: HashMap::new(),
            deferred: RefCell::new(VecDeque::new()),
            paste_offer: None,
            paste_target: None,
            enhanced_supported: None,
            colors: ColorProbe::default(),
            color_query_pending: false,
            last_button: MouseButton::None,
            pointer_capture: None,
            sidebar_resize_hover: false,
            pressed_pointers: RefCell::new(Vec::new()),
            terminal_selection: selection::State::default(),
            resizing: resizing::State::default(),
            search: search::State::default(),
            finder: None,
            theme: crate::themes::ThemeId::default(),
            theme_picker: None,
            popover: crate::popover::Popover::new(initial_size),
            overlays: OverlayStack::default(),
            toasts,
            guide: GuideCache::default(),
            chrome_graphics_enabled: true,
            notified_focus: None,
            focus_out_pending: HashSet::new(),
            lease_pending: HashSet::new(),
            host_focused: true,
        })
    }
}

/// Run the physical terminal until detach, terminal closure or a fatal client error.
///
/// Rejects unsupported clipboard policies before entering terminal mode.
pub fn run_sessions(targets: Vec<crate::SessionTarget>, options: Options) -> Result<()> {
    anyhow::ensure!(
        matches!(options.clipboard_policy.as_str(), "ask" | "allow" | "deny"),
        "clipboard must be ask, allow or deny"
    );
    let terminal = TerminalGuard::enter()?;
    let initial_size = dimensions()?;
    let mut graphics = crate::chrome_graphics::ChromeGraphics::new()?;
    let (mut resize_read, resize_write) = std::os::unix::net::UnixStream::pair()?;
    resize_read.set_nonblocking(true)?;
    resize_write.set_nonblocking(true)?;
    struct ResizeSignal(signal_hook::SigId);
    impl Drop for ResizeSignal {
        fn drop(&mut self) {
            signal_hook::low_level::unregister(self.0);
        }
    }
    let _resize = ResizeSignal(signal_hook::low_level::pipe::register(
        signal_hook::consts::SIGWINCH,
        resize_write,
    )?);
    let mut sessions = Coordinator::new(targets, options, initial_size)?;
    let shared_tree = std::sync::Arc::clone(&sessions.tree);
    let shared_toasts = std::sync::Arc::clone(&sessions.toasts);
    struct ToastPresentation {
        id: toast::ToastId,
        style_revision: u64,
        graphical: bool,
        outer: Rect,
        body: Rect,
        frame: bkpr_protocol::Frame,
    }
    let result = (|| -> Result<()> {
        let mut terminal = terminal;
        let mut renderer = Renderer::default();
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());
        out.write_all(&sessions.app_mut().colors.graphics.begin())?;
        out.flush()?;
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let mut incoming = VecDeque::new();
        let mut scoped_panes = Vec::<String>::new();
        let mut resource_namespace = None;
        let mut last_input = Instant::now();
        let mut theme = Theme::default();
        let mut ui_palette = crate::themes::Palette::default();
        let mut project_palette = crate::project_colors::ProjectPalette::default();
        let mut chrome_projects = Vec::new();
        let mut chrome_foregrounds = Vec::new();
        let mut working_titles = Vec::new();
        let mut title_animation = SidebarTitleAnimation::new(Instant::now());
        let mut guide_theme = Theme::default();
        let mut toast_theme = Theme::default();
        let mut popover_theme = Theme::default();
        let mut guide_roles = Vec::new();
        let mut chrome_markers = Vec::new();
        let mut marker_sources = Vec::with_capacity(16);
        let mut pane_graphics = Vec::new();
        let mut next_pane_graphics = Vec::new();
        let mut chrome_surface = UiSurface::new(initial_size);
        let mut popup_surface = UiSurface::new(Size {
            cols: 1,
            rows: 1,
            ..initial_size
        });
        let mut window_surface = UiSurface::new(Size {
            cols: 1,
            rows: 1,
            ..initial_size
        });
        let mut guide_surface = UiSurface::new(Size {
            cols: 1,
            rows: 1,
            ..initial_size
        });
        let mut window_size = None;
        let mut guide_painted = None;
        let mut form_theme = Theme::default();
        let mut search_surface = UiSurface::new(initial_size);
        let mut window_effects = graphics_frame(initial_size);
        let mut popover_effects = graphics_frame(initial_size);
        let mut guide_effects = graphics_frame(initial_size);
        let mut artwork_effects = graphics_frame(initial_size);
        let mut toast_effects = graphics_frame(initial_size);
        let mut toast_frames: [Option<ToastPresentation>; 3] = std::array::from_fn(|_| None);
        let mut toast_order = [0usize; 3];
        let mut resize_feedback_surface = UiSurface::new(initial_size);
        let mut resize_feedback_regions = Vec::new();
        let mut resize_feedback_style = None;
        let mut placeholders = HashMap::<String, UiSurface>::new();
        let mut session_fds = Vec::new();
        let mut fds = Vec::new();
        loop {
            // UiSurface may retain identical text/theme inputs. Remove the last
            // transient effect before any ordinary styling/layout update.
            title_animation.restore(&mut chrome_surface.frame);
            let size = dimensions()?;
            sessions.tick(size)?;
            if sessions.should_exit() {
                return Ok(());
            }
            session_fds.clear();
            sessions.append_poll_fds(&mut session_fds);
            let session_timeout = sessions.timeout(1000);
            let (attachment, generation) = sessions.namespace();
            let active_owner = sessions.current_owner();
            let app = sessions.app_mut();
            graphics.drain()?;
            for frame in [
                &mut window_effects,
                &mut popover_effects,
                &mut guide_effects,
                &mut artwork_effects,
                &mut toast_effects,
            ] {
                frame.size = size;
                frame.images.clear();
                frame.placements.clear();
            }
            app.validate_sidebar_resize(size);
            app.validate_resize(size);
            let rects = app.rects(size);
            if resource_namespace != Some((attachment, generation))
                || scoped_panes.len() != rects.len()
                || scoped_panes
                    .iter()
                    .zip(&rects)
                    .any(|(old, (id, _))| old != id)
            {
                renderer.set_resource_namespace(
                    attachment,
                    generation,
                    rects.iter().map(|(id, _)| id.as_str()),
                );
                scoped_panes.clear();
                scoped_panes.extend(rects.iter().map(|(id, _)| id.clone()));
                resource_namespace = Some((attachment, generation));
                placeholders.clear();
            }
            app.sync_views(size, &rects)?;
            app.poll_views();
            let search = app.flush_search();
            app.scope_input_result(search)?;
            let tick = app.selection_tick();
            app.scope_input_result(tick)?;
            app.selection_clipboard(&mut out)?;
            let color_revision = app.colors.revision;
            if color_revision > 0
                && let (Some(foreground), Some(background)) =
                    (app.colors.foreground, app.colors.background)
            {
                let palette = if app.colors.palette.len() == 256 {
                    Some((0..256).map(|i| app.colors.palette[&i]).collect::<Vec<_>>())
                } else {
                    None
                };
                let colors = app
                    .views
                    .iter()
                    .filter(|(_, view)| {
                        view.seeded != color_revision
                            && view
                                .metadata
                                .size_owner
                                .as_ref()
                                .is_some_and(|owner| owner.view_id == view.view)
                    })
                    .map(|(pane, view)| (pane.clone(), view.view.clone()))
                    .collect::<Vec<_>>();
                for (pane, view) in colors {
                    if app.queue_control(
                        &pane,
                        PaneRequest::Colors {
                            view_id: view,
                            foreground,
                            background,
                            cursor: app.colors.cursor,
                            palette: palette.clone(),
                        },
                    )? {
                        app.views.get_mut(&pane).unwrap().seeded = color_revision;
                    }
                }
            }
            let clipboard = app.clipboard(&mut out);
            app.scope_input_result(clipboard)?;
            let transfer = app.paste_offer.is_some()
                || app
                    .consent
                    .as_ref()
                    .is_some_and(|consent| consent.state.paste);
            let local = app.guide_context() != crate::keyguide::Context::Application;
            terminal.set_enhanced_paste(
                &mut out,
                transfer
                    || (app.enhanced_supported == Some(true)
                        && !app.options.read_only
                        && app.options.clipboard_policy != "deny"
                        && !local),
            )?;
            app.validate_resize(size);
            app.flush_resize()?;
            app.refresh_guide(size);
            let guide_rows = app.guide_rows(size);
            ui_palette.update(&app.colors, app.theme);
            let ui_colors = ui_palette.probe();
            let pane_palette = app.theme.pane_palette(app.colors.truecolor);
            crate::appearance::update_region_theme(&mut popover_theme, ui_colors, None);
            // One presentation lock; input and Coordinator callbacks run only after release.
            let mut tree = shared_tree.lock().unwrap();
            let sidebar = screen_geometry(size, tree.preferred_columns).0;
            let available = crate::controller::sidebar_content_rows(sidebar, guide_rows);
            tree.viewport(available);
            if let Some(row) = tree.pointer_row {
                let target = row
                    .checked_sub(2)
                    .map(usize::from)
                    .filter(|index| *index < available)
                    .and_then(|index| tree.rows.get(tree.scroll + index))
                    .and_then(|entry| entry.target.clone());
                tree.hover(target.as_ref());
            }
            let consent_open = app
                .consent
                .as_ref()
                .is_some_and(|consent| !consent.state.allowed && !consent.state.sent);
            let tooltip_target = crate::controller::tree_popup_target(
                &tree,
                consent_open
                    || app.state.form.is_some()
                    || app.state.prefix
                    || app.picker_open()
                    || app.search_open(),
            );
            let workspace_hits = crate::controller::tree_chrome(
                &mut chrome_surface,
                crate::controller::TreeChromeContext {
                    native: crate::controller::ChromeContext {
                        size,
                        state: &app.state,
                        registry: &app.registry,
                        rects: &rects,
                        pane_graphics: &pane_graphics,
                        guide_rows,
                    },
                    tree: &tree,
                    active: active_owner,
                    popup: tooltip_target
                        .map(|target| (target, popover_theme.background(Role::Panel, false))),
                },
                |pane| app.badge(pane),
                |pane| app.views.get(pane).map(|view| &view.metadata),
                |pane| app.history(pane),
                &mut chrome_projects,
                &mut working_titles,
            );
            crate::appearance::update_theme(&mut theme, ui_colors);
            project_palette.update(ui_colors, tree.project_capacity());
            crate::controller::theme_tree_chrome(
                &mut chrome_surface,
                &theme,
                crate::controller::TreeChromeContext {
                    native: crate::controller::ChromeContext {
                        size,
                        state: &app.state,
                        registry: &app.registry,
                        rects: &rects,
                        pane_graphics: &pane_graphics,
                        guide_rows,
                    },
                    tree: &tree,
                    active: active_owner,
                    popup: tooltip_target
                        .map(|target| (target, popover_theme.background(Role::Panel, false))),
                },
                |pane| app.views.get(pane).map(|view| &view.metadata),
                &project_palette,
                &chrome_projects,
                &mut chrome_foregrounds,
            );
            chrome_markers.clear();
            marker_sources.clear();
            chrome_surface.frame.images.clear();
            chrome_surface.frame.placements.clear();
            for (_, artwork, col, row) in
                crate::controller::tree_artwork_rows(&tree, size, guide_rows)
            {
                let style =
                    &chrome_surface.frame.rows[usize::from(row)].cells[usize::from(col)].style;
                let source = theme.effective_ink(style);
                if let Some(source) = source
                    && !marker_sources.contains(&source)
                {
                    marker_sources.push(source);
                }
                chrome_markers.push((artwork, col, row, source));
            }
            graphics.set_enabled(app.chrome_graphics_enabled);
            graphics.prepare(ui_colors, size, &marker_sources);
            graphics.prepare_panes(size, &rects);
            next_pane_graphics.clear();
            let sidebar_width = app.screen_geometry(size).0.width;
            for (index, (_, rect)) in rects.iter().enumerate() {
                if rect.width >= 3 && rect.height >= 3 {
                    let ready = graphics.decorate_pane(
                        &mut chrome_surface.frame,
                        *rect,
                        index,
                        0x10000 + (index as u32) * 64,
                        rect.x == sidebar_width,
                    );
                    if ready {
                        next_pane_graphics.push(*rect);
                    }
                }
            }
            if pane_graphics != next_pane_graphics {
                std::mem::swap(&mut pane_graphics, &mut next_pane_graphics);
                // Readiness is display-list data, not destructive glyph edits.
                // Repaint once on transition before the single physical write.
                crate::controller::tree_chrome(
                    &mut chrome_surface,
                    crate::controller::TreeChromeContext {
                        native: crate::controller::ChromeContext {
                            size,
                            state: &app.state,
                            registry: &app.registry,
                            rects: &rects,
                            pane_graphics: &pane_graphics,
                            guide_rows,
                        },
                        tree: &tree,
                        active: active_owner,
                        popup: tooltip_target
                            .map(|target| (target, popover_theme.background(Role::Panel, false))),
                    },
                    |pane| app.badge(pane),
                    |pane| app.views.get(pane).map(|view| &view.metadata),
                    |pane| app.history(pane),
                    &mut chrome_projects,
                    &mut working_titles,
                );
                crate::controller::theme_tree_chrome(
                    &mut chrome_surface,
                    &theme,
                    crate::controller::TreeChromeContext {
                        native: crate::controller::ChromeContext {
                            size,
                            state: &app.state,
                            registry: &app.registry,
                            rects: &rects,
                            pane_graphics: &pane_graphics,
                            guide_rows,
                        },
                        tree: &tree,
                        active: active_owner,
                        popup: tooltip_target
                            .map(|target| (target, popover_theme.background(Role::Panel, false))),
                    },
                    |pane| app.views.get(pane).map(|view| &view.metadata),
                    &project_palette,
                    &chrome_projects,
                    &mut chrome_foregrounds,
                );
            }
            for &(artwork, col, row, source) in &chrome_markers {
                graphics.decorate(
                    &mut chrome_surface.frame,
                    &mut artwork_effects,
                    artwork,
                    [col, row],
                    source,
                    u32::from(row) + 1,
                );
            }
            // Native application frames below deliberately bypass the UI theme.
            placeholders.retain(|id, _| {
                rects.iter().any(|(pane, _)| pane == id)
                    && app.views.get(id).is_none_or(|view| view.frame.is_none())
            });
            for (id, rect) in &rects {
                let rect = content(*rect);
                if app.views.get(id).is_some_and(|view| view.frame.is_some())
                    || rect.height == 0
                    || rect.width == 0
                {
                    continue;
                }
                let local = rect.size(size);
                let surface = placeholders
                    .entry(id.clone())
                    .or_insert_with(|| UiSurface::new(local));
                surface.paint(
                    local,
                    vec![(
                        Rect {
                            x: 0,
                            y: 0,
                            width: local.cols,
                            height: 1,
                        },
                        app.placeholder(id),
                        false,
                    )],
                    None,
                );
                surface.apply_theme(&theme, Role::Muted, &[]);
            }
            if let Some(finder) = &mut app.finder {
                finder.update_registry(&app.registry, |pane| {
                    app.views.get(pane).map(|view| &view.metadata)
                });
            }
            let popup = consent_open || app.state.form.is_some() || app.picker_open();
            let mut placement = popup.then(|| {
                if consent_open {
                    let placement = OverlayPlacement::centered(
                        size,
                        size.cols.saturating_sub(4).clamp(1, 82),
                        size.rows.min(6),
                    );
                    (placement.outer, placement.content)
                } else {
                    app.modal_geometry(size)
                }
            });
            let mut finder_cards = None;
            let mut form_region = None;
            let mut modal_graphical = false;
            if !consent_open
                && let Some((outer, _)) = placement
                && let Some(finder) = &mut app.finder
            {
                if let Some(regions) = crate::finder::Finder::graphic_regions(outer.size(size)) {
                    let ready = regions.iter().enumerate().all(|(index, (card, body))| {
                        let absolute = |rect: Rect| Rect {
                            x: outer.x + rect.x,
                            y: outer.y + rect.y,
                            ..rect
                        };
                        graphics.decorate_region(
                            &mut window_effects,
                            absolute(*card),
                            absolute(*body),
                            1 + index as u32 * 1024,
                            (index == 0).then_some(6),
                        )
                    });
                    if ready {
                        finder_cards = Some(regions);
                        modal_graphical = true;
                    } else {
                        window_effects.images.clear();
                        window_effects.placements.clear();
                    }
                }
                finder.set_graphical(finder_cards.is_some());
                placement = Some(app.modal_geometry(size));
            }
            if !consent_open
                && let Some((outer, inner)) = placement
                && let Some(form) = &app.state.form
                && outer.width >= 40
                && outer.height >= 8
            {
                let accent = match &form.kind {
                    _ if form.error.is_some() => Some(1),
                    FormKind::RenameWorkspace | FormKind::RenameWindow | FormKind::RenamePane => {
                        Some(3)
                    }
                    FormKind::CreateWorkspace | FormKind::CreateWindow => Some(4),
                    FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow => {
                        Some(1)
                    }
                    _ => None,
                };
                if graphics.decorate_region(&mut window_effects, outer, inner, 1, accent) {
                    form_region = Some(accent);
                    modal_graphical = true;
                } else {
                    window_effects.images.clear();
                    window_effects.placements.clear();
                }
            }
            app.overlays.resize(size);
            if let Some((outer, content)) = placement {
                let return_focus = app
                    .overlays
                    .layer("ui:modal")
                    .is_none()
                    .then(|| OverlayFocus {
                        workspace: app.state.workspace.clone(),
                        window: app.state.window.clone(),
                        pane: app.state.focused.clone(),
                        sidebar: tree.active,
                        tree_target: tree.cursor.clone(),
                    });
                app.overlays.present(
                    "ui:modal",
                    OverlayPlacement { outer, content },
                    OverlayPolicy {
                        dismiss_on_outside: !consent_open,
                        dismiss_on_escape: !consent_open,
                        ..OverlayPolicy::MODAL
                    },
                    return_focus,
                );
            } else {
                app.overlays.remove("ui:modal");
            }
            if let Some((outer, inner)) = placement {
                let local = Size {
                    cols: outer.width,
                    rows: outer.height,
                    ..size
                };
                if window_size != Some(local) {
                    window_surface.paint(
                        local,
                        window::border_rows(local, "", window::Border::Rounded),
                        None,
                    );
                    window_size = Some(local);
                }
                let local = inner.size(size);
                if consent_open {
                    popup_surface.paint(
                        local,
                        vec![(
                            Rect {
                                x: 0,
                                y: 0,
                                width: local.cols,
                                height: 1,
                            },
                            "Clipboard request: y allow / n deny (pane-local)".into(),
                            false,
                        )],
                        None,
                    );
                    popup_surface.apply_theme(
                        &theme,
                        Role::Panel,
                        &[RoleSpan {
                            rect: Rect {
                                x: 0,
                                y: 0,
                                width: local.cols,
                                height: 1,
                            },
                            role: Role::Warning,
                        }],
                    );
                } else if let Some(picker) = &mut app.theme_picker {
                    picker.list.set_title(if ui_palette.override_supported() {
                        "Themes · Enter keep · Esc cancel"
                    } else {
                        "Themes · RGB unavailable; using terminal colors"
                    });
                    picker.list.paint(&mut popup_surface, local);
                    picker.list.apply_theme(&mut popup_surface, &theme);
                } else if let Some(finder) = &mut app.finder {
                    finder.paint(&mut popup_surface, local);
                    finder.apply_theme(&mut popup_surface, &theme, ui_colors, &project_palette);
                } else if let Some(form) = &app.state.form {
                    modal(&mut popup_surface, local, form, &app.registry);
                    let form_palette = if let Some(accent) = form_region {
                        crate::appearance::update_region_theme(&mut form_theme, ui_colors, accent);
                        &form_theme
                    } else {
                        &theme
                    };
                    theme_modal(&mut popup_surface, form_palette, form);
                }
                window_surface.apply_theme(
                    &theme,
                    Role::Panel,
                    &window::theme_spans(window_surface.frame.size),
                );
                window_surface.frame.images.clear();
                window_surface.frame.placements.clear();
                if app.finder.is_some() && finder_cards.is_none() {
                    material_border(
                        &mut window_surface.frame,
                        Rect {
                            x: 0,
                            y: 0,
                            width: outer.width,
                            height: outer.height,
                        },
                        true,
                        &[],
                    );
                } else if finder_cards.is_none() && form_region.is_none() {
                    modal_graphical = raised_material(
                        &graphics,
                        &mut window_surface.frame,
                        &mut window_effects,
                        outer,
                        1,
                    );
                }
            }
            let anchor = tooltip_target.and_then(|target| {
                workspace_hits
                    .iter()
                    .find(|(candidate, _)| candidate == target)
                    .map(|(_, row)| *row)
            });
            let item_band = tooltip_target.and_then(|target| {
                let mut rows = workspace_hits
                    .iter()
                    .filter(|(candidate, _)| candidate == target)
                    .map(|(_, row)| *row);
                let first = rows.next()?;
                let last = rows.next_back().unwrap_or(first);
                let sidebar = app.screen_geometry(size).0;
                Some(Rect {
                    x: sidebar.x,
                    y: first,
                    width: sidebar.width,
                    height: last.saturating_sub(first).saturating_add(1),
                })
            });
            let tooltip = app.popover.paint_tree(
                &tree,
                tooltip_target.filter(|_| anchor.is_some()),
                anchor.unwrap_or(0),
                sidebar.width,
                size,
            );
            graphics.prepare_joined(
                size,
                tooltip.zip(item_band),
                popover_theme.style(Role::Panel).background,
            );
            let mut popover_graphical = false;
            if let Some(rect) = tooltip {
                app.popover.apply_theme(&popover_theme, &project_palette);
                app.popover.surface.frame.images.clear();
                app.popover.surface.frame.placements.clear();
                if let Some((body, item)) = app.popover.body_rect().zip(item_band) {
                    let glyphs = Rect {
                        x: item.x.saturating_add(1),
                        width: item.width.saturating_sub(2),
                        ..item
                    };
                    popover_graphical = graphics.joined_ready()
                        && graphics.decorate_joined(&mut popover_effects, 1, &[body, glyphs]);
                }
                if app.popover.set_graphical(popover_graphical) {
                    app.popover.paint_tree(
                        &tree,
                        tooltip_target,
                        anchor.unwrap_or(0),
                        sidebar.width,
                        size,
                    );
                    app.popover.apply_theme(&popover_theme, &project_palette);
                }
                app.overlays.present(
                    crate::popover::SURFACE_ID,
                    OverlayPlacement::framed(rect, size),
                    OverlayPolicy::PASSIVE,
                    None,
                );
            } else {
                app.overlays.remove(crate::popover::SURFACE_ID);
            }
            let guide_rect = app.guide_geometry(size, popup);
            let mut guide_draw_rect = None;
            if let Some(rect) = guide_rect {
                graphics.retain_decoration_sources(&mut guide_effects);
                let body = Rect {
                    x: rect.x + 2,
                    y: rect.y + 1,
                    width: rect.width.saturating_sub(4),
                    height: rect.height.saturating_sub(2),
                };
                let rich = rect.width >= 8
                    && graphics.decorate_guide(
                        &mut guide_effects,
                        rect,
                        body,
                        1,
                        app.guide_accent(),
                    );
                let draw_rect = if rich { body } else { rect };
                guide_draw_rect = Some(draw_rect);
                let local = draw_rect.size(size);
                let key = (app.guide.key, local, rich);
                if guide_painted != Some(key) {
                    let (mut rows, content) = if rich {
                        (
                            Vec::new(),
                            Rect {
                                x: 0,
                                y: 0,
                                width: local.cols,
                                height: local.rows,
                            },
                        )
                    } else {
                        (
                            window::border_rows(
                                local,
                                app.guide_context().title(),
                                window::Border::Rounded,
                            ),
                            window::content(local),
                        )
                    };
                    guide_roles.clear();
                    if !rich {
                        guide_roles.extend_from_slice(&window::theme_spans(local));
                        if local.cols >= 3 && local.rows >= 3 {
                            guide_roles.push(RoleSpan {
                                rect: Rect {
                                    x: 1,
                                    y: 0,
                                    width: (unicode_width::UnicodeWidthStr::width(
                                        app.guide_context().title(),
                                    ) as u16)
                                        .min(local.cols - 2),
                                    height: 1,
                                },
                                role: Role::Title,
                            });
                        }
                    }
                    guide_roles.push(RoleSpan {
                        rect: content,
                        role: Role::Base,
                    });
                    guide_roles.extend(app.guide.content.expanded_roles.iter().map(|span| {
                        RoleSpan {
                            rect: Rect {
                                x: if rich {
                                    span.rect.x.saturating_sub(1)
                                } else {
                                    span.rect.x + content.x
                                },
                                y: span.rect.y + content.y,
                                ..span.rect
                            },
                            role: span.role,
                        }
                    }));
                    rows.extend(
                        app.guide
                            .content
                            .expanded
                            .iter()
                            .take(usize::from(content.height))
                            .enumerate()
                            .map(|(y, text)| {
                                let text = if rich {
                                    text[1..text.len() - 1].to_owned()
                                } else {
                                    text.clone()
                                };
                                (
                                    Rect {
                                        x: content.x,
                                        y: content.y + y as u16,
                                        width: content.width,
                                        height: 1,
                                    },
                                    text,
                                    false,
                                )
                            }),
                    );
                    guide_surface.paint(local, rows, None);
                    guide_painted = Some(key);
                }
                crate::appearance::update_guide_theme(
                    &mut guide_theme,
                    ui_colors,
                    app.guide_accent(),
                );
                guide_surface.apply_theme(&guide_theme, Role::Panel, &guide_roles);
                guide_surface.frame.images.clear();
                guide_surface.frame.placements.clear();
                app.overlays.present(
                    "ui:guide",
                    OverlayPlacement::framed(rect, size),
                    OverlayPolicy::PASSIVE,
                    None,
                );
            } else {
                app.overlays.remove("ui:guide");
            }
            let search_rect = rects
                .iter()
                .find(|(id, _)| app.state.focused.as_ref() == Some(id))
                .map(|(_, rect)| {
                    let body = content(*rect);
                    let bottom = rect.y + rect.height.saturating_sub(1);
                    Rect {
                        x: rect.x,
                        y: if body.y + body.height <= bottom {
                            bottom
                        } else {
                            rect.y
                        },
                        width: rect.width,
                        height: u16::from(rect.height > 1),
                    }
                });
            let searching = search_rect.is_some_and(|rect| {
                rect.height > 0 && app.search_overlay(&mut search_surface, rect.size(size))
            });
            app.theme_search(&mut search_surface, &theme);
            let feedback_regions = if app.sidebar_resize_blocked() {
                Vec::new()
            } else if app.sidebar_resize_hover
                || matches!(
                    app.pointer_capture,
                    Some(PointerCapture::SidebarResize { .. })
                )
            {
                let sidebar = app.screen_geometry(size).0;
                vec![Rect {
                    x: sidebar.width,
                    y: 0,
                    width: u16::from(sidebar.width > 0),
                    height: size.rows,
                }]
            } else {
                app.resize_feedback(size)
            };
            let mut feedback_style = theme.style(Role::Base).clone();
            feedback_style.foreground_source = ColorSource::Palette(6);
            feedback_style.foreground = ui_colors
                .palette
                .get(&6)
                .copied()
                .unwrap_or(feedback_style.foreground);
            if ui_colors.truecolor {
                feedback_style.foreground_source = ColorSource::Rgb(feedback_style.foreground);
            }
            let feedback_resized = resize_feedback_surface.frame.size != size;
            if feedback_resized {
                resize_feedback_surface = UiSurface::new(size);
            }
            if feedback_resized
                || feedback_regions != resize_feedback_regions
                || resize_feedback_style.as_ref() != Some(&feedback_style)
            {
                resize_feedback_surface.frame.foreground = chrome_surface.frame.foreground;
                resize_feedback_surface.frame.background = chrome_surface.frame.background;
                resize_feedback_surface.frame.palette.clear();
                // Only current strips are projected; every cell in them is
                // rewritten, so stale cells outside those clips stay invisible.
                window::paint_edge_feedback(
                    &mut resize_feedback_surface.frame,
                    &feedback_regions,
                    &feedback_style,
                );
                resize_feedback_regions = feedback_regions;
                resize_feedback_style = Some(feedback_style);
            }
            let sidebar_active = tree.active;
            drop(tree);
            let mut toasts = shared_toasts.lock().unwrap();
            toasts.expire(Instant::now());
            crate::appearance::update_theme(&mut toast_theme, ui_colors);
            toasts.paint(
                size,
                toast::ToastLayout {
                    max_width: 72,
                    top_margin: 1,
                    side_margin: 2,
                    gap: 1,
                    max_lines: 5,
                },
                &toast_theme,
                |slot, outer, body| {
                    outer.width >= 3
                        && outer.height >= 3
                        && graphics.decorate_window_in_clipped(
                            &mut toast_effects,
                            outer,
                            true,
                            1 + slot as u32 * 64,
                            &[body],
                        )
                },
            );
            let mut toast_count = 0;
            let mut visible_toasts = [false; 3];
            for card in toasts.cards() {
                let retained = &mut toast_frames[card.slot];
                // Toast text is immutable per ID. UiSurface has no frame
                // revision; size, material mode and Theme revision cover all
                // subsequent text/style changes without warm frame copying.
                let unchanged = retained.as_ref().is_some_and(|cached| {
                    cached.id == card.id
                        && cached.frame.size == card.surface.frame.size
                        && cached.graphical == card.graphical
                        && cached.style_revision == toast_theme.revision()
                });
                if !unchanged {
                    *retained = Some(ToastPresentation {
                        id: card.id,
                        style_revision: toast_theme.revision(),
                        graphical: card.graphical,
                        outer: card.outer,
                        body: card.body,
                        frame: card.surface.frame.clone(),
                    });
                }
                let cached = retained.as_mut().expect("visible toast presentation");
                cached.outer = card.outer;
                cached.body = card.body;
                visible_toasts[card.slot] = true;
                toast_order[toast_count] = card.slot;
                toast_count += 1;
            }
            for (cached, visible) in toast_frames.iter_mut().zip(visible_toasts) {
                if !visible {
                    *cached = None;
                }
            }
            drop(toasts);
            let faint_background = popup
                && (consent_open || app.theme_picker.is_none())
                && crate::appearance::MODAL_BACKGROUND_FAINT;
            // The application chooses the matte; the framework owns its physical
            // lifetime. Prepared requests alone are not visible pane materials.
            let background = if app.chrome_graphics_enabled
                && !pane_graphics.is_empty()
                && app.colors.truecolor
                && app.colors.foreground.is_some()
            {
                match (app.colors.background, theme.background(Role::Base, false)) {
                    (Some(original), ColorSource::Rgb(matte)) => {
                        Some(TerminalBackground { original, matte })
                    }
                    _ => None,
                }
            } else {
                None
            };
            terminal.set_background(&mut out, background)?;
            fds.clear();
            fds.extend([
                libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: resize_read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: app.input.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: app.jobs.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: graphics.wake_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ]);
            fds.extend_from_slice(&session_fds);
            for (pane, view) in &app.views {
                if view.retirement_requested || app.view_failures.contains_key(pane) {
                    continue;
                }
                fds.push(libc::pollfd {
                    fd: view.pump.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            let escape = Duration::from_millis(50).saturating_sub(last_input.elapsed());
            let timeout = if escape.is_zero() {
                1000
            } else {
                escape.as_millis().max(1) as i32
            };
            let timeout = app.selection_timeout(timeout);
            let timeout = if incoming.is_empty() {
                app.toast_timeout(timeout.min(session_timeout), Instant::now())
            } else {
                0
            };
            title_animation.capture(
                &chrome_surface.frame,
                &working_titles,
                &theme,
                app.colors.truecolor,
                |title| {
                    app.overlays.layers().any(|layer| {
                        layer.policy.modal
                            && title.x
                                < layer
                                    .placement
                                    .outer
                                    .x
                                    .saturating_add(layer.placement.outer.width)
                            && layer.placement.outer.x < title.x.saturating_add(title.width)
                            && title.y
                                < layer
                                    .placement
                                    .outer
                                    .y
                                    .saturating_add(layer.placement.outer.height)
                            && layer.placement.outer.y < title.y.saturating_add(title.height)
                    })
                },
                Instant::now(),
            );
            // Decorative ticks share the current presentation. Keep the original
            // non-animation deadline fixed so clipboard/input/session timers cannot
            // be postponed by an animation, and return on every ready descriptor.
            let presentation_deadline = Instant::now() + Duration::from_millis(timeout as u64);
            let hits = loop {
                title_animation.apply(&mut chrome_surface.frame);
                let mut surfaces = vec![Surface {
                    palette: None,
                    id: "ui:chrome",
                    content: SurfaceContent::Cells,
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: size.cols,
                        height: size.rows,
                    },
                    frame: &chrome_surface.frame,
                    selection: &[],
                    search: &[],
                    focused: app.host_focused && sidebar_active && !popup && !searching,
                    faint: faint_background,
                }];
                for (id, rect) in &rects {
                    let rect = content(*rect);
                    if rect.height == 0 || rect.width == 0 {
                        continue;
                    }
                    if let Some(frame) = app
                        .views
                        .get(id)
                        .and_then(|v| v.frame.as_ref())
                        .or_else(|| placeholders.get(id).map(|surface| &surface.frame))
                    {
                        surfaces.push(Surface {
                            palette: pane_palette,
                            id,
                            content: SurfaceContent::All,
                            rect,
                            frame,
                            selection: app.selection_spans(id),
                            search: app.search_spans(id),
                            faint: faint_background,
                            focused: !popup
                                && !searching
                                && !sidebar_active
                                && app.state.focused.as_ref() == Some(id)
                                && app.input_enabled(id)
                                && app.history(id).is_none()
                                && app
                                    .registry
                                    .panes
                                    .iter()
                                    .any(|p| p.id == *id && p.status == PaneStatus::Running),
                        });
                    }
                }
                surfaces.push(Surface {
                    palette: None,
                    id: "ui:chrome:graphics",
                    content: SurfaceContent::Graphics,
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: size.cols,
                        height: size.rows,
                    },
                    frame: &chrome_surface.frame,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: faint_background,
                });
                for segment in &resize_feedback_regions {
                    surfaces.push(Surface {
                        palette: None,
                        id: "ui:resize:edges",
                        content: SurfaceContent::CellsWithUnderlay(*segment),
                        rect: Rect {
                            x: 0,
                            y: 0,
                            width: size.cols,
                            height: size.rows,
                        },
                        frame: &resize_feedback_surface.frame,
                        selection: &[],
                        search: &[],
                        focused: false,
                        faint: false,
                    });
                }
                // Artwork may extend into the joined item's framing column. Keep
                // it above passive materials, but below modal contents and effects.
                let mut artwork_surface = Some(Surface {
                    palette: None,
                    id: "ui:sidebar:artwork",
                    content: SurfaceContent::Graphics,
                    rect: Rect {
                        x: 0,
                        y: 0,
                        width: size.cols,
                        height: size.rows,
                    },
                    frame: &artwork_effects,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: faint_background,
                });
                for layer in app.overlays.layers() {
                    if layer.policy.modal
                        && let Some(artwork) = artwork_surface.take()
                    {
                        surfaces.push(artwork);
                    }
                    if layer.id == "ui:modal"
                        && let Some(regions) = finder_cards
                    {
                        for (index, (_, body)) in regions.into_iter().enumerate() {
                            surfaces.push(Surface {
                                palette: None,
                                id: ["ui:modal:input", "ui:modal:results", "ui:modal:preview"]
                                    [index],
                                content: SurfaceContent::CellsClipped(body),
                                rect: layer.placement.outer,
                                frame: &popup_surface.frame,
                                selection: &[],
                                search: &[],
                                focused: index == 0
                                    && app.overlays.keyboard_owner() == Some(layer.id),
                                faint: false,
                            });
                        }
                    } else if layer.id == crate::popover::SURFACE_ID && popover_graphical {
                        if let Some(rect) = tooltip {
                            // Full cells in the joined contour include the card's
                            // framing column at the bridge, not just its text body.
                            let body = rect;
                            let first = surfaces.len();
                            for span in graphics.joined_opaque_spans() {
                                if span.row < body.y
                                    || span.row >= body.y.saturating_add(body.height).min(size.rows)
                                {
                                    continue;
                                }
                                let left = span.start.max(body.x);
                                let right = span
                                    .end
                                    .min(body.x.saturating_add(body.width))
                                    .min(size.cols);
                                if left >= right {
                                    continue;
                                }
                                let clip = Rect {
                                    x: left - layer.placement.outer.x,
                                    y: span.row - layer.placement.outer.y,
                                    width: right - left,
                                    height: 1,
                                };
                                if surfaces.len() > first
                                    && let Some(last) = surfaces.last_mut()
                                    && let SurfaceContent::CellsClipped(previous) =
                                        &mut last.content
                                    && previous.x == clip.x
                                    && previous.width == clip.width
                                    && previous.y.saturating_add(previous.height) == clip.y
                                {
                                    previous.height += 1;
                                    continue;
                                }
                                surfaces.push(Surface {
                                    palette: None,
                                    id: layer.id,
                                    content: SurfaceContent::CellsClipped(clip),
                                    rect: layer.placement.outer,
                                    frame: &app.popover.surface.frame,
                                    selection: &[],
                                    search: &[],
                                    focused: false,
                                    faint: faint_background,
                                });
                            }
                        }
                    } else {
                        let (frame, rect, content) = match layer.id {
                            crate::popover::SURFACE_ID => (
                                &app.popover.surface.frame,
                                layer.placement.outer,
                                SurfaceContent::All,
                            ),
                            "ui:guide" => (
                                &guide_surface.frame,
                                guide_draw_rect.expect("presented guide has a cell projection"),
                                SurfaceContent::All,
                            ),
                            "ui:modal" => {
                                if !modal_graphical {
                                    surfaces.push(Surface {
                                        palette: None,
                                        id: "ui:window",
                                        content: SurfaceContent::All,
                                        rect: layer.placement.outer,
                                        frame: &window_surface.frame,
                                        selection: &[],
                                        search: &[],
                                        focused: false,
                                        faint: false,
                                    });
                                }
                                (
                                    &popup_surface.frame,
                                    layer.placement.content,
                                    SurfaceContent::All,
                                )
                            }
                            _ => continue,
                        };
                        surfaces.push(Surface {
                            palette: None,
                            id: layer.id,
                            content,
                            rect,
                            frame,
                            selection: &[],
                            search: &[],
                            focused: app.overlays.keyboard_owner() == Some(layer.id),
                            faint: faint_background && !layer.policy.modal,
                        });
                    }
                    let (effects, effects_id) = match layer.id {
                        crate::popover::SURFACE_ID => (&popover_effects, "ui:popover:graphics"),
                        "ui:guide" => (&guide_effects, "ui:guide:graphics"),
                        "ui:modal" => (&window_effects, "ui:modal:graphics"),
                        _ => continue,
                    };
                    surfaces.push(Surface {
                        palette: None,
                        id: effects_id,
                        content: SurfaceContent::Graphics,
                        rect: Rect {
                            x: 0,
                            y: 0,
                            width: size.cols,
                            height: size.rows,
                        },
                        frame: effects,
                        selection: &[],
                        search: &[],
                        focused: false,
                        faint: faint_background && !layer.policy.modal,
                    });
                }
                if let Some(artwork) = artwork_surface {
                    surfaces.push(artwork);
                }
                if searching && !popup {
                    surfaces.push(Surface {
                        palette: None,
                        id: "ui:search",
                        content: SurfaceContent::All,
                        rect: search_rect.expect("search overlay has an owning pane footer"),
                        frame: &search_surface.frame,
                        selection: &[],
                        search: &[],
                        focused: true,
                        faint: false,
                    });
                }
                for slot in &toast_order[..toast_count] {
                    let card = toast_frames[*slot]
                        .as_ref()
                        .expect("visible toast presentation");
                    surfaces.push(Surface {
                        palette: None,
                        id: "ui:toast",
                        content: if card.graphical {
                            SurfaceContent::CellsClipped(Rect {
                                x: card.body.x - card.outer.x,
                                y: card.body.y - card.outer.y,
                                width: card.body.width,
                                height: card.body.height,
                            })
                        } else {
                            SurfaceContent::All
                        },
                        rect: card.outer,
                        frame: &card.frame,
                        selection: &[],
                        search: &[],
                        focused: false,
                        faint: false,
                    });
                }
                if !toast_effects.placements.is_empty() {
                    surfaces.push(Surface {
                        palette: None,
                        id: "ui:toast:graphics",
                        content: SurfaceContent::Graphics,
                        rect: Rect {
                            x: 0,
                            y: 0,
                            width: size.cols,
                            height: size.rows,
                        },
                        frame: &toast_effects,
                        selection: &[],
                        search: &[],
                        focused: false,
                        faint: false,
                    });
                }
                let scene = Scene { size, surfaces };
                renderer.render(&mut out, &scene, &app.colors, &mut terminal)?;
                let ready = unsafe {
                    libc::poll(
                        fds.as_mut_ptr(),
                        fds.len() as _,
                        title_animation.timeout(presentation_deadline, Instant::now()),
                    )
                };
                if ready < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                let now = Instant::now();
                if ready == 0 && now < presentation_deadline && title_animation.clock.advance(now) {
                    continue;
                }
                break scene.hit_map();
            };
            let mut resized = [0u8; 64];
            let mut geometry_changed = false;
            while matches!(resize_read.read(&mut resized),Ok(n)if n>0) {
                geometry_changed = true;
            }
            let input_size = if geometry_changed {
                dimensions()?
            } else {
                size
            };
            if geometry_changed {
                app.retire_resize("terminal geometry changed");
                app.validate_sidebar_resize(input_size);
                app.clear_resize_hover();
                app.sidebar_resize_hover = false;
            }
            let resized_rects = (input_size != size).then(|| app.rects(input_size));
            let input_rects = resized_rects.as_deref().unwrap_or(&rects);
            let available = fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0;
            let mut bytes = [0u8; 8192];
            let parts = if available {
                let n = std::io::stdin().read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                last_input = Instant::now();
                demux.feed(&bytes[..n])
            } else if last_input.elapsed() > Duration::from_millis(50) {
                vec![InputPart::Keys(demux.flush_escape())]
            } else {
                Vec::new()
            };
            incoming.extend(decode_input(&mut parser, parts, available));
            while let Some(event) = incoming.pop_front() {
                if sessions.route_release(&event, input_size)? {
                    continue;
                }
                if let ClientEvent::Reply(response) = &event
                    && sessions.route_device_reply(response)?
                {
                    continue;
                }
                let app = sessions.app_mut();
                match event {
                    ClientEvent::Reply(response) => {
                        if let Some(key) = response.strip_prefix(b"key;") {
                            app.key_events_report_releases = true;
                            if app.dispatch_key(
                                kitty_key(key)?,
                                &mut out,
                                input_size,
                                input_rects,
                            )? {
                                return Ok(());
                            }
                        } else {
                            let result = app.reply(&response, &mut out, &mut terminal);
                            app.scope_input_result(result)?;
                        }
                    }
                    ClientEvent::Legacy(InputEvent::Key(key)) => {
                        app.key_events_report_releases = false;
                        if app.dispatch_key(key_event(key), &mut out, input_size, input_rects)? {
                            return Ok(());
                        }
                    }
                    ClientEvent::Legacy(InputEvent::Paste(text)) if !app.resize_capturing() => {
                        let result = app.paste(text);
                        app.scope_input_result(result)?;
                    }
                    event @ (ClientEvent::Legacy(InputEvent::Mouse(_))
                    | ClientEvent::Mouse { .. }) => {
                        let (mouse, physical) = match event {
                            ClientEvent::Legacy(InputEvent::Mouse(mouse)) => (mouse, None),
                            ClientEvent::Mouse {
                                event,
                                button,
                                action,
                            } => (event, Some((button, action))),
                            _ => unreachable!("matched mouse event"),
                        };
                        if let Err(error) = app.mouse(
                            mouse,
                            physical,
                            input_size,
                            input_rects,
                            &workspace_hits,
                            &hits,
                        ) {
                            if error.downcast_ref::<updates::InputRejected>().is_some() {
                                app.scope_input_result(Err(error))?;
                            } else {
                                app.notify(format!("{error:#}"));
                            }
                        }
                    }
                    _ => {}
                }
                if app.session_action.is_some() {
                    break;
                }
            }
            let app = sessions.app_mut();
            if app.color_query_pending {
                terminal.prepare_background_query();
            }
            app.flush_color_query(&mut out)?;
        }
        Ok(())
    })();
    result.and(sessions.finish())
}

fn finish_app(mut app: App, client: &mut sessions::Attachment) -> Result<()> {
    let endpoint = app.socket.clone();
    let socket = &endpoint;
    let result = Ok(());
    if socket.is_revoked() {
        let mut finished = Ok(());
        match app.jobs.finish() {
            Ok(completions) => {
                for completion in completions {
                    accumulate_cleanup(
                        &mut finished,
                        shutdown_result(completion.result.map(|_| ()), socket.session_ended()),
                    );
                }
            }
            Err(error) => accumulate_cleanup(&mut finished, Err(error)),
        }
        accumulate_cleanup(&mut finished, app.input.finish());
        accumulate_cleanup(
            &mut finished,
            shutdown_result(client.detach(), socket.session_ended()),
        );
        return finished;
    }
    app.retire_resize("client detached");
    let mut cleanup = Ok(());
    while app.consent.is_some() {
        let finished = app.finish_consent(ClipboardResponse::denied());
        let waiting = app
            .consent
            .as_ref()
            .is_some_and(|consent| consent.reply.is_some())
            && finished
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<updates::InputRejected>())
                .is_some_and(|rejected| !rejected.0.stopped && !rejected.0.unknown_outcome);
        if waiting && !app.responses.is_empty() {
            accumulate_cleanup(&mut cleanup, app.drain_responses());
            continue;
        }
        accumulate_cleanup(&mut cleanup, finished);
        break;
    }
    accumulate_cleanup(&mut cleanup, app.drain_responses());
    if let Err(error) = app.abort_listing("client detached before enhanced paste listing completed")
        && cleanup.is_ok()
    {
        cleanup = Err(error);
    }
    let panes = app.views.keys().cloned().collect::<Vec<_>>();
    for pane in panes {
        let retirement = app.retire_view(&pane, false);
        accumulate_cleanup(&mut cleanup, retirement);
    }
    let fallback = std::mem::take(&mut app.views);
    // A full local policy reserve means this intent was never dispatched.
    // Shutdown may synchronously admit it before draining blocked native input.
    for (pane, attached) in &fallback {
        if matches!(attached.retirement_policy, RetirementPolicy::Visible) {
            let mut spec = attached.spec.clone();
            spec.visible = false;
            spec.interactive = false;
            spec.clipboard = false;
            let revoke = socket
                .checked(Operation::Pane {
                    pane_id: pane.clone(),
                    request: PaneRequest::Update {
                        view_id: attached.view.clone(),
                        spec,
                    },
                })
                .map(|_| ());
            accumulate_cleanup(&mut cleanup, revoke);
        }
    }
    // No UI work remains. Finish accepted metadata/attach jobs without losing
    // late admitted view IDs, including results made stale by detach/navigation.
    match app.jobs.finish() {
        Ok(completions) => {
            for completion in completions {
                match completion.result {
                    Ok(Background::Attached { pane, mut attached }) => {
                        accumulate_cleanup(
                            &mut cleanup,
                            detach_view(socket, &pane, &mut attached, false),
                        );
                    }
                    Ok(Background::CiRefresh {
                        owner,
                        target,
                        result,
                    }) => {
                        let known = !result.as_ref().is_err_and(transport::outcome_unknown);
                        app.popover.refresh_receipt(owner, &target, known);
                        accumulate_cleanup(&mut cleanup, result.context(completion.key));
                    }
                    Ok(Background::Management(_) | Background::Resize(_)) => {}
                    Err(error) => {
                        accumulate_cleanup(&mut cleanup, Err(error.context(completion.key)))
                    }
                }
            }
        }
        Err(error) => accumulate_cleanup(&mut cleanup, Err(error)),
    }
    for mut candidate in std::mem::take(&mut app.retiring_candidates) {
        // Never displayed, so no user input was addressed to this view.
        accumulate_cleanup(
            &mut cleanup,
            detach_view(socket, &candidate.pane, &mut candidate.attached, false),
        );
    }
    let retiring = std::mem::take(&mut app.retiring);
    let pointers = std::mem::take(app.pressed_pointers.get_mut());
    let drained = app.input.finish();
    for mut pointer in pointers {
        if !pointer.admitted {
            continue;
        }
        let operation = pointer
            .release
            .take()
            .unwrap_or_else(|| pointer.release_operation());
        accumulate_cleanup(&mut cleanup, socket.checked(operation).map(|_| ()));
    }
    // Only views whose final retirement was not admitted reach this fallback.
    // All accepted lane operations have settled before their explicit detach.
    for (pane, mut attached) in fallback {
        let focus_out = attached.retirement_focus_out;
        accumulate_cleanup(
            &mut cleanup,
            detach_view(socket, &pane, &mut attached, focus_out),
        );
    }
    let mut released = Ok(());
    for retired in retiring {
        let result = retired
            .receipt
            .recv()
            .context("pane input worker stopped before view retirement")
            .and_then(|result| result);
        drop(retired.attached);
        accumulate_cleanup(&mut released, result);
    }
    let lifecycle = client.lifecycle().map(|_| ());
    let detached = shutdown_result(client.detach(), socket.session_ended());
    shutdown_result(result, socket.session_ended())
        .and(cleanup)
        .and(drained)
        .and(released)
        .and(lifecycle)
        .and(detached)
}

fn shutdown_result(result: Result<()>, session_ended: bool) -> Result<()> {
    match result {
        Err(error) if transport::expected_shutdown(&error, session_ended) => Ok(()),
        result => result,
    }
}

fn accumulate_cleanup(result: &mut Result<()>, next: Result<()>) {
    if let Err(error) = next {
        if transport::expected_shutdown(&error, false) {
            return;
        }
        *result = match std::mem::replace(result, Ok(())) {
            Ok(()) => Err(error),
            Err(previous) => {
                Err(previous.context(format!("additional cleanup failure: {error:#}")))
            }
        };
    }
}
fn detach_view(
    socket: &Endpoint,
    pane: &str,
    attached: &mut Attached,
    focus_out: bool,
) -> Result<()> {
    let mut result = Ok(());
    if focus_out {
        accumulate_cleanup(
            &mut result,
            socket
                .checked(Operation::Pane {
                    pane_id: pane.into(),
                    request: PaneRequest::Focus {
                        view_id: attached.view.clone(),
                        focused: false,
                    },
                })
                .map(|_| ()),
        );
    }
    attached.pump.cancel();
    accumulate_cleanup(
        &mut result,
        socket
            .checked(Operation::Pane {
                pane_id: pane.into(),
                request: PaneRequest::Detach {
                    view_id: attached.view.clone(),
                },
            })
            .map(|_| ()),
    );
    result
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::tree;
    use std::{io, path::Path};
    #[test]
    fn rich_material_clears_frame_labels_without_touching_application_cells() {
        let size = Size {
            cols: 18,
            rows: 8,
            cell_width: 9,
            cell_height: 18,
        };
        let mut frame = blank(size);
        let rect = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        text(&mut frame, 1, 0, "日本語 title", false);
        text(&mut frame, 1, 1, "native content", false);
        let application = frame.rows[1].clone();
        material_border(&mut frame, rect, false, &[]);
        assert_eq!(frame.rows[1].cells[1..17], application.cells[1..17]);
        assert!(
            frame.rows[0]
                .cells
                .iter()
                .all(|cell| cell.text == " " && cell.width == 1)
        );
        assert!(
            frame.rows[7]
                .cells
                .iter()
                .all(|cell| cell.text == " " && cell.width == 1)
        );
    }

    pub(super) fn fixture() -> (App, updates::Capture) {
        let registry = RegistrySnapshot {
            revision: 0,
            workspaces: Vec::new(),
            panes: Vec::new(),
        };
        let state = Controller::new(&registry);
        let (input, received) = InputPump::capture();
        (
            App {
                socket: {
                    static ENDPOINT: std::sync::LazyLock<Endpoint> =
                        std::sync::LazyLock::new(|| {
                            Endpoint::unmanaged(Path::new("/unused-ui-test-endpoint"))
                        });
                    ENDPOINT.clone()
                },
                options: Options {
                    read_only: false,
                    detach_others: false,
                    clipboard_policy: "ask".into(),
                },
                session_action: None,
                namespace: (1, 1),
                tree: Arc::new(Mutex::new(Tree::default())),
                tree_press: None,
                sidebar_target: None,
                sidebar_backend_available: false,
                sidebar_has_workspace: false,
                backgrounded: false,
                notification_source: Some("session 0".into()),
                spawn_defaults: Some(SpawnDefaults {
                    argv: vec!["/bin/sh".into(), "-i".into()],
                    cwd: PathBuf::from("/fixture"),
                    home: PathBuf::from("/fixture/home"),
                    isolated_environment: true,
                }),
                registry,
                state,
                views: HashMap::new(),
                retiring: Vec::new(),
                retiring_candidates: Vec::new(),
                jobs: jobs::Scope::new(8).unwrap(),
                management: HashMap::new(),
                selection_epoch: 0,
                responses: Vec::new(),
                held: HashMap::new(),
                key_events_report_releases: true,
                input,
                input_failures: HashMap::new(),
                view_failures: HashMap::new(),
                consent: None,
                answered: HashMap::new(),
                deferred: RefCell::new(VecDeque::new()),
                paste_offer: None,
                paste_target: None,
                enhanced_supported: None,
                colors: ColorProbe::default(),
                color_query_pending: false,
                last_button: MouseButton::None,
                pointer_capture: None,
                sidebar_resize_hover: false,
                pressed_pointers: RefCell::new(Vec::new()),
                terminal_selection: selection::State::default(),
                resizing: resizing::State::default(),
                search: search::State::default(),
                finder: None,
                theme: crate::themes::ThemeId::default(),
                theme_picker: None,
                popover: crate::popover::Popover::new(size()),
                overlays: OverlayStack::default(),
                toasts: Arc::new(Mutex::new(toast::ToastStack::default())),
                guide: GuideCache::default(),
                chrome_graphics_enabled: true,
                notified_focus: None,
                focus_out_pending: HashSet::new(),
                lease_pending: HashSet::new(),
                host_focused: true,
            },
            received,
        )
    }

    fn metadata_session(app: &App, owner: TreeOwner, status: SessionStatus) -> Arc<tree::Session> {
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(
            &app.registry,
            app.state.workspace.as_deref(),
            app.state.window.as_deref(),
            app.state.focused.as_deref(),
        );
        Arc::new(tree::Session {
            owner,
            group: SessionGroup {
                name: format!("session {}", owner.slot),
                host: None,
                status,
                detail: String::new(),
            },
            registry: Arc::new(app.registry.clone()),
            rows: sidebar.rows.clone().into(),
            workspace: app.state.workspace.clone(),
            window: app.state.window.clone(),
            focused: app.state.focused.clone(),
            snapshots: Arc::new(HashMap::new()),
            statuses: Arc::new(HashMap::new()),
            history: Arc::new(HashMap::new()),
        })
    }
    fn publish_test_tree(app: &mut App) {
        let owner = app.tree_owner();
        let metadata = metadata_session(app, owner, SessionStatus::Online);
        app.tree.lock().unwrap().reconcile(vec![metadata], owner);
        app.synchronize_tree_focus();
    }

    #[test]
    fn unavailable_session_has_no_native_launch_authority_or_native_hints() {
        use crate::keyguide::Action;
        let (mut app, _) = fixture();
        app.socket = Endpoint::unavailable();
        app.spawn_defaults = None;
        for action in [
            Action::CreateWorkspace,
            Action::CreateTerminal,
            Action::OpenFinder,
            Action::StartSearch,
            Action::StartResize,
            Action::SplitHorizontal,
            Action::SendPrefix,
        ] {
            assert!(!app.supports_action(action));
        }
        for action in [
            Action::EnterSessions,
            Action::ReconnectSession,
            Action::OpenThemes,
            Action::Detach,
        ] {
            assert!(app.supports_action(action));
        }
        let error = app
            .open_form(FormKind::CreateWorkspace, "new".into())
            .unwrap_err();
        assert!(transport::detached_before_dispatch(&error));
        assert!(app.state.form.is_none());
        assert!(app.spawn_defaults.is_none());
    }

    #[test]
    fn clipboard_callbacks_accept_only_the_original_attachment_generation() {
        let (mut app, _) = fixture();
        app.namespace = (1, 2);
        app.consent = Some(consent(false));
        for id in ["a2g2_request", "a1g1_request"] {
            app.device_reply(
                format!("5522;type=read:id={id}:status=DATA:mime=dGV4dC9wbGFpbg==;b2xk").as_bytes(),
            )
            .unwrap();
            assert!(app.consent.as_ref().unwrap().state.contents.is_empty());
        }
        app.device_reply(
            b"5522;type=read:id=a1g2_request:status=DATA:mime=dGV4dC9wbGFpbg==;bmF0aXZl",
        )
        .unwrap();
        let consent = app.consent.as_ref().unwrap();
        assert_eq!(
            consent.state.id, "request",
            "native callback identity must not be rewritten"
        );
        assert_eq!(consent.state.contents[0].data, b"native");
        assert!(
            app.session_navigation_allowed(),
            "an already-dispatched device callback cannot stall another session"
        );
        app.consent.as_mut().unwrap().state.sent = false;
        assert!(
            !app.session_navigation_allowed(),
            "user-decision modal retains capture"
        );
    }

    #[test]
    fn legacy_keys_do_not_wait_for_releases_the_host_cannot_report() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        physical_keys(&mut app, b"a").unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Key { .. },
                ..
            }
        ));
        assert!(app.held.is_empty());
        app.backgrounded = true;
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Update { .. },
                ..
            }
        ));
        app.retire_view("focused-pane", true).unwrap();
        assert!(!app.views.contains_key("focused-pane"));
    }

    #[test]
    fn hidden_view_retirement_keeps_owed_key_release_on_its_original_view() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.backgrounded = true;
        app.key(key("KeyA"), &mut io::sink(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.action == KeyAction::Press));
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Update { spec, .. },
        } if pane_id == "focused-pane" && !spec.visible && !spec.interactive && !spec.clipboard));
        app.retire_view("focused-pane", true).unwrap();
        assert!(app.views.contains_key("focused-pane"));
        assert!(
            app.views
                .get_mut("focused-pane")
                .unwrap()
                .pump
                .latest()
                .unwrap()
                .is_none()
        );
        let mut release = key("KeyA");
        release.action = KeyAction::Release;
        app.key(release, &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view" && event.action == KeyAction::Release));
        assert!(app.held.is_empty());
        app.retire_view("focused-pane", true).unwrap();
        assert!(!app.views.contains_key("focused-pane"));
    }

    #[test]
    fn host_focus_loss_settles_all_keys_on_original_views_and_consumes_late_releases() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        for name in ["ShiftLeft", "KeyA"] {
            app.key(key(name), &mut io::sink(), size(), &[]).unwrap();
            capture.try_recv().unwrap();
        }
        attach_named_fixture(&mut app, "peer", "peer-view");
        app.key(key("KeyB"), &mut io::sink(), size(), &[]).unwrap();
        capture.try_recv().unwrap();
        app.backgrounded = true;
        app.retire_view("focused-pane", true).unwrap();
        capture.try_recv().unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(app.views.contains_key("focused-pane"));

        app.host_focus_changed(false).unwrap();
        let mut released = HashSet::new();
        while let Ok(operation) = capture.try_recv() {
            if let Operation::Pane {
                pane_id,
                request: PaneRequest::Key { view_id, event },
            } = operation
            {
                assert_eq!(event.action, KeyAction::Release);
                released.insert((pane_id, view_id, event.key));
            }
        }
        assert_eq!(
            released,
            HashSet::from([
                (
                    "focused-pane".into(),
                    "focused-view".into(),
                    "ShiftLeft".into()
                ),
                ("focused-pane".into(), "focused-view".into(), "KeyA".into()),
                ("peer".into(), "peer-view".into(), "KeyB".into()),
            ])
        );
        assert!(app.held.is_empty());
        app.retire_view("focused-pane", true).unwrap();
        assert!(!app.views.contains_key("focused-pane"));
        while capture.try_recv().is_ok() {}
        app.host_focus_changed(true).unwrap();
        while capture.try_recv().is_ok() {}
        for name in ["ShiftLeft", "KeyA", "KeyB"] {
            let mut release = key(name);
            release.action = KeyAction::Release;
            app.key(release, &mut io::sink(), size(), &[]).unwrap();
        }
        assert!(
            capture.try_recv().is_err(),
            "late key-up reached another view"
        );
        app.key(key("KeyA"), &mut io::sink(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "peer" && event.action == KeyAction::Press));
    }

    #[test]
    fn key_release_keeps_its_owner_when_admission_is_full_and_retires_after_retry() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.key(key("KeyA"), &mut io::sink(), size(), &[]).unwrap();
        capture.try_recv().unwrap();
        let mut occupied = std::iter::from_fn(|| {
            app.input
                .reserve(Operation::Pane {
                    pane_id: "busy".into(),
                    request: PaneRequest::Key {
                        view_id: "busy-view".into(),
                        event: key("KeyB"),
                    },
                })
                .ok()
        })
        .collect::<Vec<_>>();
        app.host_focus_changed(false).unwrap();
        assert!(app.held["KeyA"].pending);
        assert!(app.flush_key_releases().is_err());
        assert!(capture.try_recv().is_err());
        app.retire_view("focused-pane", true).unwrap();
        capture.try_recv().unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(app.views.contains_key("focused-pane"));
        let mut physical_release = key("KeyA");
        physical_release.action = KeyAction::Release;
        physical_release.modifiers = 4;
        assert!(
            app.key(physical_release, &mut io::sink(), size(), &[])
                .is_err()
        );
        assert!(app.key(key("KeyA"), &mut io::sink(), size(), &[]).is_err());
        assert!(app.held["KeyA"].pending);
        occupied.pop();
        app.flush_key_releases().unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == KeyAction::Release && event.modifiers == 4));
        assert!(app.held.is_empty());
        app.flush_key_releases().unwrap();
        assert!(capture.try_recv().is_err());
        drop(occupied);
        app.retire_view("focused-pane", true).unwrap();
        assert!(!app.views.contains_key("focused-pane"));
    }

    #[test]
    fn focus_loss_key_release_follows_accepted_paste_and_suffix_without_cancelling_them() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.key(key("KeyA"), &mut io::sink(), size(), &[]).unwrap();
        capture.try_recv().unwrap();
        let mut paste = consent(true);
        paste.pane = "focused-pane".into();
        paste.view = "focused-view".into();
        app.consent = Some(paste);
        app.enqueue(
            "focused-pane",
            PaneRequest::Key {
                view_id: "focused-view".into(),
                event: key("Enter"),
            },
        )
        .unwrap();
        app.host_focus_changed(false).unwrap();
        assert!(app.held["KeyA"].pending);
        assert!(capture.try_recv().is_err());
        app.finish_consent(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "text/plain".into(),
                data: b"accepted-paste".to_vec(),
            }],
            available: Vec::new(),
            remember: false,
        })
        .unwrap();
        app.flush_key_releases().unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::PasteContents { contents, .. },
        } if pane_id == "focused-pane" && contents[0].data == b"accepted-paste"));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.key == "Enter" && event.action == KeyAction::Press));
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Focus { focused: false, .. },
                ..
            }
        ));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.key == "KeyA" && event.action == KeyAction::Release));
        assert!(app.held.is_empty());
    }

    #[test]
    fn tree_click_selects_on_release_and_consumes_the_whole_gesture() {
        let (mut app, capture) = fixture();
        let own = app.tree_owner();
        let foreign = TreeOwner {
            slot: 1,
            generation: 7,
        };
        app.tree.lock().unwrap().reconcile(
            vec![
                metadata_session(&app, own, SessionStatus::Online),
                metadata_session(&app, foreign, SessionStatus::Online),
            ],
            own,
        );
        let target = TreeTarget {
            owner: foreign,
            node: TreeNode::Session,
        };
        let tree_hits = vec![(target.clone(), 3)];
        let size = size();
        let frame = blank(size);
        let hits = Scene {
            size,
            surfaces: vec![Surface {
                id: "ui:chrome",
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &frame,
                palette: None,
                content: SurfaceContent::Cells,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
            }],
        }
        .hit_map();
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        for (packet, pressed) in [
            (b"\x1b[<0;3;4M".as_slice(), true),
            (b"\x1b[<0;3;4m".as_slice(), false),
        ] {
            for event in decode_input(&mut parser, demux.feed(packet), false) {
                let ClientEvent::Mouse {
                    event,
                    button,
                    action,
                } = event
                else {
                    panic!("expected mouse event");
                };
                app.mouse(event, Some((button, action)), size, &[], &tree_hits, &hits)
                    .unwrap();
            }
            if pressed {
                assert!(app.session_action.is_none());
                assert!(matches!(app.pointer_capture, Some(PointerCapture::Ui)));
            }
        }
        assert!(matches!(app.session_action, Some(SessionAction::Tree {
            target: ref selected, action: crate::keyguide::Action::ActivateSelected, ..
        }) if selected == &target));
        assert_eq!(app.tree.lock().unwrap().cursor.as_ref(), Some(&target));
        assert!(
            app.state.sidebar.cursor.is_none(),
            "foreign IDs must not enter local context"
        );
        assert!(app.pointer_capture.is_none());
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn foreign_tree_navigation_uses_source_capabilities_without_local_native_ids() {
        use crate::keyguide::Action;
        let (mut app, capture) = fixture();
        unavailable_fixture(&mut app);
        let own = app.tree_owner();
        let foreign = TreeOwner {
            slot: 9,
            generation: 3,
        };
        let native = SidebarTarget::Pane {
            workspace: "history".into(),
            pane: "focused-pane".into(),
        };
        let selected = TreeTarget {
            owner: foreign,
            node: TreeNode::Native(native.clone()),
        };
        let source = metadata_session(&app, own, SessionStatus::Online);
        let foreign_source = metadata_session(&app, foreign, SessionStatus::Online);
        {
            let mut tree = app.tree.lock().unwrap();
            tree.reconcile(vec![source.clone(), foreign_source], own);
            tree.enter();
            tree.select(selected.clone());
            tree.hover(Some(&selected));
        }
        app.synchronize_tree_focus();
        assert!(app.state.sidebar.active);
        assert_eq!(app.state.sidebar.cursor, None);
        assert_eq!(app.state.sidebar.hover, None);
        assert!(app.supports_action(Action::CreateTerminal));
        let current_workspace = app.state.workspace.clone();
        app.key(key("ArrowUp"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert_eq!(app.state.workspace, current_workspace);
        assert_eq!(
            app.tree.lock().unwrap().cursor.as_ref().unwrap().owner,
            foreign
        );
        assert!(app.session_action.is_none());
        app.tree.lock().unwrap().reconcile(
            vec![
                source,
                metadata_session(&app, foreign, SessionStatus::Offline),
            ],
            own,
        );
        app.synchronize_tree_focus();
        assert_eq!(app.guide_context(), crate::keyguide::Context::Sessions);
        assert!(!app.supports_action(Action::CreateWorkspace));
        assert!(!app.supports_action(Action::CreateTerminal));
        let mut reconnect = key("KeyR");
        reconnect.text = "R".into();
        reconnect.modifiers = 1;
        app.key(reconnect, &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(app.session_action, Some(SessionAction::Tree {
            target: TreeTarget { owner, node: TreeNode::Session },
            action: Action::ReconnectSession, ..
        }) if owner == foreign));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn headless_online_tree_keeps_sidebar_creation_on_its_exact_owner() {
        let (mut app, capture) = fixture();
        publish_test_tree(&mut app);
        app.enter_sessions();
        assert!(app.tree.lock().unwrap().rows.is_empty());
        assert!(app.state.sidebar.active);
        assert!(app.sidebar_target.is_none());
        let mut create = key("KeyN");
        create.text = "n".into();
        app.key(create, &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(app.session_action, Some(SessionAction::Tree {
            target: TreeTarget { owner, node: TreeNode::Session },
            action: crate::keyguide::Action::CreateWorkspace, ..
        }) if owner == app.tree_owner()));
        assert!(
            app.state.form.is_none(),
            "coordinator must dispatch the owning context"
        );
        assert!(capture.try_recv().is_err());
    }

    fn theme_key(app: &mut App, key: &str, text: &str, modifiers: u16) {
        for action in [KeyAction::Press, KeyAction::Release] {
            app.key(
                KeyEvent {
                    key: key.into(),
                    text: text.into(),
                    modifiers,
                    action,
                    unshifted_codepoint: 0,
                },
                &mut Vec::new(),
                size(),
                &[],
            )
            .unwrap();
        }
    }

    #[test]
    fn theme_preview_cancel_restores_the_last_commit_and_empty_results_cannot_commit() {
        use crate::themes::ThemeId;
        let (mut app, _) = fixture();
        app.colors.truecolor = true;
        app.colors.foreground = Some([220, 230, 240]);
        app.colors.background = Some([20, 30, 40]);
        theme_key(&mut app, "KeyB", "", 2);
        theme_key(&mut app, "KeyT", "t", 0);
        app.paste("Solarized Light".into()).unwrap();
        assert_eq!(app.theme, ThemeId::SolarizedLight);
        theme_key(&mut app, "Enter", "", 0);
        assert!(app.theme_picker.is_none());
        theme_key(&mut app, "KeyB", "", 2);
        theme_key(&mut app, "KeyT", "t", 0);
        assert_eq!(
            app.theme_picker.as_ref().unwrap().list.selected(),
            Some(&ThemeId::SolarizedLight)
        );
        app.paste("not-a-theme".into()).unwrap();
        theme_key(&mut app, "Enter", "", 0);
        assert!(app.theme_picker.is_some());
        assert_eq!(app.theme, ThemeId::SolarizedLight);
        theme_key(&mut app, "KeyU", "", 2);
        app.paste("Tokyo Night Day".into()).unwrap();
        assert_eq!(app.theme, ThemeId::TokyoNightDay);
        theme_key(&mut app, "Escape", "", 0);
        assert!(app.theme_picker.is_none());
        assert_eq!(app.theme, ThemeId::SolarizedLight);
        let (fresh, _) = fixture();
        assert_eq!(fresh.theme, ThemeId::Terminal);
    }

    #[test]
    fn terminal_notifications_refresh_behind_preview_and_cancel_uses_latest_colors() {
        use crate::themes::{Palette, ThemeId};
        let (mut app, _) = fixture();
        app.colors.truecolor = true;
        app.colors.foreground = Some([220, 230, 240]);
        app.colors.background = Some([20, 30, 40]);
        app.begin_theme_picker();
        app.paste("Catppuccin Mocha".into()).unwrap();
        assert_eq!(app.theme, ThemeId::CatppuccinMocha);
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        for event in decode_input(&mut parser, demux.feed(b"\x1b[?997;2n"), false) {
            let ClientEvent::Reply(reply) = event else {
                panic!("theme report became input")
            };
            app.color_reply(&reply);
        }
        assert!(app.color_query_pending);
        app.color_reply(b"10;rgb:1111/2222/3333");
        app.color_reply(b"11;rgb:eeee/dddd/cccc");
        let mut palette = Palette::default();
        palette.update(&app.colors, app.theme);
        assert_eq!(palette.probe().background, Some([0x1e, 0x1e, 0x2e]));
        assert_eq!(app.theme, ThemeId::CatppuccinMocha);
        app.cancel_modal();
        palette.update(&app.colors, app.theme);
        assert_eq!(app.theme, ThemeId::Terminal);
        assert_eq!(palette.probe().foreground, Some([0x11, 0x22, 0x33]));
        assert_eq!(palette.probe().background, Some([0xee, 0xdd, 0xcc]));
    }

    #[test]
    fn passive_guide_tracks_ownership_and_sidebar_fit_without_application_input() {
        let (mut app, received) = fixture();
        let size = Size {
            cols: 120,
            rows: 36,
            cell_width: 9,
            cell_height: 18,
        };
        app.state.sidebar.active = false;
        app.refresh_guide(size);
        let sidebar = app.screen_geometry(size).0;
        let normal = app.guide_geometry(size, false).unwrap();
        assert_eq!(normal.x, sidebar.x);
        assert_eq!(normal.y + normal.height, size.rows);
        assert_eq!(app.guide_accent(), None);
        app.options.read_only = true;
        assert_eq!(app.guide_accent(), None);
        app.state.prefix = true;
        app.refresh_guide(size);
        assert_eq!(app.guide_accent(), Some(3));
        assert!(app.guide_geometry(size, false).is_some());
        app.state.prefix = false;
        app.state.sidebar.active = true;
        app.refresh_guide(size);
        assert_eq!(app.guide_accent(), Some(4));
        assert_eq!(app.guide_geometry(size, false).unwrap().x, sidebar.x);
        assert!(app.guide_geometry(size, true).is_none());
        app.state.sidebar.active = false;
        let narrow = Size {
            cols: 30,
            rows: 18,
            ..size
        };
        app.refresh_guide(narrow);
        let overflow = app.guide_geometry(narrow, false).unwrap();
        assert_eq!(overflow.x, app.screen_geometry(narrow).0.x);
        assert!(overflow.width > app.screen_geometry(narrow).0.width);
        assert!(overflow.width <= narrow.cols);
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn private_theme_notifications_decode_and_coalesce_one_refresh_per_batch() {
        let wire = b"\x1b[?997;1n\x1b[?997;2n";
        let (mut app, _) = fixture();
        for split in 0..=wire.len() {
            app.colors.truecolor = true;
            app.colors.foreground = Some([220, 230, 240]);
            app.colors.background = Some([20, 30, 40]);
            app.colors.palette.insert(2, [80, 180, 100]);
            let mut parser = InputParser::new();
            let mut demux = ReplyDemux::default();
            let mut events = decode_input(&mut parser, demux.feed(&wire[..split]), false);
            events.extend(decode_input(&mut parser, demux.feed(&wire[split..]), false));
            assert_eq!(events.len(), 2, "split {split}");
            for event in events {
                let ClientEvent::Reply(response) = event else {
                    panic!(
                        "private theme notification leaked to application input at split {split}"
                    );
                };
                app.color_reply(&response);
            }
            assert!(app.colors.truecolor);
            assert!(app.colors.foreground.is_none());
            assert!(app.colors.background.is_none());
            assert!(app.colors.palette.is_empty());
            assert!(app.color_query_pending);
            let mut out = Vec::new();
            app.flush_color_query(&mut out).unwrap();
            assert_eq!(out, ColorProbe::query(), "split {split}");
            assert!(!app.color_query_pending);
            app.flush_color_query(&mut out).unwrap();
            assert_eq!(out, ColorProbe::query(), "duplicate flush at split {split}");
        }

        app.colors.foreground = Some([220, 230, 240]);
        app.colors.background = Some([20, 30, 40]);
        app.colors.palette.insert(2, [80, 180, 100]);
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let mut typed = String::new();
        for event in decode_input(&mut parser, demux.feed(b"before\x1b[?997;3nafter"), false) {
            let ClientEvent::Legacy(InputEvent::Key(key)) = event else {
                panic!("unknown theme mode must not become a reply or consume adjacent typing");
            };
            typed.push_str(&key_event(key).text);
        }
        assert_eq!(typed, "beforeafter");
        let events = decode_input(
            &mut parser,
            demux.feed(b"\x1b[200~literal\x1b[?997;1ntext\x1b[201~"),
            false,
        );
        let [ClientEvent::Legacy(InputEvent::Paste(text))] = events.as_slice() else {
            panic!("a pasted notification must remain one literal paste");
        };
        assert_eq!(text, "literal\x1b[?997;1ntext");
        assert_eq!(app.colors.foreground, Some([220, 230, 240]));
        assert_eq!(app.colors.background, Some([20, 30, 40]));
        assert_eq!(app.colors.palette.get(&2), Some(&[80, 180, 100]));
        let mut out = Vec::new();
        app.flush_color_query(&mut out).unwrap();
        assert!(out.is_empty());
    }
    fn consent(paste: bool) -> DeviceConsent {
        DeviceConsent {
            pane: "origin-pane".into(),
            view: "origin-view".into(),
            reply: None,
            reservation: None,
            state: Consent {
                id: "request".into(),
                request: ClipboardRequest {
                    operation: ClipboardOperation::Read {
                        mimes: vec!["text/plain".into()],
                        list: false,
                    },
                    location: ClipboardLocation::Standard,
                    name: String::new(),
                    granted: false,
                    can_remember: false,
                    paste_offer_id: None,
                },
                started: Instant::now(),
                sent: true,
                allowed: true,
                contents: Vec::new(),
                available: Vec::new(),
                enhanced_supported: Some(true),
                paste,
                password: None,
            },
        }
    }
    pub(super) fn key(name: &str) -> KeyEvent {
        KeyEvent {
            key: name.into(),
            text: String::new(),
            modifiers: 0,
            action: KeyAction::Press,
            unshifted_codepoint: 0,
        }
    }
    pub(super) fn size() -> Size {
        Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
    #[test]
    fn slow_paste_blocks_only_origin_and_preserves_fifo_after_focus_changes() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(true));
        app.state.focused = Some("other-pane".into());
        app.enqueue(
            "origin-pane",
            PaneRequest::Key {
                view_id: "origin-view".into(),
                event: key("Enter"),
            },
        )
        .unwrap();
        assert!(received.try_recv().is_err());
        app.enqueue(
            "other-pane",
            PaneRequest::Key {
                view_id: "other-view".into(),
                event: key("KeyA"),
            },
        )
        .unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Key{..}}if pane_id=="other-pane")
        );
        app.finish_consent(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "text/plain".into(),
                data: b"paste-sentinel".to_vec(),
            }],
            available: Vec::new(),
            remember: false,
        })
        .unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::PasteContents{view_id,contents,..}}if pane_id=="origin-pane"&&view_id=="origin-view"&&contents[0].data==b"paste-sentinel")
        );
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Key{view_id,event}}if pane_id=="origin-pane"&&view_id=="origin-view"&&event.key=="Enter")
        );
    }
    #[test]
    fn failed_paste_never_releases_following_input() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(true));
        app.enqueue(
            "origin-pane",
            PaneRequest::Key {
                view_id: "origin-view".into(),
                event: key("Enter"),
            },
        )
        .unwrap();
        assert!(app.finish_consent(ClipboardResponse::denied()).is_err());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn clipboard_answer_stays_bound_to_origin_after_focus_changes() {
        let (mut app, received) = fixture();
        app.consent = Some(consent(false));
        app.state.focused = Some("other-pane".into());
        app.finish_consent(ClipboardResponse::denied()).unwrap();
        assert!(
            matches!(received.try_recv().unwrap(),Operation::Pane{pane_id,request:PaneRequest::Clipboard{view_id,request_id,..}}if pane_id=="origin-pane"&&view_id=="origin-view"&&request_id=="request")
        );
    }
    #[test]
    fn pending_capability_consumes_consent_key_release_and_repeat() {
        let (mut app, received) = fixture();
        let mut request = consent(false);
        request.state.sent = false;
        request.state.allowed = false;
        request.state.enhanced_supported = None;
        app.consent = Some(request);
        let mut press = key("KeyY");
        press.text = "y".into();
        let mut out = Vec::new();
        app.key(press.clone(), &mut out, size(), &[]).unwrap();
        assert!(app.consent.as_ref().unwrap().state.allowed);
        assert!(!app.consent.as_ref().unwrap().state.sent);
        app.consent = None;
        for action in [KeyAction::Repeat, KeyAction::Release] {
            let mut event = press.clone();
            event.action = action;
            app.key(event, &mut out, size(), &[]).unwrap();
        }
        assert!(received.try_recv().is_err());
        assert!(!app.state.suppressed.contains("KeyY"));
    }
    fn unavailable_fixture(app: &mut App) {
        attach_fixture(app);
        app.views.clear();
        app.notified_focus = None;
        app.registry.panes[0].status = PaneStatus::Stopped;
        app.registry.panes[0].workspace_id = Some("history".into());
        app.registry.panes[0].window_id = Some("history-window".into());
        app.registry.panes[0].cwd = "/recorded/cwd".into();
        app.registry.panes[0].foreground_cwd = Some("/observed/cwd".into());
        app.registry.panes[0].home = "/retired/home".into();
        app.registry.panes[0].isolated_environment = false;
        app.registry.workspaces.push(Workspace {
            id: "history".into(),
            name: "History".into(),
            revision: 7,
            windows: vec![Window {
                id: "history-window".into(),
                name: "History terminal".into(),
                revision: 7,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "focused-pane".into(),
                }),
            }],
        });
        app.state = Controller::new(&app.registry);
        publish_test_tree(app);
    }

    fn reopen_key(app: &mut App) -> Result<bool> {
        app.state.prefix = true;
        let mut event = key("KeyR");
        event.text = "R".into();
        event.modifiers = 1;
        app.key(event, &mut Vec::new(), size(), &[])
    }

    #[test]
    fn unavailable_confirmation_cancel_never_spawns_or_attaches_and_live_keys_still_flow() {
        let (mut app, received) = fixture();
        unavailable_fixture(&mut app);
        let before = serde_json::to_value(&app.registry).unwrap();
        app.sync_views(size(), &app.rects(size())).unwrap();
        assert!(!app.jobs.contains("attach:focused-pane"));
        assert!(app.active().is_none());
        reopen_key(&mut app).unwrap();
        assert!(matches!(
            app.state.form.as_ref().unwrap().kind,
            FormKind::ReopenPane { .. }
        ));
        assert!(app.management.is_empty());
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert!(app.state.form.is_none());
        assert_eq!(serde_json::to_value(&app.registry).unwrap(), before);
        assert!(received.try_recv().is_err());
        app.options.read_only = true;
        assert!(reopen_key(&mut app).is_err());
        assert!(app.state.form.is_none());
        assert!(app.management.is_empty());

        let (mut live, received) = fixture();
        attach_fixture(&mut live);
        assert!(reopen_key(&mut live).is_err());
        assert!(live.state.form.is_none());
        for name in ["Enter", "Escape"] {
            live.key(key(name), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
                request: PaneRequest::Key { event, .. }, ..
            } if event.key == name));
        }
    }
    #[test]
    fn modal_cancel_restores_sidebar_context_and_pending_submit_keeps_ownership() {
        let (mut app, captured) = fixture();
        unavailable_fixture(&mut app);
        app.enter_sidebar();
        app.tree.lock().unwrap().select(TreeTarget {
            owner: app.tree_owner(),
            node: TreeNode::Native(SidebarTarget::Workspace("history".into())),
        });
        app.synchronize_tree_focus();
        let focus = app.state.focused.clone();
        app.open_form(FormKind::RenameWorkspace, "name".into())
            .unwrap();
        app.leave_sidebar();
        app.state.form.as_mut().unwrap().pending = Some("accepted-request".into());
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert!(app.state.form.is_some());
        assert_eq!(app.overlays.keyboard_owner(), Some("ui:modal"));
        app.state.form.as_mut().unwrap().pending = None;
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert!(app.state.form.is_none());
        assert_eq!(app.state.focused, focus);
        assert!(app.state.sidebar.active);
        assert_eq!(
            app.state.sidebar.cursor,
            Some(SidebarTarget::Workspace("history".into()))
        );
        assert_eq!(app.overlays.keyboard_owner(), None);
        assert!(
            captured.try_recv().is_err(),
            "modal keys must not reach a native pane"
        );
    }

    #[test]
    fn confirmed_reopen_uses_client_shell_policy_and_explicit_directory_on_background_lane() {
        use std::{os::unix::net::UnixListener, thread};
        for (use_client_cwd, has_foreground_cwd) in [(false, true), (false, false), (true, true)] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("reopen.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (mut app, received) = fixture();
            let endpoint = Endpoint::unmanaged(&socket);
            app.socket = endpoint.clone();
            unavailable_fixture(&mut app);
            if !has_foreground_cwd {
                app.registry.panes[0].foreground_cwd = None;
            }
            app.spawn_defaults.as_mut().unwrap().argv =
                vec!["/client/configured-shell".into(), "--interactive".into()];
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = transport::test_wire::read_request(&mut stream).unwrap();
                transport::test_wire::write_response(
                    &mut stream,
                    &Response {
                        version: PROTOCOL_VERSION,
                        failure: None,
                        ok: false,
                        result: None,
                        error: Some("fixture rejected spawn".into()),
                    },
                )
                .unwrap();
                request
            });
            let before = serde_json::to_value(&app.registry).unwrap();
            reopen_key(&mut app).unwrap();
            if use_client_cwd {
                app.key(key("ArrowDown"), &mut Vec::new(), size(), &[])
                    .unwrap();
            }
            // A newer registry observation must not silently replace the
            // revision that the user confirmed.
            app.registry.workspaces[0].windows[0].revision = 9;
            app.key(key("Enter"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(
                app.state
                    .form
                    .as_ref()
                    .is_some_and(|form| form.pending.is_some())
            );
            assert!(app.jobs.contains("manage:window:history-window"));
            assert_eq!(
                app.registry.workspaces[0].windows[0].layout.as_ref(),
                Some(&LayoutNode::Leaf {
                    pane_id: "focused-pane".into()
                })
            );
            let request = server.join().unwrap();
            let jobs = std::mem::replace(&mut app.jobs, jobs::Scope::new(1).unwrap());
            let completions = jobs.finish().unwrap();
            assert_eq!(completions.len(), 1);
            assert!(completions[0].result.is_err());
            match request.operation {
                Operation::PaneReopen {
                    workspace_id,
                    window_id,
                    expected_revision,
                    pane_id,
                    spec,
                } => {
                    assert_eq!(workspace_id, "history");
                    assert_eq!(window_id, "history-window");
                    assert_eq!(expected_revision, 7);
                    assert_eq!(pane_id, "focused-pane");
                    assert_eq!(spec.argv, app.defaults().unwrap().argv);
                    assert_eq!(spec.home, app.defaults().unwrap().home);
                    assert_eq!(
                        spec.isolated_environment,
                        app.defaults().unwrap().isolated_environment
                    );
                    assert_eq!(
                        spec.cwd,
                        if use_client_cwd {
                            app.defaults().unwrap().cwd.clone()
                        } else if has_foreground_cwd {
                            PathBuf::from("/observed/cwd")
                        } else {
                            PathBuf::from("/recorded/cwd")
                        }
                    );
                }
                other => panic!("unexpected replacement operation: {other:?}"),
            }
            app.registry.workspaces[0].windows[0].revision = 7;
            assert_eq!(serde_json::to_value(&app.registry).unwrap(), before);
            assert!(received.try_recv().is_err());
        }
    }

    #[test]
    fn changed_selection_and_read_only_transition_reject_confirmation_before_mutation() {
        for read_only in [false, true] {
            let (mut app, received) = fixture();
            unavailable_fixture(&mut app);
            reopen_key(&mut app).unwrap();
            if read_only {
                app.options.read_only = true;
            } else {
                app.focus("focused-pane".into());
            }
            assert!(app.apply_form(size()).is_err());
            assert!(app.management.is_empty());
            assert!(!app.jobs.contains("manage:window:history-window"));
            assert!(received.try_recv().is_err());
        }
    }

    #[test]
    fn delayed_reopen_completion_never_steals_newer_selection_or_focuses_unprojected_record() {
        for (epoch_changed, still_projected) in [(true, true), (false, false), (false, true)] {
            let (mut app, _) = fixture();
            unavailable_fixture(&mut app);
            let mut neighbor = app.registry.panes[0].clone();
            neighbor.id = "neighbor".into();
            neighbor.status = PaneStatus::Running;
            app.registry.panes.push(neighbor);
            app.registry.workspaces[0].windows[0].layout = split_leaf(
                app.registry.workspaces[0].windows[0].layout.take(),
                "focused-pane",
                "neighbor",
                Axis::Horizontal,
            );
            let mut snapshot = app.registry.clone();
            let mut replacement = snapshot.panes[0].clone();
            replacement.id = "new-execution".into();
            replacement.status = PaneStatus::Running;
            snapshot.panes.push(replacement);
            snapshot.revision += 1;
            if still_projected {
                snapshot.workspaces[0].windows[0].layout = split_leaf(
                    snapshot.workspaces[0].windows[0].layout.take(),
                    "focused-pane",
                    "new-execution",
                    Axis::Horizontal,
                );
            }
            let intent = ManagementIntent {
                epoch: app.selection_epoch,
                selection: None,
            };
            if epoch_changed {
                app.focus("neighbor".into());
            }
            app.accept_management(
                management::Completion {
                    registry: snapshot,
                    selection: Some(management::Selection {
                        workspace: "history".into(),
                        window: "history-window".into(),
                        pane: "new-execution".into(),
                    }),
                },
                Some(intent),
            );
            assert_eq!(
                app.state.focused.as_deref(),
                Some(if epoch_changed {
                    "neighbor"
                } else if still_projected {
                    "new-execution"
                } else {
                    "focused-pane"
                })
            );
        }
    }

    #[test]
    fn read_only_split_is_rejected_without_sending_mutation() {
        let (mut app, received) = fixture();
        app.options.read_only = true;
        app.state.prefix = true;
        let mut event = key("KeyN");
        event.text = "n".into();
        assert!(app.key(event, &mut Vec::new(), size(), &[]).is_err());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn detach_never_sends_a_termination() {
        let (mut app, received) = fixture();
        app.state.prefix = true;
        let mut event = key("KeyD");
        event.text = "d".into();
        assert!(app.key(event, &mut Vec::new(), size(), &[]).unwrap());
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn control_bracket_reaches_original_pane_with_balanced_key_lifecycle() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        for action in [KeyAction::Press, KeyAction::Release] {
            let mut event = key("BracketRight");
            event.modifiers = 2;
            event.text = if action == KeyAction::Press { "]" } else { "" }.into();
            event.action = action;
            assert!(!app.key(event, &mut io::sink(), size(), &[]).unwrap());
            assert!(matches!(
                received.try_recv().unwrap(),
                Operation::Pane {
                    pane_id,
                    request: PaneRequest::Key { view_id, event },
                } if pane_id == "focused-pane"
                    && view_id == "focused-view"
                    && event.key == "BracketRight"
                    && event.modifiers == 2
                    && event.action == action
            ));
        }
        assert!(app.held.is_empty());
    }
    #[test]
    fn removed_prefix_management_commands_do_not_mutate_or_open_sidebar_forms() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        for command in ["b", "S", "c", "N", "r", "p", "!", "X"] {
            physical_keys(&mut app, format!("\x02{command}").as_bytes()).unwrap();
            assert!(app.state.form.is_none(), "{command}");
            assert!(!app.state.sidebar.active, "{command}");
            assert!(!app.state.prefix, "{command}");
            assert!(received.try_recv().is_err(), "{command}");
        }
    }
    #[test]
    fn read_only_is_independent_of_visible_interactive_size_eligibility() {
        let (mut app, _received) = fixture();
        app.options.read_only = true;
        app.options.clipboard_policy = "deny".into();
        let spec = app.spec(size());
        assert!(spec.visible && spec.interactive);
        assert!(!spec.input && !spec.clipboard);
    }
    pub(super) fn attach_fixture(app: &mut App) {
        attach_named_fixture(app, "focused-pane", "focused-view");
    }
    pub(super) fn attach_named_fixture(app: &mut App, pane: &str, view: &str) {
        app.registry.panes.push(PaneInfo {
            id: pane.into(),
            workspace_id: None,
            window_id: None,
            title: "test shell".into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: app.defaults().unwrap().cwd.clone(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: None,
            home: app.defaults().unwrap().home.clone(),
            isolated_environment: true,
            harness_status_changed_at_ms: None,
        });
        let metadata = PaneSnapshot {
            pane_id: pane.into(),
            pid: None,
            revision: 1,
            snapshot_revision: 1,
            frame: None,
            image_refs: Vec::new(),
            resource_protocol: 1,
            writer: None,
            lifecycle: "running".into(),
            exit_code: None,
            error: None,
            program_status: Vec::new(),
            program_identity: None,
            events: Vec::new(),
            event_sequence: 0,
            size_owner: None,
            lease_generation: 0,
            views: Vec::new(),
            clipboard: None,
            selection: None,
            search: None,
            clipboard_pending: false,
            capabilities: PaneCapabilities {
                sixel: false,
                normalized_frames: true,
                native_mouse_reporting: true,
                clipboard_timeout_ms: 5000,
                view_timeout_ms: 5000,
                poll_wait_max_ms: 1000,
                row_deltas: true,
                scene_encodings: vec!["planus_v1".into()],
            },
        };
        app.views.insert(
            pane.into(),
            Attached {
                view: view.into(),
                spec: app.spec(size()),
                pump: Pump::capture(),
                metadata,
                clipboard_id: None,
                frame: Some(blank(size())),
                history: false,
                revision: 1,
                images: HashMap::new(),
                seeded: 0,
                retirement_requested: false,
                retirement_policy: RetirementPolicy::Visible,
                retirement_focus_out: false,
                retirement_error: None,
            },
        );
        app.state.focused = Some(pane.into());
        app.notified_focus = Some((pane.into(), view.into()));
    }
    #[test]
    fn rejected_scene_keeps_metadata_frame_resources_and_clipboard_unchanged() {
        for invalid_baseline in [true, false] {
            let (mut app, _received) = fixture();
            attach_fixture(&mut app);
            let attached = app.views.get_mut("focused-pane").unwrap();
            attached.clipboard_id = Some("pending-request".into());
            attached.history = true;
            let before = attached.frame.clone().unwrap();
            let before_metadata = serde_json::to_value(&attached.metadata).unwrap();
            let mut next = before.clone();
            next.rows[0].cells[0].text = "λ".into();
            next.alternate_screen = true;
            let baseline = invalid_baseline.then(|| (0, Arc::new(before.clone())));
            let bytes = scene_wire::encode(&next, 2, baseline.as_ref(), 0, &[]).unwrap();
            let mut metadata = attached.metadata.clone();
            metadata.revision = 2;
            metadata.snapshot_revision += 1;
            metadata.lifecycle = "exited".into();
            if !invalid_baseline {
                metadata.image_refs.push(ImageKey {
                    id: 7,
                    generation: 9,
                });
            }
            let update = Snapshot {
                metadata,
                scene: Some(scene_wire::decode(&bytes).unwrap()),
            };
            assert!(attached.apply(update).is_err());
            assert_eq!(
                serde_json::to_value(&attached.metadata).unwrap(),
                before_metadata
            );
            assert_eq!(attached.revision, 1);
            assert_eq!(attached.frame.as_ref(), Some(&before));
            assert!(attached.images.is_empty());
            assert!(attached.history);
            assert_eq!(attached.clipboard_id.as_deref(), Some("pending-request"));
        }
    }

    #[test]
    fn scene_resource_generation_replacement_retires_only_the_old_cache_entry() {
        let (mut app, _received) = fixture();
        attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        let old = Arc::new(Image {
            id: 7,
            generation: 1,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![1, 2, 3, 255],
        });
        let new = Arc::new(Image {
            id: 7,
            generation: 2,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![4, 5, 6, 255],
        });
        attached.images.insert(old.key(), old.clone());
        let mut next = attached.frame.clone().unwrap();
        next.rows[0].cells[0].text = "N".into();
        next.images.push(new.clone());
        let bytes = scene_wire::encode(&next, 2, None, 0, &[old.key()]).unwrap();
        let mut metadata = attached.metadata.clone();
        metadata.revision = 2;
        metadata.image_refs = vec![new.key()];
        attached
            .apply(Snapshot {
                metadata,
                scene: Some(scene_wire::decode(&bytes).unwrap()),
            })
            .unwrap();
        assert_eq!(attached.revision, 2);
        assert!(!attached.images.contains_key(&old.key()));
        assert_eq!(attached.images[&new.key()].pixels, new.pixels);
        let frame = attached.frame.as_ref().unwrap();
        assert_eq!(frame.rows[0].cells[0].text.as_str(), "N");
        assert_eq!(frame.images, vec![new]);
    }
    fn physical_keys(app: &mut App, bytes: &[u8]) -> Result<()> {
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        for event in decode_input(&mut parser, demux.feed(bytes), false) {
            let key = match event {
                ClientEvent::Reply(packet) if packet.starts_with(b"key;") => {
                    app.key_events_report_releases = true;
                    kitty_key(&packet[4..])?
                }
                ClientEvent::Legacy(InputEvent::Key(key)) => {
                    app.key_events_report_releases = false;
                    key_event(key)
                }
                _ => panic!("expected physical keyboard event"),
            };
            assert!(!app.key(key, &mut Vec::new(), size(), &[])?);
        }
        Ok(())
    }
    #[test]
    fn kitty_modifier_transitions_and_repeats_preserve_prefix_until_percent_press() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        app.options.read_only = true;
        physical_keys(&mut app, b"\x1b[57442;5:1u\x1b[98;5:1u").unwrap();
        assert!(app.state.prefix);
        physical_keys(&mut app, b"\x1b[98;5:2u\x1b[98;5:3u\x1b[57442;1:3u").unwrap();
        assert!(app.state.prefix);
        physical_keys(
            &mut app,
            b"\x1b[57441;2:1u\x1b[57441;2:2u\x1b[97;2:2u\x1b[97;2:3u",
        )
        .unwrap();
        assert!(app.state.prefix);
        physical_keys(&mut app, b"\x1b[53:37;2:1;37u").unwrap_err();
        assert!(!app.state.prefix);
        assert!(app.state.suppressed.contains("Digit5"));
        assert!(received.try_recv().is_err());
        app.options.read_only = false;
        physical_keys(
            &mut app,
            b"\x1b[53:37;2:2;37u\x1b[53:37;2:3u\x1b[57441;1:3u",
        )
        .unwrap();
        assert!(
            received.try_recv().is_err(),
            "UI command repeat/release leaked to pane"
        );
        physical_keys(&mut app, b"\x1b[97;1:1;97u").unwrap();
        assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. }
        } if pane_id == "focused-pane" && event.key == "KeyA" && event.text == "a"));
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn kitty_double_prefix_queues_exactly_one_literal_control_b_lifecycle() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        physical_keys(
            &mut app,
            b"\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u\x1b[57442;1:3u",
        )
        .unwrap();
        assert!(app.state.prefix);
        assert!(received.try_recv().is_err());
        physical_keys(&mut app, b"\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u").unwrap();
        assert!(!app.state.prefix);
        for expected in [KeyAction::Press, KeyAction::Release] {
            assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { view_id, event }
            } if pane_id == "focused-pane" && view_id == "focused-view"
                && event.key == "KeyB" && event.modifiers == 2 && event.action == expected));
        }
        assert!(
            received.try_recv().is_err(),
            "physical prefix repeat/release was forwarded twice"
        );
    }
    #[test]
    fn physical_sgr_sidebar_click_selects_the_rendered_workspace_row() {
        let (mut app, received) = fixture();
        app.registry.workspaces = vec![
            Workspace {
                id: "main".into(),
                name: "Main".into(),
                revision: 1,
                windows: vec![Window {
                    id: "main-window".into(),
                    name: "Main terminal".into(),
                    revision: 1,
                    layout: Some(LayoutNode::Split {
                        axis: Axis::Horizontal,
                        ratio: 500,
                        first: Box::new(LayoutNode::Leaf {
                            pane_id: "alpha".into(),
                        }),
                        second: Box::new(LayoutNode::Leaf {
                            pane_id: "beta".into(),
                        }),
                    }),
                }],
            },
            Workspace {
                id: "remote".into(),
                name: "Other workspace".into(),
                revision: 1,
                windows: Vec::new(),
            },
        ];
        app.registry.panes = ["alpha", "beta"]
            .into_iter()
            .map(|id| PaneInfo {
                id: id.into(),
                workspace_id: Some("main".into()),
                window_id: Some("main-window".into()),
                title: id.into(),
                status: PaneStatus::Running,
                exit_code: None,
                cwd: app.defaults().unwrap().cwd.clone(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                project: None,
                home: app.defaults().unwrap().home.clone(),
                isolated_environment: true,
                harness_status_changed_at_ms: None,
            })
            .collect();
        app.state = Controller::new(&app.registry);
        publish_test_tree(&mut app);
        let size = Size {
            cols: 100,
            rows: 36,
            cell_width: 9,
            cell_height: 18,
        };
        let rects = app.rects(size);
        let mut surface = UiSurface::new(size);
        let workspace_hits = crate::controller::tree_chrome(
            &mut surface,
            crate::controller::TreeChromeContext {
                native: crate::controller::ChromeContext {
                    size,
                    state: &app.state,
                    registry: &app.registry,
                    rects: &rects,
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                tree: &app.tree.lock().unwrap(),
                active: app.tree_owner(),
                popup: None,
            },
            |_| None,
            |_| None,
            |_| None,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let row = surface
            .frame
            .rows
            .iter()
            .position(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("Other workspace")
            })
            .expect("target workspace is visible");
        let scene = Scene {
            size,
            surfaces: vec![Surface {
                palette: None,
                id: "ui:chrome",
                content: SurfaceContent::All,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &surface.frame,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
            }],
        };
        let hits = scene.hit_map();
        let mut parser = InputParser::new();
        let mut demux = ReplyDemux::default();
        let bytes = format!("\x1b[<0;6;{}M\x1b[<0;6;{}m", row + 1, row + 1);
        for event in decode_input(&mut parser, demux.feed(bytes.as_bytes()), false) {
            let ClientEvent::Mouse {
                event,
                button,
                action,
            } = event
            else {
                panic!("expected physical SGR mouse event");
            };
            app.mouse(
                event,
                Some((button, action)),
                size,
                &rects,
                &workspace_hits,
                &hits,
            )
            .unwrap();
        }
        let SessionAction::Tree {
            target,
            action,
            size,
        } = app.session_action.take().unwrap()
        else {
            panic!("expected tree action");
        };
        let TreeNode::Native(native) = target.node else {
            panic!("expected native target");
        };
        assert_eq!(target.owner, app.tree_owner());
        app.dispatch_tree_action(Some(&native), action, size)
            .unwrap();
        assert_eq!(app.state.workspace.as_deref(), Some("remote"));
        assert!(
            received.try_recv().is_err(),
            "sidebar input leaked to a pane"
        );
    }
    struct PointerFixture {
        size: Size,
        rects: Vec<(String, Rect)>,
        hits: HitMap,
    }
    impl PointerFixture {
        fn new(app: &mut App) -> Self {
            attach_fixture(app);
            attach_named_fixture(app, "beta", "beta-view");
            // This fixture exercises application-owned pointer lifecycles;
            // selection cases explicitly opt out of native mouse reporting.
            for view in app.views.values_mut() {
                view.frame.as_mut().unwrap().mouse_reporting = true;
            }
            app.state.focused = Some("focused-pane".into());
            app.notified_focus = Some(("focused-pane".into(), "focused-view".into()));
            let size = Size {
                cols: 120,
                rows: 32,
                ..size()
            };
            let (_, body) = app.screen_geometry(size);
            let first = Rect {
                width: body.width / 2,
                ..body
            };
            let second = Rect {
                x: body.x + first.width,
                width: body.width - first.width,
                ..body
            };
            let rects: Vec<(String, Rect)> =
                vec![("focused-pane".into(), first), ("beta".into(), second)];
            let frame = blank(size);
            let mut surfaces = vec![Surface {
                palette: None,
                id: "ui:chrome",
                content: SurfaceContent::All,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: size.rows,
                },
                frame: &frame,
                selection: &[],
                search: &[],
                focused: false,
                faint: false,
            }];
            for (pane, rect) in &rects {
                surfaces.push(Surface {
                    palette: None,
                    id: pane,
                    content: SurfaceContent::All,
                    rect: content(*rect),
                    frame: &frame,
                    selection: &[],
                    search: &[],
                    focused: false,
                    faint: false,
                });
            }
            let hits = Scene { size, surfaces }.hit_map();
            Self { size, rects, hits }
        }
        fn send(&self, app: &mut App, button: u8, x: u16, y: u16, release: bool) {
            self.try_send(app, button, x, y, release).unwrap();
        }
        fn try_send(&self, app: &mut App, button: u8, x: u16, y: u16, release: bool) -> Result<()> {
            let packet = format!(
                "\x1b[<{button};{};{}{}",
                x + 1,
                y + 1,
                if release { 'm' } else { 'M' }
            );
            let mut parser = InputParser::new();
            let mut demux = ReplyDemux::default();
            for event in decode_input(&mut parser, demux.feed(packet.as_bytes()), false) {
                let ClientEvent::Mouse {
                    event,
                    button,
                    action,
                } = event
                else {
                    panic!("expected decoded SGR mouse event");
                };
                app.mouse(
                    event,
                    Some((button, action)),
                    self.size,
                    &self.rects,
                    &[],
                    &self.hits,
                )?;
            }
            Ok(())
        }
    }
    fn paint_test_toasts(app: &mut App, size: Size) {
        app.toasts.lock().unwrap().paint(
            size,
            toast::ToastLayout {
                max_width: 72,
                top_margin: 1,
                side_margin: 2,
                gap: 1,
                max_lines: 5,
            },
            &Theme::default(),
            |_, _, _| false,
        );
    }

    #[test]
    fn physical_sgr_chord_releases_stay_with_toast_after_dismissal_or_expiry() {
        for expire in [false, true] {
            let (mut app, capture) = fixture();
            let pointer = PointerFixture::new(&mut app);
            app.notify("Chorded notice".into());
            paint_test_toasts(&mut app, pointer.size);
            let dismiss = {
                let toasts = app.toasts.lock().unwrap();
                let card = toasts.cards().next().unwrap();
                card.dismiss
            };
            pointer.send(&mut app, 0, dismiss.x, dismiss.y, false);
            pointer.send(&mut app, 2, dismiss.x, dismiss.y, false);
            assert!(app.toasts.lock().unwrap().pointer_captured());
            if expire {
                let mut toasts = app.toasts.lock().unwrap();
                let deadline = toasts.next_deadline().unwrap();
                toasts.expire(deadline);
            }
            pointer.send(&mut app, 0, dismiss.x, dismiss.y, true);
            assert!(app.toasts.lock().unwrap().is_empty());
            assert!(
                app.toasts.lock().unwrap().pointer_captured(),
                "secondary button remains held"
            );
            let native = content(pointer.rects[0].1);
            pointer.send(&mut app, 2, native.x + 2, native.y + 8, true);
            assert!(!app.toasts.lock().unwrap().pointer_captured());
            assert!(app.pointer_capture.is_none());
            assert!(capture.try_recv().is_err(), "consumed chord release leaked");
            pointer.send(&mut app, 0, native.x + 2, native.y + 8, false);
            pointer.send(&mut app, 0, native.x + 2, native.y + 8, true);
            let events = captured_mouse(&capture);
            assert_eq!(events.len(), 2, "subsequent native click was swallowed");
            assert_eq!(events[0].2.action, MouseAction::Press);
            assert_eq!(events[1].2.action, MouseAction::Release);
        }
    }

    #[test]
    fn title_sweep_restores_stable_selected_and_hover_ink_without_accumulation() {
        let now = Instant::now();
        for selected in [false, true] {
            let size = Size {
                cols: 32,
                rows: 1,
                cell_width: 9,
                cell_height: 18,
            };
            let rect = Rect {
                x: 3,
                y: 0,
                width: 24,
                height: 1,
            };
            let mut surface = UiSurface::new(size);
            surface.paint(
                size,
                vec![(rect, "A visible working title".into(), selected)],
                None,
            );
            let mut colors = ColorProbe {
                truecolor: true,
                foreground: Some([220, 230, 240]),
                background: Some([20, 30, 40]),
                ..Default::default()
            };
            colors.palette.insert(12, [210, 130, 220]);
            colors.palette.insert(3, [230, 180, 90]);
            let mut theme = Theme::default();
            crate::appearance::update_theme(&mut theme, &colors);
            surface.apply_theme(&theme, Role::Base, &[]);
            for cell in &mut surface.frame.rows[0].cells {
                cell.style.underline = 1; // Hover intent remains independent.
            }
            let baseline = surface.frame.clone();
            let mut animation = SidebarTitleAnimation::new(now);
            animation.capture(&surface.frame, &[rect], &theme, true, |_| false, now);
            assert!(animation.clock.next_deadline().is_some());
            assert!(animation.clock.advance(now + Duration::from_secs(1)));
            animation.apply(&mut surface.frame);
            assert_ne!(surface.frame, baseline);
            let first = surface.frame.clone();
            animation.apply(&mut surface.frame);
            assert_eq!(surface.frame, first, "No accumulated blends");
            for (cell, original) in surface.frame.rows[0]
                .cells
                .iter()
                .zip(&baseline.rows[0].cells)
            {
                assert_eq!(cell.text, original.text);
                assert_eq!(cell.width, original.width);
                assert_eq!(cell.style.inverse, original.style.inverse);
                assert_eq!(cell.style.underline, original.style.underline);
                if original.style.inverse {
                    assert_eq!(
                        cell.style.foreground_source,
                        original.style.foreground_source
                    );
                } else {
                    assert_eq!(
                        cell.style.background_source,
                        original.style.background_source
                    );
                }
            }
            animation.restore(&mut surface.frame);
            assert_eq!(
                surface.frame, baseline,
                "Stable style restored before cached paint/theme"
            );
            for (truecolor, covered, titles) in [
                (false, false, vec![rect]),
                (true, true, vec![rect]),
                (true, false, Vec::new()),
            ] {
                animation.capture(&surface.frame, &titles, &theme, truecolor, |_| covered, now);
                assert!(animation.clock.next_deadline().is_none());
                animation.apply(&mut surface.frame);
                assert_eq!(
                    surface.frame, baseline,
                    "Fallback/covered/offscreen is static"
                );
            }
        }
    }

    #[test]
    fn title_sweep_deadline_does_not_extend_other_presentation_deadlines() {
        let now = Instant::now();
        let animation = SidebarTitleAnimation::new(now);
        let deadline = now + Duration::from_millis(40);
        assert_eq!(animation.timeout(deadline, now), 40);
        assert_eq!(
            animation.timeout(deadline, deadline - Duration::from_nanos(1)),
            1
        );
        assert_eq!(animation.timeout(deadline, deadline), 0);
    }

    #[test]
    fn toast_deadline_is_event_owned_and_poll_timeout_rounds_up() {
        let (mut app, _) = fixture();
        assert_eq!(app.toast_timeout(1000, Instant::now()), 1000);
        app.toggle_chrome();
        let deadline = app.toasts.lock().unwrap().next_deadline().unwrap();
        for _ in 0..3 {
            paint_test_toasts(&mut app, size());
        }
        assert_eq!(app.toasts.lock().unwrap().next_deadline(), Some(deadline));
        assert_eq!(
            app.toast_timeout(1000, deadline - Duration::from_nanos(1)),
            1
        );
        assert_eq!(app.toast_timeout(1000, deadline), 0);
        {
            let mut toasts = app.toasts.lock().unwrap();
            assert!(toasts.expire(deadline));
            assert!(toasts.is_empty());
        }
        assert_eq!(app.toast_timeout(1000, deadline), 1000);
    }

    #[test]
    fn shared_toast_gesture_and_expiry_survive_session_switches_without_local_capture() {
        let (mut first, first_input) = fixture();
        let first_pointer = PointerFixture::new(&mut first);
        let (mut second, second_input) = fixture();
        let second_pointer = PointerFixture::new(&mut second);
        second.toasts = Arc::clone(&first.toasts);
        first.notify("Physical global notice".into());
        paint_test_toasts(&mut first, first_pointer.size);
        let (id, dismiss, deadline) = {
            let toasts = first.toasts.lock().unwrap();
            let card = toasts.cards().next().unwrap();
            (card.id, card.dismiss, toasts.next_deadline().unwrap())
        };
        first_pointer.send(&mut first, 0, dismiss.x, dismiss.y, false);
        first_pointer.send(&mut first, 2, dismiss.x, dismiss.y, false);
        assert!(first.pointer_capture.is_none());
        paint_test_toasts(&mut second, second_pointer.size);
        {
            let mut toasts = second.toasts.lock().unwrap();
            assert_eq!(toasts.cards().next().unwrap().id, id);
            assert_eq!(toasts.next_deadline(), Some(deadline));
            assert!(toasts.pointer_captured());
            toasts.expire(deadline);
            assert!(toasts.is_empty());
            assert!(toasts.pointer_captured());
        }
        let native = content(second_pointer.rects[0].1);
        second_pointer.send(&mut second, 0, native.x + 2, native.y + 8, true);
        assert!(second.toasts.lock().unwrap().pointer_captured());
        second_pointer.send(&mut second, 2, native.x + 2, native.y + 8, true);
        assert!(!second.toasts.lock().unwrap().pointer_captured());
        assert!(first.pointer_capture.is_none());
        assert!(second.pointer_capture.is_none());
        assert!(first_input.try_recv().is_err());
        assert!(second_input.try_recv().is_err());
        paint_test_toasts(&mut first, first_pointer.size);
        assert!(
            first.toasts.lock().unwrap().is_empty(),
            "expired notices must not revive"
        );
        let native = content(first_pointer.rects[0].1);
        first_pointer.send(&mut first, 0, native.x + 2, native.y + 8, false);
        first_pointer.send(&mut first, 0, native.x + 2, native.y + 8, true);
        assert_eq!(
            captured_mouse(&first_input).len(),
            2,
            "old App retained a stale UI capture"
        );
    }

    #[test]
    fn hidden_backend_completion_notifies_shared_queue_once_with_its_origin() {
        let (mut visible, _) = fixture();
        let (mut hidden, _) = fixture();
        hidden.namespace = (2, 7);
        hidden.backgrounded = true;
        hidden.notification_source = Some("session 1".into());
        hidden.tree = Arc::clone(&visible.tree);
        hidden.toasts = Arc::clone(&visible.toasts);
        let visible_owner = visible.tree_owner();
        hidden.tree.lock().unwrap().reconcile(
            vec![
                metadata_session(&visible, visible_owner, SessionStatus::Online),
                metadata_session(&hidden, hidden.tree_owner(), SessionStatus::Online),
            ],
            visible_owner,
        );
        hidden.tree.lock().unwrap().reconcile(
            vec![metadata_session(
                &visible,
                visible_owner,
                SessionStatus::Online,
            )],
            visible_owner,
        );
        hidden
            .jobs
            .submit("hidden-job".into(), || bail!("background failure"))
            .unwrap();
        wait_background(&mut hidden, "hidden-job");
        paint_test_toasts(&mut visible, size());
        let deadline = {
            let toasts = visible.toasts.lock().unwrap();
            assert_eq!(toasts.cards().count(), 1);
            let text = toasts
                .cards()
                .next()
                .unwrap()
                .surface
                .frame
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .map(|cell| cell.text.as_str())
                .collect::<String>();
            assert!(text.contains("session 1"));
            assert!(text.contains("hidden-job"));
            toasts.next_deadline().unwrap()
        };
        hidden.complete_background().unwrap();
        paint_test_toasts(&mut hidden, size());
        {
            let mut toasts = hidden.toasts.lock().unwrap();
            assert_eq!(toasts.cards().count(), 1);
            assert_eq!(toasts.next_deadline(), Some(deadline));
            toasts.expire(deadline);
        }
        paint_test_toasts(&mut visible, size());
        paint_test_toasts(&mut hidden, size());
        assert!(visible.toasts.lock().unwrap().is_empty());
        assert!(hidden.toasts.lock().unwrap().is_empty());
    }

    #[test]
    fn toast_pointer_capture_does_not_steal_keyboard_or_dismiss_underlying_modal() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        app.copy_focused_selection().unwrap();
        capture.respond(serde_json::json!({"copied":false, "contents":[]}));
        let mut clipboard = Vec::new();
        app.selection_clipboard(&mut clipboard).unwrap();
        assert!(clipboard.is_empty());
        assert!(!app.toasts.lock().unwrap().is_empty());
        paint_test_toasts(&mut app, pointer.size);
        let (body, dismiss) = {
            let toasts = app.toasts.lock().unwrap();
            let card = toasts.cards().next().unwrap();
            (card.body, card.dismiss)
        };
        pointer.send(&mut app, 35, body.x, body.y, false);
        assert!(app.pointer_capture.is_none(), "hover must not capture");
        pointer.send(&mut app, 0, body.x, body.y, false);
        pointer.send(&mut app, 0, 0, pointer.size.rows - 1, true);
        assert!(!app.toasts.lock().unwrap().is_empty());
        assert!(capture.try_recv().is_err());
        app.key(key("KeyX"), &mut Vec::new(), pointer.size, &pointer.rects)
            .unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.key == "KeyX"));
        app.open_form(FormKind::ProjectDetails, String::new())
            .unwrap();
        pointer.send(&mut app, 0, dismiss.x, dismiss.y, false);
        pointer.send(&mut app, 0, dismiss.x, dismiss.y, true);
        assert!(app.toasts.lock().unwrap().is_empty());
        assert!(app.state.form.is_some());
        assert_eq!(app.overlays.keyboard_owner(), Some("ui:modal"));
        assert!(capture.try_recv().is_err());

        app.notify("Another notice".into());
        paint_test_toasts(&mut app, pointer.size);
        let dismiss = {
            let toasts = app.toasts.lock().unwrap();
            let card = toasts.cards().next().unwrap();
            card.dismiss
        };
        pointer.send(&mut app, 0, dismiss.x, dismiss.y, false);
        {
            let mut toasts = app.toasts.lock().unwrap();
            assert!(toasts.pointer_captured());
            let deadline = toasts.next_deadline().unwrap();
            toasts.expire(deadline);
            assert!(toasts.is_empty());
        }
        pointer.send(&mut app, 0, 0, pointer.size.rows - 1, true);
        assert!(!app.toasts.lock().unwrap().pointer_captured());
        assert!(app.pointer_capture.is_none());
        assert!(app.state.form.is_some());
        assert!(capture.try_recv().is_err(), "expired toast release leaked");
    }

    #[test]
    fn toast_appearing_mid_native_or_divider_gesture_preserves_existing_owner() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let native = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, native.x + 2, native.y + 8, false);
        app.notify("Appeared during native drag".into());
        paint_test_toasts(&mut app, pointer.size);
        let dismiss = {
            let toasts = app.toasts.lock().unwrap();
            let card = toasts.cards().next().unwrap();
            card.dismiss
        };
        pointer.send(&mut app, 32, dismiss.x, dismiss.y, false);
        pointer.send(&mut app, 0, dismiss.x, dismiss.y, true);
        {
            let toasts = app.toasts.lock().unwrap();
            assert!(!toasts.is_empty());
            assert!(!toasts.pointer_captured());
        }
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|(pane, _, _)| pane == "focused-pane"));
        assert_eq!(events.last().unwrap().2.action, MouseAction::Release);

        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        let divider = resize_divider(&app, pointer.size);
        pointer.send(&mut app, 0, divider.x, 3, false);
        assert!(matches!(app.pointer_capture, Some(PointerCapture::Resize)));
        app.notify("Appeared during divider drag".into());
        paint_test_toasts(&mut app, pointer.size);
        pointer.send(&mut app, 0, divider.x, 3, true);
        {
            let toasts = app.toasts.lock().unwrap();
            assert!(!toasts.is_empty());
            assert!(!toasts.pointer_captured());
        }
        assert!(!app.resize_capturing());
        assert!(!app.jobs.contains("manage:window:resize-window"));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn outside_modal_click_cancels_without_starting_a_native_gesture() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        app.open_form(FormKind::ProjectDetails, String::new())
            .unwrap();
        let placement = OverlayPlacement::centered(pointer.size, 30, 10);
        app.overlays
            .present("ui:modal", placement, OverlayPolicy::MODAL, None);
        let native = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, native.x + 1, native.y + 1, false);
        assert!(app.state.form.is_none());
        pointer.send(&mut app, 0, native.x + 2, native.y + 2, true);
        assert!(
            captured_mouse(&capture).is_empty(),
            "dismissal press/release leaked through"
        );
    }
    fn captured_mouse(capture: &updates::Capture) -> Vec<(String, String, MouseEvent)> {
        let mut events = Vec::new();
        while let Ok(operation) = capture.try_recv() {
            if let Operation::Pane {
                pane_id,
                request: PaneRequest::Mouse { view_id, event },
            } = operation
            {
                events.push((pane_id, view_id, event));
            }
        }
        events
    }
    #[test]
    fn physical_drag_and_release_stay_in_original_pane_and_clip_to_its_edge() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 8, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 40, beta.x + 2, beta.y + 2, false);
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|(pane, view, _)| pane == "focused-pane" && view == "focused-view")
        );
        assert_eq!(events[0].2.action, MouseAction::Press);
        assert_eq!(events[1].2.action, MouseAction::Motion);
        let edge = mouse_position(alpha.width - 1, 2, alpha.size(pointer.size), size()).unwrap();
        assert_eq!(events[1].2.x.to_bits(), edge.0.to_bits());
        assert_eq!(events[1].2.y.to_bits(), edge.1.to_bits());
        assert_eq!(app.state.focused.as_deref(), Some("focused-pane"));
        pointer.send(&mut app, 8, beta.x + 2, beta.y + 2, true);
        let released = captured_mouse(&capture);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, "focused-pane");
        assert_eq!(released[0].2.action, MouseAction::Release);
        assert_eq!(released[0].2.modifiers, 4);
        assert_eq!(released[0].2.x.to_bits(), edge.0.to_bits());
        assert!(app.pressed_pointers.borrow().is_empty());
        assert!(app.pointer_capture.is_none());
    }
    #[test]
    fn captured_gesture_clips_to_actual_frame_when_owner_size_is_smaller_than_projection() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        let actual = Size {
            cols: 10,
            rows: 6,
            ..size()
        };
        app.views.get_mut("focused-pane").unwrap().frame = Some(blank(actual));
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = true;
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 32, beta.x + 2, beta.y + 2, false);
        pointer.send(&mut app, 0, beta.x + 2, beta.y + 2, true);
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 3);
        let edge = mouse_position(actual.cols - 1, 2, alpha.size(pointer.size), actual).unwrap();
        assert!(
            events
                .iter()
                .all(|(pane, view, _)| pane == "focused-pane" && view == "focused-view")
        );
        assert_eq!(events[1].2.action, MouseAction::Motion);
        assert_eq!(events[2].2.action, MouseAction::Release);
        assert_eq!(events[1].2.x.to_bits(), edge.0.to_bits());
        assert_eq!(events[2].2.x.to_bits(), edge.0.to_bits());
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn full_normal_budget_retains_physical_release_until_local_admission() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        let mut held = (0..2048)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            pointer
                .try_send(&mut app, 0, beta.x + 2, beta.y + 2, true)
                .is_err()
        );
        assert!(app.pointer_capture.is_none());
        assert!(app.pressed_pointers.borrow()[0].release.is_some());
        assert!(app.flush_pointer_releases().is_err());
        assert!(capture.try_recv().is_err());
        held.pop();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "focused-pane");
        assert_eq!(events[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn failed_release_admission_waits_for_accepted_deferred_press_before_completing_it() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        let held = (0..2047)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            pointer
                .try_send(&mut app, 0, alpha.x + 3, alpha.y + 2, true)
                .is_err()
        );
        drop(held);
        app.flush_pointer_releases().unwrap();
        assert!(
            capture.try_recv().is_err(),
            "release preceded its held press"
        );
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        assert_eq!(captured_mouse(&capture)[0].2.action, MouseAction::Press);
        app.flush_pointer_releases().unwrap();
        let released = captured_mouse(&capture);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn native_prefix_release_never_overtakes_accepted_paste_barrier_suffix() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        for _ in 0..64 {
            pointer.send(&mut app, 32, alpha.x + 3, alpha.y + 2, false);
        }
        assert!(
            pointer
                .try_send(&mut app, 0, alpha.x + 3, alpha.y + 2, true)
                .is_err()
        );
        app.flush_pointer_releases().unwrap();
        assert!(
            capture.try_recv().is_err(),
            "release overtook accepted suffix"
        );
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 65);
        assert!(
            events[..64]
                .iter()
                .all(|(_, _, event)| event.action == MouseAction::Motion)
        );
        assert_eq!(events[64].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn completed_shell_key_release_is_not_forwarded_to_dead_or_new_execution() {
        for snapshot_first in [false, true] {
            let (mut app, capture) = fixture();
            let _pointer = PointerFixture::new(&mut app);
            let mut exit = key("KeyD");
            exit.modifiers = 2;
            app.key(exit.clone(), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. },
            } if pane_id == "focused-pane" && event.action == KeyAction::Press));
            if snapshot_first {
                app.views
                    .get_mut("focused-pane")
                    .unwrap()
                    .metadata
                    .lifecycle = "exited".into();
            } else {
                app.registry.panes[0].status = PaneStatus::Exited;
            }
            app.state.focused = Some("beta".into());
            exit.action = KeyAction::Release;
            app.key(exit, &mut Vec::new(), size(), &[]).unwrap();
            assert!(capture.try_recv().is_err());
            assert!(!app.held.contains_key("KeyD"));
            assert!(app.input_failures.is_empty());
            app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Focus { focused: true, .. },
            } if pane_id == "beta"));
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. },
            } if pane_id == "beta" && event.action == KeyAction::Press));
            assert!(capture.try_recv().is_err());
        }
    }
    #[test]
    fn completed_shell_retirement_discards_pending_focus_but_detaches_and_keeps_peer_input() {
        for snapshot_first in [false, true] {
            let (mut app, capture) = fixture();
            let _pointer = PointerFixture::new(&mut app);
            // Completion can arrive after retirement captured a running focus.
            app.retire_view("focused-pane", true).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                request: PaneRequest::Update { spec, .. }, ..
            } if !spec.visible && !spec.interactive && !spec.clipboard));
            if snapshot_first {
                app.views
                    .get_mut("focused-pane")
                    .unwrap()
                    .metadata
                    .lifecycle = "exited".into();
            } else {
                app.registry.panes[0].status = PaneStatus::Exited;
            }
            app.focus_out_pending
                .insert(("focused-pane".into(), "focused-view".into()));
            app.notify_focus().unwrap();
            assert!(capture.try_recv().is_err());
            app.retire_view("focused-pane", true).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Detach { view_id },
            } if pane_id == "focused-pane" && view_id == "focused-view"));
            app.reap_retiring().unwrap();
            assert!(app.retiring.is_empty());
            assert!(app.input_failures.is_empty());
            assert!(app.view_failures.is_empty());
            app.state.focused = Some("beta".into());
            app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Focus { focused: true, .. },
            } if pane_id == "beta"));
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { .. },
            } if pane_id == "beta"));
            assert!(capture.try_recv().is_err());
        }
    }
    #[test]
    fn pending_and_failed_retirement_never_block_unrelated_pane_input() {
        for completion in 0..3 {
            let (mut app, capture) = fixture();
            attach_fixture(&mut app);
            attach_named_fixture(&mut app, "beta", "beta-view");
            let attached = app.views.remove("focused-pane").unwrap();
            let (done, receipt) = mpsc::channel();
            app.retiring.push(RetiringView { attached, receipt });
            let rect = Rect {
                x: 0,
                y: 0,
                width: size().cols,
                height: size().rows,
            };
            let rects = vec![("focused-pane".into(), rect), ("beta".into(), rect)];
            app.sync_views(size(), &rects).unwrap();
            assert!(!app.jobs.contains("attach:focused-pane"));
            while let Ok(operation) = capture.try_recv() {
                assert!(matches!(operation, Operation::Pane { pane_id, .. } if pane_id == "beta"));
            }
            match completion {
                0 => done.send(Ok(())).unwrap(),
                1 => done
                    .send(Err(anyhow::anyhow!("fixture detach rejected")))
                    .unwrap(),
                _ => drop(done),
            }
            app.reap_retiring().unwrap();
            app.sync_views(size(), &rects).unwrap();
            if completion == 0 {
                assert!(app.jobs.contains("attach:focused-pane"));
                assert!(!app.view_failures.contains_key("focused-pane"));
            } else {
                assert!(!app.jobs.contains("attach:focused-pane"));
                assert!(app.view_failures.contains_key("focused-pane"));
            }
            app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
            assert!(matches!(
                capture.try_recv().unwrap(),
                Operation::Pane {
                    pane_id,
                    request: PaneRequest::Key { view_id, event },
                } if pane_id == "beta" && view_id == "beta-view" && event.action == KeyAction::Press
            ));
            assert!(capture.try_recv().is_err());
        }
    }
    #[test]
    fn retiring_pressed_view_queues_original_release_before_focus_and_detach() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 8, alpha.x + 2, alpha.y + 2, false);
        let press = captured_mouse(&capture).pop().unwrap().2;
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Mouse { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == MouseAction::Release && event.button == press.button
            && event.modifiers == press.modifiers && event.x.to_bits() == press.x.to_bits()
            && event.y.to_bits() == press.y.to_bits()));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Update { spec, .. }, ..
        } if spec.input && !spec.visible && !spec.interactive && !spec.clipboard));
        app.notify_focus().unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Focus { focused: false, .. },
                ..
            }
        ));
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Detach { .. },
                ..
            }
        ));
        app.reap_retiring().unwrap();
        pointer.send(&mut app, 8, beta.x + 2, beta.y + 2, true);
        assert!(
            capture.try_recv().is_err(),
            "physical release leaked to newly visible pane"
        );
        assert!(app.retiring.is_empty());
        assert_eq!(app.registry.panes[0].status, PaneStatus::Running);
    }
    #[test]
    fn empty_paste_listing_releases_suffix_and_unrelated_replies_keep_origin() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        app.enqueue(
            "focused-pane",
            PaneRequest::Key {
                view_id: "focused-view".into(),
                event: key("KeyA"),
            },
        )
        .unwrap();
        app.paste_reply(b"10;rgb:ffff/ffff/ffff").unwrap();
        assert_eq!(app.paste_target.as_ref().unwrap().0, "focused-pane");
        assert!(capture.try_recv().is_err());
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        assert!(app.paste_offer.is_none() && app.paste_target.is_none());
        assert!(app.deferred.borrow().is_empty());
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.key == "KeyA"));
    }
    #[test]
    fn malformed_listing_drops_only_origin_suffix_and_never_invents_mouse_release() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, true);
        assert_eq!(app.deferred.borrow().len(), 2);
        assert!(app.pressed_pointers.borrow().is_empty());
        let error = app
            .paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<updates::InputRejected>()
                .unwrap()
                .0
                .not_dispatched,
            2
        );
        app.scope_input_result(Err(error)).unwrap();
        app.flush_pointer_releases().unwrap();
        assert!(capture.try_recv().is_err());
        assert!(
            app.enqueue(
                "focused-pane",
                PaneRequest::Key {
                    view_id: "focused-view".into(),
                    event: key("KeyB"),
                }
            )
            .is_err()
        );
        app.enqueue(
            "beta",
            PaneRequest::Key {
                view_id: "beta-view".into(),
                event: key("KeyB"),
            },
        )
        .unwrap();
        assert!(
            matches!(capture.try_recv().unwrap(), Operation::Pane { pane_id, .. } if pane_id == "beta")
        );
    }
    #[test]
    fn paste_failure_completes_queued_prefix_press_but_discards_deferred_release() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        pointer.send(&mut app, 0, alpha.x + 3, alpha.y + 2, true);
        let failed = app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;");
        app.scope_input_result(failed).unwrap();
        app.flush_pointer_releases().unwrap();
        let events = captured_mouse(&capture);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "focused-pane");
        assert_eq!(events[0].2.action, MouseAction::Release);
        assert!(app.pressed_pointers.borrow().is_empty());
    }
    #[test]
    fn accepted_device_response_settles_before_eligibility_revoke() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        let mut pending = consent(false);
        pending.pane = "focused-pane".into();
        pending.view = "focused-view".into();
        pending.reservation = Some(
            app.input
                .reserve_response(&pending.pane, &pending.view, &pending.state.id)
                .unwrap(),
        );
        app.consent = Some(pending);
        app.finish_consent(ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "application/octet-stream".into(),
                data: b"\0origin\xff".to_vec(),
            }],
            available: Vec::new(),
            remember: false,
        })
        .unwrap();
        app.retire_view("focused-pane", true).unwrap();
        assert!(app.views.contains_key("focused-pane"));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Clipboard { view_id, response, .. },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && response.contents[0].data == b"\0origin\xff"));
        assert!(
            capture.try_recv().is_err(),
            "revocation overtook an accepted device response"
        );
        app.reap_responses();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Update { .. },
                ..
            }
        ));
        app.retire_view("focused-pane", true).unwrap();
        capture.try_recv().unwrap();
        capture.try_recv().unwrap();
        app.reap_retiring().unwrap();
        assert!(app.retiring.is_empty());
    }
    #[test]
    fn retirement_compound_rejects_without_partial_focus_and_retries_only_local_intent() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        let mut held = (0..2047)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Update { .. },
                ..
            }
        ));
        assert!(app.retire_view("focused-pane", true).is_err());
        assert!(app.views.contains_key("focused-pane"));
        assert!(
            capture.try_recv().is_err(),
            "partial focus escaped failed compound admission"
        );
        held.pop();
        app.retire_view("focused-pane", true).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Focus { focused: false, .. },
                ..
            }
        ));
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Detach { .. },
                ..
            }
        ));
        app.reap_retiring().unwrap();
        assert!(app.retiring.is_empty());
    }
    fn wait_background(app: &mut App, key: &str) {
        while app.jobs.contains(key) {
            let mut fd = libc::pollfd {
                fd: app.jobs.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert!(
                unsafe { libc::poll(&mut fd, 1, 5000) } > 0,
                "background completion did not wake UI"
            );
            app.complete_background().unwrap();
        }
    }
    #[test]
    fn newer_selection_intent_and_registry_snapshot_win_out_of_order_completions() {
        let (mut app, _) = fixture();
        attach_fixture(&mut app);
        app.registry.panes[0].workspace_id = Some("old".into());
        app.registry.panes[0].window_id = Some("old-window".into());
        let old = Workspace {
            id: "old".into(),
            name: "Old".into(),
            revision: 1,
            windows: vec![Window {
                id: "old-window".into(),
                name: "Old terminal".into(),
                revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "focused-pane".into(),
                }),
            }],
        };
        let new = Workspace {
            id: "new".into(),
            name: "New".into(),
            revision: 1,
            windows: vec![Window {
                id: "new-window".into(),
                name: "New terminal".into(),
                revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "new-pane".into(),
                }),
            }],
        };
        let older = RegistrySnapshot {
            revision: 3,
            workspaces: vec![old.clone()],
            panes: app.registry.panes.clone(),
        };
        let mut new_pane = app.registry.panes[0].clone();
        new_pane.id = "new-pane".into();
        new_pane.workspace_id = Some("new".into());
        new_pane.window_id = Some("new-window".into());
        let newer = RegistrySnapshot {
            revision: 7,
            workspaces: vec![old, new],
            panes: vec![app.registry.panes[0].clone(), new_pane],
        };
        app.registry = older.clone();
        app.state = Controller::new(&app.registry);
        app.selection_epoch = 2;
        app.management.insert(
            "manage:old".into(),
            ManagementIntent {
                epoch: 1,
                selection: None,
            },
        );
        app.management.insert(
            "manage:new".into(),
            ManagementIntent {
                epoch: 2,
                selection: None,
            },
        );
        let (release, waiting) = mpsc::sync_channel(1);
        app.jobs
            .submit("manage:old".into(), move || {
                waiting.recv().unwrap();
                Ok(Background::Management(management::Completion {
                    registry: older,
                    selection: Some(management::Selection {
                        workspace: "old".into(),
                        window: "old-window".into(),
                        pane: "focused-pane".into(),
                    }),
                }))
            })
            .unwrap();
        app.jobs
            .submit("manage:new".into(), move || {
                Ok(Background::Management(management::Completion {
                    registry: newer,
                    selection: Some(management::Selection {
                        workspace: "new".into(),
                        window: "new-window".into(),
                        pane: "new-pane".into(),
                    }),
                }))
            })
            .unwrap();
        wait_background(&mut app, "manage:new");
        assert_eq!(app.state.workspace.as_deref(), Some("new"));
        release.send(()).unwrap();
        wait_background(&mut app, "manage:old");
        assert_eq!(app.state.workspace.as_deref(), Some("new"));
        assert_eq!(app.registry.revision, 7);
        assert_eq!(app.registry.workspaces.len(), 2);
    }
    #[test]
    fn selection_generation_advances_when_new_selection_job_is_admitted() {
        let (mut app, _) = fixture();
        app.manage(
            management::Command::Single(Operation::LayoutUpdate {
                workspace_id: "old".into(),
                window_id: "old-window".into(),
                expected_revision: 1,
                layout: None,
            }),
            Some(management::Selection {
                workspace: "old".into(),
                window: "old-window".into(),
                pane: "pane".into(),
            }),
        )
        .unwrap();
        assert_eq!(app.selection_epoch, 1);
        app.manage(
            management::Command::Create {
                name: Some("new".into()),
                spec: PaneSpawnSpec {
                    argv: vec!["/bin/sh".into()],
                    cwd: app.defaults().unwrap().cwd.clone(),
                    home: app.defaults().unwrap().home.clone(),
                    size: size(),
                    title: None,
                    isolated_environment: true,
                },
            },
            None,
        )
        .unwrap();
        assert_eq!(app.selection_epoch, 2);
    }
    fn scroll(capture: &updates::Capture, pane: &str, view: &str) -> i64 {
        match capture
            .try_recv()
            .expect("expected admitted history navigation")
        {
            Operation::Pane {
                pane_id,
                request: PaneRequest::Scroll { view_id, delta },
            } => {
                assert_eq!(pane_id, pane);
                assert_eq!(view_id, view);
                delta
            }
            other => panic!("history navigation dispatched application input: {other:?}"),
        }
    }

    #[test]
    fn history_navigation_does_not_capture_application_key_lifecycles_or_paste() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        physical_keys(&mut app, b"\x1b[5;2~").unwrap();
        assert!(scroll(&capture, "focused-pane", "focused-view") > 0);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .scrollback_offset = 12;
        for (name, expected) in [("ArrowUp", 1), ("ArrowDown", -1), ("Home", i64::MAX)] {
            app.key(key(name), &mut Vec::new(), size(), &[]).unwrap();
            assert_eq!(scroll(&capture, "focused-pane", "focused-view"), expected);
        }
        physical_keys(&mut app, b"\x1b[120;1:1u\x1b[120;1:2u\x1b[120;1:3u").unwrap();
        for action in [KeyAction::Press, KeyAction::Repeat, KeyAction::Release] {
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { view_id, event }
            } if pane_id == "focused-pane" && view_id == "focused-view"
                && event.key == "KeyX" && event.action == action));
        }
        physical_keys(&mut app, b"\x02\x02").unwrap();
        for action in [KeyAction::Press, KeyAction::Release] {
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { view_id, event }
            } if pane_id == "focused-pane" && view_id == "focused-view"
                && event.key == "KeyB" && event.modifiers == 2 && event.action == action));
        }
        app.paste("paste sentinel\nλ".into()).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Paste { view_id, data }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && data == "paste sentinel\nλ".as_bytes()));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn historical_paste_offer_keeps_its_original_target_when_focus_changes() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.enter_history(&[]).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        app.state.focused = Some("other-pane".into());
        app.paste_reply(b"5522;type=read:status=DATA:pw=cHc=:mime=Lg==;dGV4dC9wbGFpbg==")
            .unwrap();
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        let consent = app.consent.as_ref().unwrap();
        assert_eq!(consent.pane, "focused-pane");
        assert_eq!(consent.view, "focused-view");
        assert!(!consent.state.allowed);
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn reaching_live_output_retires_history_context_and_hints() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.enter_history(&[]).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        for offset in [9, 0] {
            let attached = app.views.get_mut("focused-pane").unwrap();
            let mut next = attached.frame.clone().unwrap();
            next.scrollback_total = 400;
            next.scrollback_offset = offset;
            let revision = attached.revision + 1;
            let bytes = scene_wire::encode(&next, revision, None, 0, &[]).unwrap();
            let mut metadata = attached.metadata.clone();
            metadata.revision = revision;
            metadata.snapshot_revision += 1;
            attached
                .apply(Snapshot {
                    metadata,
                    scene: Some(scene_wire::decode(&bytes).unwrap()),
                })
                .unwrap();
            assert_eq!(app.focused_history(), offset != 0);
            assert_eq!(
                app.guide_context(),
                if offset == 0 {
                    crate::keyguide::Context::Application
                } else {
                    crate::keyguide::Context::History
                }
            );
        }
        app.enter_history(&[]).unwrap();
        let page = scroll(&capture, "focused-pane", "focused-view");
        app.scroll_history("focused-pane", -page, true).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        assert!(
            !app.focused_history(),
            "coalesced up/down retained scroll mode"
        );
    }

    #[test]
    fn all_live_return_keys_consume_their_lifecycle_without_an_application_enter_or_escape() {
        for name in ["Escape", "Enter", "End"] {
            let (mut app, capture) = fixture();
            attach_fixture(&mut app);
            app.enter_history(&[]).unwrap();
            scroll(&capture, "focused-pane", "focused-view");
            let event = key(name);
            app.key(event.clone(), &mut Vec::new(), size(), &[])
                .unwrap();
            assert_eq!(scroll(&capture, "focused-pane", "focused-view"), i64::MIN);
            for action in [KeyAction::Repeat, KeyAction::Release] {
                app.key(
                    KeyEvent {
                        action,
                        ..event.clone()
                    },
                    &mut Vec::new(),
                    size(),
                    &[],
                )
                .unwrap();
            }
            assert!(!app.focused_history());
            assert!(capture.try_recv().is_err());
        }
    }

    #[test]
    fn history_is_view_local_and_held_application_keys_still_release_at_their_origin() {
        let (mut app, capture) = fixture();
        let _pointer = PointerFixture::new(&mut app);
        app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Key { event, .. }, ..
    } if event.action == KeyAction::Press));
        app.enter_history(&[]).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        app.key(
            KeyEvent {
                action: KeyAction::Repeat,
                ..key("KeyA")
            },
            &mut Vec::new(),
            size(),
            &[],
        )
        .unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == KeyAction::Repeat));
        app.state.focused = Some("beta".into());
        app.notified_focus = Some(("beta".into(), "beta-view".into()));
        app.key(
            KeyEvent {
                action: KeyAction::Repeat,
                ..key("KeyA")
            },
            &mut Vec::new(),
            size(),
            &[],
        )
        .unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == KeyAction::Repeat));
        app.key(
            KeyEvent {
                action: KeyAction::Release,
                ..key("KeyA")
            },
            &mut Vec::new(),
            size(),
            &[],
        )
        .unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        pane_id, request: PaneRequest::Key { view_id, event }
    } if pane_id == "focused-pane" && view_id == "focused-view" && event.action == KeyAction::Release));
        assert!(!app.focused_history());
        app.key(key("KeyZ"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        pane_id, request: PaneRequest::Key { .. }
    } if pane_id == "beta"));
        app.state.focused = Some("focused-pane".into());
        app.notified_focus = Some(("focused-pane".into(), "focused-view".into()));
        assert!(app.focused_history());
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .scrollback_offset = 12;
        app.key(key("End"), &mut Vec::new(), size(), &[]).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        app.key(key("KeyQ"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.key == "KeyQ" && event.action == KeyAction::Press));
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .scrollback_offset = 0;
        assert!(!app.focused_history());
    }

    #[test]
    fn read_only_views_can_navigate_but_never_deliver_application_input() {
        let (mut app, capture) = fixture();
        app.options.read_only = true;
        attach_fixture(&mut app);
        physical_keys(&mut app, b"\x02s").unwrap();
        assert!(scroll(&capture, "focused-pane", "focused-view") > 0);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .scrollback_offset = 50;
        physical_keys(&mut app, b"\x1b[6;2~").unwrap();
        assert!(scroll(&capture, "focused-pane", "focused-view") < 0);
        physical_keys(&mut app, b"typing\r").unwrap();
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), i64::MIN);
        app.paste("blocked".into()).unwrap();
        assert!(capture.try_recv().is_err());
        assert!(!app.views["focused-pane"].spec.input);
    }

    #[test]
    fn existing_workspace_shortcuts_remain_available_while_a_view_inspects_history() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.registry.workspaces = ["first", "second"]
            .into_iter()
            .map(|id| Workspace {
                id: id.into(),
                name: id.into(),
                revision: 1,
                windows: if id == "first" {
                    vec![Window {
                        id: "first-window".into(),
                        name: "First terminal".into(),
                        revision: 1,
                        layout: Some(LayoutNode::Leaf {
                            pane_id: "focused-pane".into(),
                        }),
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect();
        app.registry.panes[0].workspace_id = Some("first".into());
        app.registry.panes[0].window_id = Some("first-window".into());
        app.state = Controller::new(&app.registry);
        app.enter_history(&[]).unwrap();
        scroll(&capture, "focused-pane", "focused-view");
        physical_keys(&mut app, b"\x02]").unwrap();
        assert_eq!(app.state.workspace.as_deref(), Some("second"));
        physical_keys(&mut app, b"\x02[").unwrap();
        assert_eq!(app.state.workspace.as_deref(), Some("first"));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn physical_wheels_follow_exact_native_mouse_ownership_and_shift_override() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = false;
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 64, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), 3);

        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = true;
        pointer.send(&mut app, 64, alpha.x + 2, alpha.y + 2, false);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Mouse { event, .. }, ..
    } if event.button == MouseButton::WheelUp && event.modifiers == 0));
        pointer.send(&mut app, 68, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), 3);
        assert!(capture.try_recv().is_err());

        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .alternate_screen = true;
        pointer.send(&mut app, 65, alpha.x + 2, alpha.y + 2, false);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Mouse { event, .. }, ..
    } if event.button == MouseButton::WheelDown && event.modifiers == 0));
        pointer.send(&mut app, 68, alpha.x + 2, alpha.y + 2, false);
        physical_keys(&mut app, b"\x1b[5;2~").unwrap();
        assert!(!app.focused_history());
        assert!(
            capture.try_recv().is_err(),
            "alternate screen exposed primary history"
        );
    }

    #[test]
    fn unknown_native_mouse_ownership_never_intercepts_an_ordinary_wheel() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .metadata
            .capabilities
            .native_mouse_reporting = false;
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 64, alpha.x + 2, alpha.y + 2, false);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Mouse { event, .. }, ..
    } if event.button == MouseButton::WheelUp && event.modifiers == 0));
        assert!(!app.focused_history());
        pointer.send(&mut app, 68, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), 3);
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn read_only_wheel_history_needs_no_application_mouse_input_grant() {
        let (mut app, capture) = fixture();
        app.options.read_only = true;
        let pointer = PointerFixture::new(&mut app);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = false;
        assert!(!app.views["focused-pane"].spec.input);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 64, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), 3);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = true;
        pointer.send(&mut app, 64, alpha.x + 2, alpha.y + 2, false);
        assert!(capture.try_recv().is_err());
        pointer.send(&mut app, 68, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), 3);
    }

    #[test]
    fn history_navigation_preserves_accepted_pointer_release_and_paste_suffix_fifo() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, false);
        assert_eq!(captured_mouse(&capture).len(), 1);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        app.enter_history(&[]).unwrap();
        pointer.send(&mut app, 0, alpha.x + 2, alpha.y + 2, true);
        physical_keys(&mut app, b"q").unwrap();
        assert!(capture.try_recv().is_err());
        app.paste_reply(b"5522;type=read:status=DONE:pw=cHc=;")
            .unwrap();
        assert!(scroll(&capture, "focused-pane", "focused-view") > 0);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Mouse { event, .. }, ..
    } if event.action == MouseAction::Release));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.text == "q" && event.action == KeyAction::Press));
        assert!(capture.try_recv().is_err());
        assert!(app.pointer_capture.is_none());
        assert!(app.pressed_pointers.borrow().is_empty());
    }

    fn selected_metadata() -> SelectionSnapshot {
        SelectionSnapshot {
            active: true,
            dragging: false,
            rectangle: false,
            autoscroll: 0,
            spans: vec![SelectionSpan {
                row: 0,
                start: 0,
                end: 2,
            }],
        }
    }
    fn copy_contents(data: Vec<u8>) -> serde_json::Value {
        serde_json::json!({
            "copied": true,
            "contents": [ClipboardContent { mime: "text/plain".into(), data }],
        })
    }

    #[test]
    fn local_selection_respects_application_mouse_unknown_capability_and_shift_override() {
        for (known, reporting, shifted, history, read_only, selected) in [
            (true, true, false, false, false, false),
            (true, true, true, false, false, true),
            (false, false, false, false, false, false),
            (false, false, true, false, false, true),
            (true, false, false, false, false, true),
            (true, true, false, true, false, true),
            (true, false, false, false, true, true),
            (true, true, false, false, true, false),
        ] {
            let (mut app, capture) = fixture();
            app.options.read_only = read_only;
            app.options.clipboard_policy = "deny".into();
            let pointer = PointerFixture::new(&mut app);
            let attached = app.views.get_mut("focused-pane").unwrap();
            attached.metadata.capabilities.native_mouse_reporting = known;
            attached.frame.as_mut().unwrap().mouse_reporting = reporting;
            attached.history = history;
            let alpha = content(pointer.rects[0].1);
            let button = if shifted { 4 } else { 0 };
            pointer.send(&mut app, button, alpha.x + 2, alpha.y + 2, false);
            assert_eq!(
                matches!(app.pointer_capture, Some(PointerCapture::Selection(_))),
                selected
            );
            assert_eq!(
                !app.pressed_pointers.borrow().is_empty(),
                !selected && !read_only
            );
            pointer.send(&mut app, button, alpha.x + 3, alpha.y + 2, true);
            let application = captured_mouse(&capture);
            assert_eq!(!application.is_empty(), !selected && !read_only);
            assert!(app.pointer_capture.is_none());
            assert!(app.pressed_pointers.borrow().is_empty());
        }
    }

    #[test]
    fn read_only_selection_crosses_panes_and_chrome_without_retargeting_and_exports_exact_bytes() {
        let (mut app, capture) = fixture();
        app.options.read_only = true;
        let pointer = PointerFixture::new(&mut app);
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .frame
            .as_mut()
            .unwrap()
            .mouse_reporting = false;
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        let source_rows = app.views["focused-pane"]
            .frame
            .as_ref()
            .unwrap()
            .rows
            .as_ptr();
        pointer.send(&mut app, 8, alpha.x + 2, alpha.y + 2, false);
        pointer.send(&mut app, 40, beta.x + 2, beta.y + 2, false);
        pointer.send(&mut app, 8, 0, 0, true);
        for _ in 0..3 {
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Select { view_id, .. },
            } if pane_id == "focused-pane" && view_id == "focused-view"));
        }
        assert!(
            matches!(capture.respond(copy_contents("é\n".as_bytes().to_vec())), Operation::Pane {
            pane_id, request: PaneRequest::CopySelection { view_id },
        } if pane_id == "focused-pane" && view_id == "focused-view")
        );
        let mut out = Vec::new();
        app.selection_clipboard(&mut out).unwrap();
        assert_eq!(out, b"\x1b]52;c;w6kK\x1b\\");
        assert!(!app.views["focused-pane"].spec.input);
        assert!(!app.views["focused-pane"].spec.clipboard);
        assert_eq!(app.state.focused.as_deref(), Some("focused-pane"));
        assert!(app.pressed_pointers.borrow().is_empty());
        assert!(app.pointer_capture.is_none());
        assert_eq!(
            app.views["focused-pane"]
                .frame
                .as_ref()
                .unwrap()
                .rows
                .as_ptr(),
            source_rows
        );
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn delayed_copy_cannot_export_after_clear_new_selection_focus_roundtrip_or_view_retirement() {
        for change in 0..6 {
            let (mut app, capture) = fixture();
            let pointer = PointerFixture::new(&mut app);
            app.views
                .get_mut("focused-pane")
                .unwrap()
                .metadata
                .selection = Some(selected_metadata());
            app.copy_focused_selection().unwrap();
            match change {
                0 => app.clear_selection("focused-pane").unwrap(),
                1 => {
                    app.focus("beta".into());
                    app.focus("focused-pane".into());
                }
                2 => {
                    app.retire_view("focused-pane", true).unwrap();
                    assert!(app.views["focused-pane"].retirement_requested);
                }
                3 => app.views.get_mut("focused-pane").unwrap().view = "replacement-view".into(),
                4 => {
                    let alpha = content(pointer.rects[0].1);
                    pointer.send(&mut app, 4, alpha.x + 1, alpha.y + 1, false);
                }
                5 => app.selection_focus_lost(),
                _ => unreachable!(),
            }
            capture.respond(copy_contents(b"stale".to_vec()));
            let mut out = Vec::new();
            app.selection_clipboard(&mut out).unwrap();
            assert!(
                out.is_empty(),
                "stale selection exported for lifecycle change {change}"
            );
        }
    }

    #[test]
    fn copy_policy_empty_and_oversized_results_do_not_write_host_clipboard() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.options.clipboard_policy = "deny".into();
        app.copy_focused_selection().unwrap();
        assert!(capture.try_recv().is_err());
        app.options.clipboard_policy = "ask".into();
        for value in [
            serde_json::json!({"copied":false, "contents":[]}),
            copy_contents(vec![b'x'; MAX_INPUT_BYTES + 1]),
        ] {
            app.copy_focused_selection().unwrap();
            capture.respond(value);
            let mut out = Vec::new();
            app.selection_clipboard(&mut out).unwrap();
            assert!(out.is_empty());
        }
        assert!(app.input_failures.is_empty());
    }

    #[test]
    fn selection_escape_is_local_normal_typing_clears_and_held_releases_keep_origin() {
        let (mut app, capture) = fixture();
        let _pointer = PointerFixture::new(&mut app);
        app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Key { .. },
                ..
            }
        ));
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .metadata
            .selection = Some(selected_metadata());
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert!(app.selection_spans("focused-pane").is_empty());
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Select { event, .. }, ..
        } if event.action == SelectionAction::Clear));
        for action in [KeyAction::Repeat, KeyAction::Release] {
            let mut escape = key("Escape");
            escape.action = action;
            app.key(escape, &mut Vec::new(), size(), &[]).unwrap();
        }
        assert!(
            capture.try_recv().is_err(),
            "selection Escape leaked into application"
        );
        app.focus("beta".into());
        let mut release = key("KeyA");
        release.action = KeyAction::Release;
        app.key(release, &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. },
        } if pane_id == "focused-pane" && event.action == KeyAction::Release));
        app.views.get_mut("beta").unwrap().metadata.selection = Some(selected_metadata());
        app.notified_focus = Some(("beta".into(), "beta-view".into()));
        app.key(key("KeyQ"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Select { event, .. }, ..
        } if event.action == SelectionAction::Clear));
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { .. },
        } if pane_id == "beta"));
    }

    #[test]
    fn history_selection_escape_clears_selection_before_leaving_history() {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        let view = app.views.get_mut("focused-pane").unwrap();
        view.history = true;
        view.metadata.selection = Some(selected_metadata());
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert!(app.focused_history());
        assert!(matches!(
            capture.try_recv().unwrap(),
            Operation::Pane {
                request: PaneRequest::Select { .. },
                ..
            }
        ));
        assert!(capture.try_recv().is_err());
        app.key(key("Escape"), &mut Vec::new(), size(), &[])
            .unwrap();
        assert_eq!(scroll(&capture, "focused-pane", "focused-view"), i64::MIN);
    }

    #[test]
    fn autoscroll_deadline_exists_only_for_an_active_native_edge_drag() {
        let (mut app, capture) = fixture();
        app.options.clipboard_policy = "deny".into();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        app.selection_tick().unwrap();
        assert_eq!(app.selection_timeout(1000), 1000);
        let mut selection = selected_metadata();
        selection.dragging = true;
        selection.autoscroll = -1;
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .metadata
            .selection = Some(selection);
        app.selection_tick().unwrap();
        assert!(app.selection_timeout(1000) <= 15);
        pointer.send(&mut app, 4, 0, 0, true);
        app.selection_tick().unwrap();
        assert_eq!(app.selection_timeout(1000), 1000);
        while capture.try_recv().is_ok() {}
    }

    #[test]
    fn locally_rejected_selection_release_waits_for_capacity_once_and_stops_edge_ticks() {
        let (mut app, capture) = fixture();
        app.options.read_only = true;
        app.options.clipboard_policy = "deny".into();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        capture.try_recv().unwrap();
        let mut held = (0..2048)
            .map(|_| {
                app.input
                    .reserve(Operation::Pane {
                        pane_id: "busy".into(),
                        request: PaneRequest::Key {
                            view_id: "busy-view".into(),
                            event: key("KeyA"),
                        },
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(pointer.try_send(&mut app, 4, 0, 0, true).is_err());
        assert!(app.pointer_capture.is_none());
        assert_eq!(app.selection_timeout(1000), 1000);
        assert!(capture.try_recv().is_err());
        held.pop();
        app.flush_selection_release().unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Select { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view" && event.action == SelectionAction::Release));
        app.flush_selection_release().unwrap();
        assert!(capture.try_recv().is_err());
        assert!(app.pressed_pointers.borrow().is_empty());
    }

    #[test]
    fn clipboard_output_failure_is_unknown_not_reported_as_success_or_replayed() {
        struct BrokenHost;
        impl Write for BrokenHost {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "fixture host closed",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.copy_focused_selection().unwrap();
        capture.respond(copy_contents(b"exact bytes".to_vec()));
        let error = app.selection_clipboard(&mut BrokenHost).unwrap_err();
        assert!(error.to_string().contains("outcome unknown"));
        let mut next = Vec::new();
        app.selection_clipboard(&mut next).unwrap();
        assert!(next.is_empty());
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn focus_retirement_stops_edge_drag_without_export_or_late_release_in_another_pane() {
        let (mut app, capture) = fixture();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        capture.try_recv().unwrap();
        let mut selection = selected_metadata();
        selection.dragging = true;
        selection.autoscroll = 1;
        app.views
            .get_mut("focused-pane")
            .unwrap()
            .metadata
            .selection = Some(selection);
        app.selection_tick().unwrap();
        assert!(app.selection_timeout(1000) <= 15);
        app.selection_focus_lost();
        app.flush_selection_release().unwrap();
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Select { event, .. },
        } if pane_id == "focused-pane" && event.action == SelectionAction::Release));
        assert_eq!(app.selection_timeout(1000), 1000);
        pointer.send(&mut app, 4, beta.x + 2, beta.y + 2, true);
        let mut out = Vec::new();
        app.selection_clipboard(&mut out).unwrap();
        assert!(out.is_empty());
        assert!(
            capture.try_recv().is_err(),
            "retired selection release reached a different pane"
        );
        assert!(app.pointer_capture.is_none());
    }

    #[test]
    fn selection_cannot_start_in_letterbox_but_captured_drag_keeps_outside_coordinates() {
        let (mut app, capture) = fixture();
        app.options.read_only = true;
        app.options.clipboard_policy = "deny".into();
        let pointer = PointerFixture::new(&mut app);
        let alpha = content(pointer.rects[0].1);
        let native = Size {
            cols: 10,
            rows: 6,
            ..size()
        };
        app.views.get_mut("focused-pane").unwrap().frame = Some(blank(native));
        pointer.send(&mut app, 4, alpha.x + 12, alpha.y + 2, false);
        pointer.send(&mut app, 4, alpha.x + 12, alpha.y + 2, true);
        assert!(capture.try_recv().is_err());
        pointer.send(&mut app, 4, alpha.x + 2, alpha.y + 2, false);
        capture.try_recv().unwrap();
        pointer.send(&mut app, 36, alpha.x + 12, alpha.y + 8, false);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Select { event, .. }, ..
        } if event.action == SelectionAction::Drag && event.x > f32::from(native.cols) && event.y > f32::from(native.rows)));
        pointer.send(&mut app, 4, 0, 0, true);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Select { event, .. }, ..
        } if event.action == SelectionAction::Release && event.x < 0.0 && event.y < 0.0));
        assert!(app.pointer_capture.is_none());
        assert!(capture.try_recv().is_err());
    }

    fn resize_fixture(app: &mut App) -> PointerFixture {
        let mut pointer = PointerFixture::new(app);
        app.registry.workspaces = vec![
            Workspace {
                id: "resize".into(),
                name: "resize".into(),
                revision: 7,
                windows: vec![Window {
                    id: "resize-window".into(),
                    name: "Resize terminal".into(),
                    revision: 7,
                    layout: Some(LayoutNode::Split {
                        axis: Axis::Horizontal,
                        ratio: 500,
                        first: Box::new(LayoutNode::Leaf {
                            pane_id: "focused-pane".into(),
                        }),
                        second: Box::new(LayoutNode::Leaf {
                            pane_id: "beta".into(),
                        }),
                    }),
                }],
            },
            Workspace {
                id: "other".into(),
                name: "other".into(),
                revision: 2,
                windows: Vec::new(),
            },
        ];
        for pane in &mut app.registry.panes {
            pane.workspace_id = Some("resize".into());
            pane.window_id = Some("resize-window".into());
        }
        app.state = Controller::new(&app.registry);
        pointer.rects = app.rects(pointer.size);
        pointer
    }

    fn resize_divider(app: &App, size: Size) -> Rect {
        let mut splits = Vec::new();
        crate::tiling::dividers(
            app.registry.workspaces[0].windows[0].layout.as_ref(),
            app.screen_geometry(size).1,
            &mut splits,
        );
        splits[0].divider
    }

    fn read_resize_request(stream: &mut std::os::unix::net::UnixStream) -> Operation {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        transport::test_wire::read_request(stream)
            .unwrap()
            .operation
    }

    fn accept_resize_request(registry: &mut RegistrySnapshot, operation: &Operation) {
        let Operation::LayoutUpdate {
            workspace_id,
            window_id,
            expected_revision,
            layout,
        } = operation
        else {
            panic!("expected resize layout mutation");
        };
        let workspace = registry
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == *workspace_id)
            .unwrap();
        let window = workspace
            .windows
            .iter_mut()
            .find(|window| window.id == *window_id)
            .unwrap();
        assert_eq!(window.revision, *expected_revision);
        window.revision += 1;
        workspace.revision += 1;
        window.layout = layout.clone();
        registry.revision += 1;
    }

    fn resize_reply(stream: &mut std::os::unix::net::UnixStream, registry: &RegistrySnapshot) {
        transport::test_wire::write_response(
            stream,
            &Response {
                version: PROTOCOL_VERSION,
                failure: None,
                ok: true,
                result: Some(
                    serde_json::to_value(RegistryResult {
                        registry: registry.clone(),
                        created_panes: Default::default(),
                        pane_id: None,
                        workspace_id: None,
                        window_id: None,
                    })
                    .unwrap()
                    .into(),
                ),
                error: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn sidebar_drag_is_local_in_read_only_clients_and_consumes_the_entire_gesture() {
        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        app.options.read_only = true;
        let before = serde_json::to_value(&app.registry).unwrap();
        let width = app.screen_geometry(pointer.size).0.width;
        pointer.send(&mut app, 35, width - 1, 3, false);
        assert!(app.sidebar_resize_hover);
        pointer.send(&mut app, 0, width - 1, 3, false);
        pointer.send(&mut app, 32, width - 1, 3, false);
        assert_eq!(app.screen_geometry(pointer.size).0.width, width);
        pointer.send(&mut app, 32, width + 8, 3, false);
        pointer.send(&mut app, 0, width + 8, 3, true);
        assert_eq!(app.screen_geometry(pointer.size).0.width, width + 9);
        assert!(app.pointer_capture.is_none());
        assert_eq!(serde_json::to_value(&app.registry).unwrap(), before);
        assert!(
            capture.try_recv().is_err(),
            "sidebar drag reached the backend"
        );
    }

    #[test]
    fn sidebar_drag_cancellation_tombstones_motion_and_release() {
        for escape in [false, true] {
            let (mut app, capture) = fixture();
            let pointer = resize_fixture(&mut app);
            let width = app.screen_geometry(pointer.size).0.width;
            pointer.send(&mut app, 0, width, 3, false);
            if escape {
                app.key(key("Escape"), &mut Vec::new(), pointer.size, &pointer.rects)
                    .unwrap();
            } else {
                app.host_focused = false;
            }
            pointer.send(&mut app, 32, width + 10, 3, false);
            pointer.send(&mut app, 0, width + 10, 3, true);
            assert_eq!(app.screen_geometry(pointer.size).0.width, width);
            assert!(app.pointer_capture.is_none());
            assert!(
                capture.try_recv().is_err(),
                "cancelled resize leaked native input"
            );
        }
    }

    #[test]
    fn resize_hover_obeys_modal_and_read_only_ownership_and_geometry_retirement() {
        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        let divider = resize_divider(&app, pointer.size);
        pointer.send(&mut app, 35, divider.x, 3, false);
        assert_eq!(app.resize_feedback(pointer.size), vec![divider]);
        app.overlays.present(
            "ui:modal",
            OverlayPlacement::centered(pointer.size, 30, 10),
            OverlayPolicy::MODAL,
            None,
        );
        pointer.send(&mut app, 35, divider.x, 3, false);
        assert!(app.resize_feedback(pointer.size).is_empty());
        app.overlays.remove("ui:modal");
        app.options.read_only = true;
        pointer.send(&mut app, 35, divider.x, 3, false);
        assert!(app.resize_feedback(pointer.size).is_empty());
        app.options.read_only = false;
        pointer.send(&mut app, 0, divider.x, 3, false);
        assert!(app.resize_capturing());
        app.state.sidebar.preferred_columns = Some(app.screen_geometry(pointer.size).0.width + 4);
        app.validate_resize(pointer.size);
        assert!(!app.resize_capturing());
        pointer.send(&mut app, 0, divider.x + 3, 5, true);
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn corner_pointer_drag_submits_both_axes_in_one_revision_guarded_layout() {
        use std::{os::unix::net::UnixListener, thread};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("corner.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let endpoint = Endpoint::unmanaged(&socket);
        let (mut app, capture) = fixture();
        app.socket = endpoint.clone();
        let mut pointer = resize_fixture(&mut app);
        let mut gamma = app.registry.panes[0].clone();
        gamma.id = "gamma".into();
        app.registry.panes.push(gamma);
        if let Some(LayoutNode::Split { first, .. }) =
            &mut app.registry.workspaces[0].windows[0].layout
        {
            **first = LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 500,
                first: Box::new(LayoutNode::Leaf {
                    pane_id: "focused-pane".into(),
                }),
                second: Box::new(LayoutNode::Leaf {
                    pane_id: "gamma".into(),
                }),
            };
        }
        pointer.rects = app.rects(pointer.size);
        let mut splits = Vec::new();
        crate::tiling::dividers(
            app.registry.workspaces[0].windows[0].layout.as_ref(),
            app.screen_geometry(pointer.size).1,
            &mut splits,
        );
        let (x, y) = (splits[0].divider.x, splits[1].divider.y - 2);
        pointer.send(&mut app, 35, x, y, false);
        assert_eq!(
            app.resize_feedback(pointer.size),
            vec![splits[0].divider, splits[1].divider]
        );
        pointer.send(&mut app, 0, x, y, false);
        pointer.send(&mut app, 32, x, y, false);
        assert!(
            !app.jobs.contains("manage:window:resize-window"),
            "near-edge press jumped"
        );
        let mut registry = app.registry.clone();
        let (observed, requests) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let operation = read_resize_request(&mut stream);
            accept_resize_request(&mut registry, &operation);
            observed.send(operation).unwrap();
            resize_reply(&mut stream, &registry);
        });
        pointer.send(&mut app, 32, x + 4, y + 3, false);
        pointer.send(&mut app, 0, x + 4, y + 3, true);
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            request,
            Operation::LayoutUpdate {
                expected_revision: 7,
                ..
            }
        ));
        wait_background(&mut app, "manage:window:resize-window");
        app.flush_resize().unwrap();
        server.join().unwrap();
        let mut resized = Vec::new();
        crate::tiling::dividers(
            app.registry.workspaces[0].windows[0].layout.as_ref(),
            app.screen_geometry(pointer.size).1,
            &mut resized,
        );
        assert_eq!(resized[0].divider.x, splits[0].divider.x + 4);
        assert_eq!(resized[1].divider.y, splits[1].divider.y + 3);
        assert_eq!(app.registry.workspaces[0].windows[0].revision, 8);
        assert!(!app.resize_capturing());
        assert!(
            capture.try_recv().is_err(),
            "corner drag leaked application mouse input"
        );
    }

    #[test]
    fn resize_coalesces_keyboard_repeats_and_captured_sidebar_release_after_inflight_commit() {
        use std::{os::unix::net::UnixListener, thread};
        for drag in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("resize.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (mut app, capture) = fixture();
            let endpoint = Endpoint::unmanaged(&socket);
            app.socket = endpoint.clone();
            let pointer = resize_fixture(&mut app);
            let initial = resize_divider(&app, pointer.size);
            let mut registry = app.registry.clone();
            let (observed, requests) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            let server = thread::spawn(move || {
                for index in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let operation = read_resize_request(&mut stream);
                    observed.send(operation.clone()).unwrap();
                    if index == 0 {
                        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    }
                    accept_resize_request(&mut registry, &operation);
                    resize_reply(&mut stream, &registry);
                }
            });
            if drag {
                pointer.send(&mut app, 0, initial.x, 3, false);
                pointer.send(&mut app, 32, initial.x + 3, 3, false);
            } else {
                app.state.prefix = true;
                let mut enter = key("KeyE");
                enter.text = "e".into();
                app.key(enter, &mut Vec::new(), pointer.size, &pointer.rects)
                    .unwrap();
                app.key(
                    key("ArrowRight"),
                    &mut Vec::new(),
                    pointer.size,
                    &pointer.rects,
                )
                .unwrap();
            }
            let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                first,
                Operation::LayoutUpdate {
                    expected_revision: 7,
                    ..
                }
            ));
            if drag {
                pointer.send(&mut app, 32, initial.x + 8, 5, false);
                // The release itself supplies the final desired point, even
                // outside the tile area and while the first CAS is blocked.
                pointer.send(&mut app, 0, 0, 5, true);
                assert!(app.pointer_capture.is_none());
            } else {
                let mut shifted = key("ArrowRight");
                shifted.modifiers = 1;
                app.key(
                    shifted.clone(),
                    &mut Vec::new(),
                    pointer.size,
                    &pointer.rects,
                )
                .unwrap();
                shifted.action = KeyAction::Repeat;
                app.key(shifted, &mut Vec::new(), pointer.size, &pointer.rects)
                    .unwrap();
                app.key(key("Enter"), &mut Vec::new(), pointer.size, &pointer.rects)
                    .unwrap();
                for action in [KeyAction::Repeat, KeyAction::Release] {
                    let mut tail = key("ArrowRight");
                    tail.modifiers = 1;
                    tail.action = action;
                    app.key(tail, &mut Vec::new(), pointer.size, &pointer.rects)
                        .unwrap();
                }
            }
            assert!(!app.resize_capturing());
            assert!(
                requests.try_recv().is_err(),
                "raw motion submitted another CAS"
            );
            release.send(()).unwrap();
            wait_background(&mut app, "manage:window:resize-window");
            app.validate_resize(pointer.size);
            app.flush_resize().unwrap();
            let second = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                second,
                Operation::LayoutUpdate {
                    expected_revision: 8,
                    ..
                }
            ));
            wait_background(&mut app, "manage:window:resize-window");
            server.join().unwrap();
            assert_eq!(app.registry.workspaces[0].windows[0].revision, 9);
            let final_divider = resize_divider(&app, pointer.size);
            if drag {
                let mut final_rects = Vec::new();
                layout_rects(
                    app.registry.workspaces[0].windows[0].layout.as_ref(),
                    app.screen_geometry(pointer.size).1,
                    &mut final_rects,
                );
                let pane = final_rects
                    .iter()
                    .find(|(id, _)| id == "focused-pane")
                    .unwrap()
                    .1;
                let native = content(pane);
                assert_eq!(native.width, 1, "clamp must preserve a native input column");
                assert!(native.x > pane.x && native.x + native.width < pane.x + pane.width);
                assert!(
                    !final_divider.contains(native.x, native.y),
                    "resize hit covers the surviving native column"
                );
            } else {
                assert_eq!(final_divider.x, initial.x + 11);
            }
            assert_eq!(app.state.focused.as_deref(), Some("focused-pane"));
            assert!(
                capture.try_recv().is_err(),
                "resize input leaked to an application"
            );
        }
    }

    #[test]
    fn rejected_unknown_and_inconsistent_resize_results_never_rebase_pending_intent() {
        use std::{os::unix::net::UnixListener, thread};
        for outcome in ["conflict", "lost", "inconsistent"] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("resize.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let (mut app, capture) = fixture();
            let endpoint = Endpoint::unmanaged(&socket);
            app.socket = endpoint.clone();
            let pointer = resize_fixture(&mut app);
            let before = app.registry.workspaces[0].windows[0].layout.clone();
            let mut registry = app.registry.clone();
            let (accepted, dispatched) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let operation = read_resize_request(&mut stream);
                accepted.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                match outcome {
                    "conflict" => {
                        transport::test_wire::write_response(
                            &mut stream,
                            &Response {
                                version: PROTOCOL_VERSION,
                                failure: None,
                                ok: false,
                                result: None,
                                error: Some("window revision conflict".into()),
                            },
                        )
                        .unwrap();
                    }
                    "inconsistent" => {
                        registry.workspaces[0].revision += 1;
                        registry.revision += 1;
                        resize_reply(&mut stream, &registry);
                    }
                    "lost" => accept_resize_request(&mut registry, &operation),
                    _ => unreachable!(),
                }
            });
            app.begin_keyboard_resize(pointer.size).unwrap();
            app.key(
                key("ArrowRight"),
                &mut Vec::new(),
                pointer.size,
                &pointer.rects,
            )
            .unwrap();
            dispatched.recv_timeout(Duration::from_secs(5)).unwrap();
            app.key(
                key("ArrowRight"),
                &mut Vec::new(),
                pointer.size,
                &pointer.rects,
            )
            .unwrap();
            app.key(key("Enter"), &mut Vec::new(), pointer.size, &pointer.rects)
                .unwrap();
            release.send(()).unwrap();
            wait_background(&mut app, "manage:window:resize-window");
            server.join().unwrap();
            app.flush_resize().unwrap();
            assert!(!app.jobs.contains("manage:window:resize-window"));
            assert!(!app.resize_capturing());
            assert_eq!(app.registry.workspaces[0].windows[0].layout, before);
            assert!(capture.try_recv().is_err());
        }
    }

    #[test]
    fn resize_retirement_consumes_pointer_tail_and_preserves_accepted_app_key_release() {
        for reason in [
            "focus",
            "workspace",
            "geometry",
            "modal",
            "modal-prefix",
            "host",
            "read-only",
        ] {
            let (mut app, capture) = fixture();
            let pointer = resize_fixture(&mut app);
            app.key(key("KeyA"), &mut Vec::new(), pointer.size, &pointer.rects)
                .unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. }
            } if pane_id == "focused-pane" && event.action == KeyAction::Press));
            let divider = resize_divider(&app, pointer.size);
            pointer.send(&mut app, 0, divider.x, 3, false);
            assert!(matches!(app.pointer_capture, Some(PointerCapture::Resize)));
            let mut new_size = pointer.size;
            match reason {
                "focus" => app.focus("beta".into()),
                "workspace" => app.switch("other".into()),
                "geometry" => new_size.cols += 1,
                "modal" => app.state.open(FormKind::ProjectDetails, String::new()),
                "modal-prefix" => {
                    let mut prefix = key("KeyB");
                    prefix.modifiers = 2;
                    app.key(prefix, &mut Vec::new(), pointer.size, &pointer.rects)
                        .unwrap();
                    let mut details = key("KeyI");
                    details.text = "i".into();
                    app.key(details, &mut Vec::new(), pointer.size, &pointer.rects)
                        .unwrap();
                    assert!(app.state.form.is_some());
                }
                "host" => app.host_focused = false,
                "read-only" => app.options.read_only = true,
                _ => unreachable!(),
            }
            app.validate_resize(new_size);
            assert!(!app.resize_capturing(), "{reason}");
            assert!(matches!(app.pointer_capture, Some(PointerCapture::Ui)));
            pointer.send(&mut app, 32, divider.x + 5, 3, false);
            pointer.send(&mut app, 0, pointer.size.cols - 1, 5, true);
            let mut release = key("KeyA");
            release.action = KeyAction::Release;
            app.key(release, &mut Vec::new(), new_size, &pointer.rects)
                .unwrap();
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { event, .. }
            } if pane_id == "focused-pane" && event.action == KeyAction::Release));
            assert!(capture.try_recv().is_err(), "{reason}: pointer tail leaked");
            assert!(!app.jobs.contains("manage:window:resize-window"));
        }
    }

    #[test]
    fn resize_read_only_modal_and_no_matching_axis_do_not_admit_layout_mutations() {
        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        let divider = resize_divider(&app, pointer.size);
        app.options.read_only = true;
        app.state.prefix = true;
        let mut enter = key("KeyE");
        enter.text = "e".into();
        app.dispatch_key(enter, &mut Vec::new(), pointer.size, &pointer.rects)
            .unwrap();
        assert!(!app.resize_capturing());
        assert!(pointer.try_send(&mut app, 0, divider.x, 3, false).is_err());
        pointer.send(&mut app, 0, divider.x, 3, true);
        app.options.read_only = false;
        app.state.open(FormKind::ProjectDetails, String::new());
        pointer.send(&mut app, 0, divider.x, 3, false);
        pointer.send(&mut app, 0, divider.x + 8, 3, true);
        assert!(!app.resize_capturing());
        app.state.cancel();
        app.state.reconcile(&app.registry);
        pointer.send(&mut app, 0, divider.x, 3, false);
        pointer.send(&mut app, 0, divider.x, 3, true);
        assert!(!app.jobs.contains("manage:window:resize-window"));
        app.begin_keyboard_resize(pointer.size).unwrap();
        app.key(
            key("ArrowUp"),
            &mut Vec::new(),
            pointer.size,
            &pointer.rects,
        )
        .unwrap();
        app.key(key("Escape"), &mut Vec::new(), pointer.size, &pointer.rects)
            .unwrap();
        assert!(!app.resize_capturing());
        assert!(!app.jobs.contains("manage:window:resize-window"));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn resize_registry_observation_preserves_other_workspaces_and_retires_foreign_target_updates() {
        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        app.begin_keyboard_resize(pointer.size).unwrap();
        let mut unrelated = app.registry.clone();
        unrelated.revision += 1;
        unrelated.workspaces[1].revision += 1;
        unrelated.workspaces[1].name = "renamed elsewhere".into();
        app.observe_resize(&unrelated);
        app.registry = unrelated;
        app.validate_resize(pointer.size);
        assert!(app.resize_capturing());
        let mut foreign = app.registry.clone();
        foreign.revision += 1;
        foreign.workspaces[0].windows[0].revision += 1;
        if let Some(LayoutNode::Split { ratio, .. }) = &mut foreign.workspaces[0].windows[0].layout
        {
            *ratio = 700;
        }
        app.observe_resize(&foreign);
        app.registry = foreign;
        app.validate_resize(pointer.size);
        assert!(!app.resize_capturing());
        assert!(!app.jobs.contains("manage:window:resize-window"));
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn native_pointer_release_keeps_original_app_view_while_keyboard_resize_captures_keys() {
        let (mut app, capture) = fixture();
        let pointer = resize_fixture(&mut app);
        let alpha = content(pointer.rects[0].1);
        let beta = content(pointer.rects[1].1);
        pointer.send(&mut app, 0, alpha.x + 2, 3, false);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Mouse { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == MouseAction::Press));
        app.begin_keyboard_resize(pointer.size).unwrap();
        app.key(key("KeyZ"), &mut Vec::new(), pointer.size, &pointer.rects)
            .unwrap();
        pointer.send(&mut app, 0, beta.x + 2, 3, true);
        assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Mouse { view_id, event }
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.action == MouseAction::Release));
        assert!(app.resize_capturing());
        app.key(key("Escape"), &mut Vec::new(), pointer.size, &pointer.rects)
            .unwrap();
        assert!(capture.try_recv().is_err());
        assert!(app.held.is_empty());
    }
    #[test]
    fn modal_targets_do_not_follow_registry_driven_focus_or_workspace_replacement() {
        let (mut app, _) = fixture();
        unavailable_fixture(&mut app);
        let original = app.state.focused.clone().unwrap();
        app.open_form(FormKind::RenamePane, "renamed".into())
            .unwrap();
        let mut replacement = app
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == original)
            .unwrap()
            .clone();
        replacement.id = "replacement-pane".into();
        replacement.status = PaneStatus::Running;
        app.registry
            .panes
            .iter_mut()
            .find(|pane| pane.id == original)
            .unwrap()
            .status = PaneStatus::Exited;
        app.registry.panes.push(replacement);
        app.registry.workspaces[0].windows[0].layout = Some(LayoutNode::Leaf {
            pane_id: "replacement-pane".into(),
        });
        app.state.reconcile(&app.registry);
        assert_eq!(app.state.focused.as_deref(), Some("replacement-pane"));
        let target = app.state.form.as_ref().unwrap().target.as_ref();
        assert_eq!(app.form_pane(target).unwrap(), original);

        let (mut app, _) = fixture();
        unavailable_fixture(&mut app);
        app.open_form(FormKind::RenameWorkspace, "renamed".into())
            .unwrap();
        app.registry.workspaces[0].id = "replacement-workspace".into();
        app.state.reconcile(&app.registry);
        assert_eq!(
            app.state.workspace.as_deref(),
            Some("replacement-workspace")
        );
        let target = app.state.form.as_ref().unwrap().target.as_ref();
        assert!(app.form_workspace(target).is_err());
    }
    #[test]
    fn legacy_control_space_owns_sidebar_navigation_instead_of_native_input() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        physical_keys(&mut app, b"\0").unwrap();
        assert!(
            app.state.sidebar.active,
            "native event: {:?}",
            received.try_recv()
        );
        physical_keys(&mut app, b"j").unwrap();
        assert!(received.try_recv().is_err());
    }
    #[test]
    fn activation_selects_the_authoritative_owner_window_without_borrowing_its_layout() {
        let (mut app, capture) = fixture();
        unavailable_fixture(&mut app);
        let mut hidden = app.registry.panes[0].clone();
        hidden.id = "hidden-pane".into();
        hidden.status = PaneStatus::Running;
        hidden.workspace_id = Some("other".into());
        hidden.window_id = Some("other-window".into());
        app.registry.panes.push(hidden);
        app.registry.workspaces.push(Workspace {
            id: "other".into(),
            name: "Other".into(),
            revision: 42,
            windows: vec![Window {
                id: "other-window".into(),
                name: "Hidden terminal".into(),
                revision: 9,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "hidden-pane".into(),
                }),
            }],
        });
        let before = serde_json::to_value(&app.registry).unwrap();
        assert!(
            app.rects(size())
                .iter()
                .all(|(pane, _)| pane != "hidden-pane")
        );
        // A stale sidebar workspace hint cannot transfer pane ownership.
        app.activate_sidebar(SidebarTarget::Pane {
            workspace: "history".into(),
            pane: "hidden-pane".into(),
        })
        .unwrap();
        assert_eq!(app.state.workspace.as_deref(), Some("other"));
        assert_eq!(app.state.window.as_deref(), Some("other-window"));
        assert_eq!(app.state.focused.as_deref(), Some("hidden-pane"));
        assert_eq!(
            app.rects(size())
                .iter()
                .map(|(pane, _)| pane.as_str())
                .collect::<Vec<_>>(),
            vec!["hidden-pane"]
        );
        assert_eq!(serde_json::to_value(&app.registry).unwrap(), before);
        assert!(app.management.is_empty());
        assert!(capture.try_recv().is_err());
    }

    #[test]
    fn restoring_a_removed_projection_targets_only_the_panes_owning_window_revision() {
        use std::{os::unix::net::UnixListener, thread};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("restore.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let endpoint = Endpoint::unmanaged(&socket);
        let (mut app, _) = fixture();
        app.socket = endpoint.clone();
        unavailable_fixture(&mut app);
        let mut removed = app.registry.panes[0].clone();
        removed.id = "removed-pane".into();
        removed.status = PaneStatus::Running;
        removed.window_id = Some("owner-window".into());
        app.registry.panes.push(removed.clone());
        removed.id = "owner-neighbor".into();
        app.registry.panes.push(removed);
        app.registry.workspaces[0].windows.push(Window {
            id: "owner-window".into(),
            name: "Owner".into(),
            revision: 23,
            layout: Some(LayoutNode::Leaf {
                pane_id: "owner-neighbor".into(),
            }),
        });
        app.registry.workspaces[0].revision = 100;
        let before = serde_json::to_value(&app.registry).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let operation = read_resize_request(&mut stream);
            transport::test_wire::write_response(
                &mut stream,
                &Response {
                    version: PROTOCOL_VERSION,
                    failure: None,
                    ok: false,
                    result: None,
                    error: Some("fixture rejected layout".into()),
                },
            )
            .unwrap();
            operation
        });
        app.show_existing("removed-pane".into()).unwrap();
        assert_eq!(app.state.window.as_deref(), Some("owner-window"));
        let Operation::LayoutUpdate {
            workspace_id,
            window_id,
            expected_revision,
            layout,
        } = server.join().unwrap()
        else {
            panic!("restoration must be an owned layout update");
        };
        assert_eq!(
            (workspace_id.as_str(), window_id.as_str(), expected_revision),
            ("history", "owner-window", 23)
        );
        let mut projected = Vec::new();
        leaves(layout.as_ref(), &mut projected);
        assert_eq!(projected, vec!["owner-neighbor", "removed-pane"]);
        assert_eq!(serde_json::to_value(&app.registry).unwrap(), before);
        assert_eq!(app.management.len(), 1);
    }

    fn submitted_form_operation(kind: FormKind, prepare: impl FnOnce(&mut App)) -> Operation {
        use std::{os::unix::net::UnixListener, thread};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("form.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let endpoint = Endpoint::unmanaged(&socket);
        let (mut app, _) = fixture();
        app.socket = endpoint.clone();
        unavailable_fixture(&mut app);
        app.open_form(kind, "renamed".into()).unwrap();
        prepare(&mut app);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_resize_request(&mut stream);
            transport::test_wire::write_response(
                &mut stream,
                &Response {
                    version: PROTOCOL_VERSION,
                    failure: None,
                    ok: false,
                    result: None,
                    error: Some("fixture revision rejection".into()),
                },
            )
            .unwrap();
            request
        });
        app.apply_form(size()).unwrap();
        let jobs = std::mem::replace(&mut app.jobs, jobs::Scope::new(1).unwrap());
        assert!(
            jobs.finish()
                .unwrap()
                .into_iter()
                .all(|completion| completion.result.is_err())
        );
        server.join().unwrap()
    }

    #[test]
    fn form_creation_is_atomic_preserves_native_cwd_and_captures_workspace_revision() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let cwd = PathBuf::from(OsString::from_vec(b"/native/\xffcwd".to_vec()));
        for window in [false, true] {
            let operation = submitted_form_operation(
                if window {
                    FormKind::CreateWindow
                } else {
                    FormKind::CreateWorkspace
                },
                |app| {
                    let form = app.state.form.as_mut().unwrap();
                    form.fields.primary.value.clear();
                    form.fields.inherit_directory(cwd.clone());
                    app.registry.workspaces[0].revision = 99;
                },
            );
            match operation {
                Operation::WorkspaceCreate {
                    name: None,
                    initial: Some(spec),
                } if !window => {
                    assert_eq!(spec.cwd, cwd);
                }
                Operation::WindowCreate {
                    workspace_id,
                    expected_revision: 7,
                    name: None,
                    spec,
                } if window => {
                    assert_eq!(workspace_id, "history");
                    assert_eq!(spec.cwd, cwd);
                }
                other => panic!("unexpected creation request: {other:?}"),
            }
        }
    }

    #[test]
    fn close_and_rename_forms_submit_owned_document_targets_with_captured_revisions() {
        for kind in [
            FormKind::CloseWorkspace,
            FormKind::CloseWindow,
            FormKind::RenameWindow,
        ] {
            let original = kind.clone();
            let operation = submitted_form_operation(kind, |app| {
                app.state.form.as_mut().unwrap().confirmed = true;
                app.registry.workspaces[0].revision = 90;
                app.registry.workspaces[0].windows[0].revision = 91;
            });
            match (original, operation) {
                (
                    FormKind::CloseWorkspace,
                    Operation::WorkspaceClose {
                        workspace_id,
                        expected_revision: 7,
                    },
                ) => {
                    assert_eq!(workspace_id, "history");
                }
                (
                    FormKind::CloseWindow,
                    Operation::WindowClose {
                        workspace_id,
                        window_id,
                        expected_revision: 7,
                    },
                )
                | (
                    FormKind::RenameWindow,
                    Operation::WindowRename {
                        workspace_id,
                        window_id,
                        expected_revision: 7,
                        ..
                    },
                ) => {
                    assert_eq!(workspace_id, "history");
                    assert_eq!(window_id, "history-window");
                }
                other => panic!("unexpected document mutation: {other:?}"),
            }
        }
    }

    #[test]
    fn shutdown_waits_for_policy_revocation_before_admitting_backend_detach() {
        use std::{os::unix::net::UnixListener, thread, time::Instant};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("retire.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let accept = || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "retirement worker did not connect");
                let mut ready = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let result =
                    unsafe { libc::poll(&mut ready, 1, remaining.as_millis().max(1) as i32) };
                if result < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                assert_eq!(result, 1, "retirement worker did not connect");
                return listener.accept().unwrap().0;
            }
        };
        let endpoint = Endpoint::unmanaged(&socket);
        let input = InputPump::new(&endpoint).unwrap();
        let (mut app, _) = fixture();
        attach_fixture(&mut app);
        let mut attached = app.views.remove("focused-pane").unwrap();
        let worker = thread::spawn(move || {
            let receipt = attached
                .retire(&input, "focused-pane", false, false, true)
                .unwrap()
                .unwrap();
            receipt.recv().unwrap().unwrap();
            input.finish().unwrap();
        });
        let mut policy = accept();
        assert!(matches!(read_resize_request(&mut policy), Operation::Pane {
            request: PaneRequest::Update { spec, .. }, ..
        } if !spec.visible && !spec.interactive && !spec.clipboard));
        let mut pending_detach = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut pending_detach, 1, 100) },
            0,
            "Detach overtook unacknowledged policy revocation"
        );
        transport::test_wire::write_response(
            &mut policy,
            &Response {
                version: PROTOCOL_VERSION,
                failure: None,
                ok: true,
                result: Some(serde_json::Value::Bool(true).into()),
                error: None,
            },
        )
        .unwrap();
        let mut detach = accept();
        assert!(matches!(read_resize_request(&mut detach), Operation::Pane {
            pane_id, request: PaneRequest::Detach { view_id },
        } if pane_id == "focused-pane" && view_id == "focused-view"));
        transport::test_wire::write_response(
            &mut detach,
            &Response {
                version: PROTOCOL_VERSION,
                failure: None,
                ok: true,
                result: Some(serde_json::Value::Bool(true).into()),
                error: None,
            },
        )
        .unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn passive_ci_hover_preserves_ordinary_pane_r_input_and_keyboard_focus() {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        app.registry.panes[0].workspace_id = Some("workspace".into());
        app.registry.panes[0].window_id = Some("window".into());
        app.registry.panes[0].project = Some(crate::ci::tests::project());
        app.registry.workspaces.push(Workspace {
            id: "workspace".into(),
            name: "Workspace".into(),
            revision: 1,
            windows: vec![Window {
                id: "window".into(),
                name: "Window".into(),
                revision: 1,
                layout: Some(LayoutNode::Leaf {
                    pane_id: "focused-pane".into(),
                }),
            }],
        });
        app.state.workspace = Some("workspace".into());
        app.state.window = Some("window".into());
        publish_test_tree(&mut app);
        let target = TreeTarget {
            owner: app.tree_owner(),
            node: TreeNode::Native(SidebarTarget::Pane {
                workspace: "workspace".into(),
                pane: "focused-pane".into(),
            }),
        };
        let shared = Arc::clone(&app.tree);
        {
            let mut tree = shared.lock().unwrap();
            tree.hover(Some(&target));
            app.popover
                .paint_tree(&tree, Some(&target), 3, 24, size())
                .unwrap();
            assert!(app.popover.refresh_available(&tree));
        }
        app.synchronize_tree_focus();
        assert!(!app.state.sidebar.active);
        let focused = app.state.focused.clone();
        let views = app.views.len();
        let mut event = key("KeyR");
        event.text = "R".into();
        app.key(event, &mut Vec::new(), size(), &[]).unwrap();
        assert!(matches!(received.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { event, .. }
        } if pane_id == "focused-pane" && event.text == "R"));
        assert!(
            app.session_action.is_none(),
            "passive metadata cannot dispatch refresh"
        );
        assert_eq!(app.state.focused, focused);
        assert_eq!(app.views.len(), views);
    }
    include!("attach_shutdown_tests.rs");
    include!("search_tests.rs");
}
