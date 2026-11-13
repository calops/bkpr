use crate::tiling;
use crate::{metadata::ProjectSpan, project_colors::ProjectPalette};
use bkpr_protocol::*;
use bkpr_tui_framework::{BackgroundSpan, ForegroundSpan, Rect, Role, RoleSpan, Theme, UiSurface};
use std::collections::HashMap;
use std::fmt::Write as _;
#[path = "sidebar.rs"]
mod sidebar;
pub use sidebar::{Row as SidebarRow, RowKind as SidebarRowKind, Sidebar, SidebarTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormKind {
    CreateWorkspace,
    CreateWindow,
    RenameWorkspace,
    RenameWindow,
    RenamePane,
    TerminatePane,
    ReopenPane {
        workspace_id: String,
        window_id: String,
        expected_revision: u64,
        pane_id: String,
        selection_epoch: u64,
        cwd: std::path::PathBuf,
    },
    CloseWorkspace,
    CloseWindow,
    ProjectDetails,
}
#[derive(Clone, Debug)]
pub struct Form {
    pub kind: FormKind,
    pub fields: crate::form_fields::FormFields,
    pub error: Option<String>,
    pub pending: Option<String>,
    pub ambiguous: bool,
    pub confirmed: bool,
    pub selection: usize,
    pub restore: Option<String>,
    pub target: Option<(SidebarTarget, u64)>,
}
impl Form {
    pub fn new(kind: FormKind, value: String, restore: Option<String>) -> Self {
        Self {
            kind,
            fields: crate::form_fields::FormFields::new(value, std::path::PathBuf::new()),
            error: None,
            pending: None,
            ambiguous: false,
            confirmed: false,
            selection: 0,
            restore,
            target: None,
        }
    }
    pub fn context(&self) -> crate::keyguide::Context {
        use crate::keyguide::Context;
        match self.kind {
            FormKind::CreateWorkspace | FormKind::CreateWindow => Context::Create,
            FormKind::RenameWorkspace | FormKind::RenameWindow | FormKind::RenamePane => {
                Context::Edit
            }
            FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow => {
                Context::Confirmation
            }
            FormKind::ProjectDetails => Context::Details,
            FormKind::ReopenPane { .. } => Context::Chooser,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Queued,
    Connecting,
    Reconnecting,
    Activating,
    Online,
    Offline,
    Rejected,
}
impl SessionStatus {
    pub(crate) fn busy(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Reconnecting | Self::Activating
        )
    }
    pub(crate) fn badge(self) -> &'static str {
        match self {
            Self::Queued => "\u{f0150}",
            Self::Connecting | Self::Reconnecting | Self::Activating => "\u{f0a9e}",
            Self::Online => "",
            Self::Offline => "\u{f0c5f}",
            Self::Rejected => "\u{f05d6}",
        }
    }
    pub(crate) fn role(self) -> Role {
        match self {
            Self::Connecting | Self::Reconnecting | Self::Activating => Role::Warning,
            Self::Online => Role::Title,
            Self::Offline | Self::Rejected => Role::Error,
            Self::Queued => Role::Muted,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionGroup {
    pub name: String,
    pub host: Option<String>,
    pub status: SessionStatus,
    pub detail: String,
}

impl SessionGroup {
    pub(crate) fn tail_role(&self) -> Role {
        match self.status {
            SessionStatus::Online | SessionStatus::Queued => Role::Muted,
            status => status.role(),
        }
    }

    fn tail_icon(&self, spinner: char) -> Option<char> {
        if self.status.busy()
            && spinner != ' '
            && unicode_width::UnicodeWidthChar::width(spinner) == Some(1)
        {
            return Some(spinner);
        }
        match (self.host.is_some(), self.status) {
            (true, SessionStatus::Online) => Some('\u{f048d}'),
            (true, SessionStatus::Offline | SessionStatus::Rejected) => Some('\u{f048e}'),
            _ => self.status.badge().chars().next(),
        }
    }
}

#[derive(Default)]
struct SidebarCaptionPart<'a> {
    text: &'a str,
    width: u16,
    ellipsis: bool,
}
impl<'a> SidebarCaptionPart<'a> {
    fn fit(text: &'a str, width: u16) -> Self {
        use unicode_segmentation::UnicodeSegmentation as _;
        if width == 0 {
            return Self::default();
        }
        let mut columns = 0;
        let mut ellipsis_end = 0;
        let mut ellipsis_columns = 0;
        for (offset, glyph) in text.grapheme_indices(true) {
            let advance = unicode_width::UnicodeWidthStr::width(glyph);
            if columns + advance > usize::from(width) {
                return Self {
                    text: &text[..ellipsis_end],
                    width: ellipsis_columns + 1,
                    ellipsis: true,
                };
            }
            columns += advance;
            if columns < usize::from(width) {
                ellipsis_end = offset + glyph.len();
                ellipsis_columns = columns as u16;
            }
        }
        Self {
            text,
            width: columns as u16,
            ellipsis: false,
        }
    }

    fn push_to(&self, caption: &mut String) {
        caption.push_str(self.text);
        if self.ellipsis {
            caption.push('…');
        }
    }
}

struct SessionCaptionLayout<'a> {
    name: SidebarCaptionPart<'a>,
    host: SidebarCaptionPart<'a>,
    tail_width: u16,
}
fn session_caption_layout(group: &SessionGroup, width: u16) -> SessionCaptionLayout<'_> {
    let body = width.saturating_sub(2);
    let mut layout = SessionCaptionLayout {
        name: SidebarCaptionPart::default(),
        host: SidebarCaptionPart::default(),
        tail_width: 0,
    };
    if body >= 3 && group.tail_icon(' ').is_some() {
        layout.tail_width = 1;
        if let Some(host) = group.host.as_deref().filter(|host| !host.is_empty())
            && body >= 5
        {
            // Keep at least half the usable text space for the name, but let a
            // short name release its unused share to the right-aligned host.
            let name_share = SidebarCaptionPart::fit(&group.name, (body - 3).div_ceil(2));
            layout.host = SidebarCaptionPart::fit(host, body - 3 - name_share.width);
            layout.tail_width += 1 + layout.host.width;
        }
    }
    let name_width = body.saturating_sub(layout.tail_width + u16::from(layout.tail_width > 0));
    layout.name = SidebarCaptionPart::fit(&group.name, name_width);
    layout
}

/// Width of the right-aligned host/status slot, excluding the preceding gap.
/// This shares caption layout without formatting or allocating a second string.
fn session_tail_width(group: &SessionGroup, width: u16) -> u16 {
    session_caption_layout(group, width).tail_width
}

/// The beekeeper and its spacer stay fixed. The name gets a fair share before
/// the optional host, whose status icon is replaced rather than supplemented.
fn session_caption(group: &SessionGroup, spinner: char, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let layout = session_caption_layout(group, width);
    let mut caption = String::with_capacity(usize::from(width) * 4);
    caption.push('\u{f14e2}');
    if width == 1 {
        return caption;
    }
    caption.push(' ');
    layout.name.push_to(&mut caption);
    caption.extend(std::iter::repeat_n(
        ' ',
        usize::from(width - 2 - layout.name.width - layout.tail_width),
    ));
    if layout.tail_width > 0 {
        caption.push(group.tail_icon(spinner).expect("layout reserves an icon"));
        if layout.tail_width > 1 {
            caption.push(' ');
            layout.host.push_to(&mut caption);
        }
    }
    caption
}

pub struct Controller {
    pub workspace: Option<String>,
    pub window: Option<String>,
    pub focused: Option<String>,
    remembered: HashMap<String, String>,
    remembered_windows: HashMap<String, String>,
    pub prefix: bool,
    pub form: Option<Form>,
    pub suppressed: std::collections::HashSet<String>,
    pub sidebar: Sidebar,
    /// Provenance of this controller's owner; never used to route another group.
    pub remote: Option<(String, String)>,
    pub(crate) session_spinner: char,
}
impl Controller {
    pub fn new(registry: &RegistrySnapshot) -> Self {
        let mut state = Self {
            workspace: initial_workspace(registry).map(|w| w.id.clone()),
            window: None,
            focused: None,
            remembered: HashMap::new(),
            remembered_windows: HashMap::new(),
            prefix: false,
            form: None,
            suppressed: Default::default(),
            sidebar: Sidebar::default(),
            remote: None,
            session_spinner: ' ',
        };
        state.reconcile(registry);
        state
    }
    pub(crate) fn sidebar_rows(&self, size: Size, guide_rows: u16) -> usize {
        let sidebar = screen_geometry(size, self.sidebar.preferred_columns).0;
        sidebar_content_rows(sidebar, guide_rows)
    }

    pub fn current<'a>(&self, registry: &'a RegistrySnapshot) -> Option<&'a Workspace> {
        registry
            .workspaces
            .iter()
            .find(|w| Some(&w.id) == self.workspace.as_ref())
    }
    pub fn current_window<'a>(&self, registry: &'a RegistrySnapshot) -> Option<&'a Window> {
        self.current(registry)?
            .windows
            .iter()
            .find(|window| Some(&window.id) == self.window.as_ref())
    }
    pub fn reconcile(&mut self, registry: &RegistrySnapshot) {
        self.remembered_windows.retain(|workspace, window| {
            registry.workspaces.iter().any(|w| {
                &w.id == workspace && w.windows.iter().any(|candidate| &candidate.id == window)
            })
        });
        self.remembered.retain(|window, pane| {
            registry
                .workspaces
                .iter()
                .flat_map(|w| &w.windows)
                .any(|w| &w.id == window && contains_leaf(w.layout.as_ref(), pane))
        });
        if self.current(registry).is_none() {
            self.workspace = initial_workspace(registry).map(|w| w.id.clone());
            self.window = None;
            self.focused = None;
        }
        if self.current_window(registry).is_none() {
            self.window = self.current(registry).and_then(|workspace| {
                self.remembered_windows
                    .get(&workspace.id)
                    .and_then(|id| workspace.windows.iter().find(|window| &window.id == id))
                    .or_else(|| initial_window(workspace, registry))
                    .map(|window| window.id.clone())
            });
            self.focused = None;
        }
        let mut ids = Vec::new();
        if let Some(window) = self.current_window(registry) {
            leaves(window.layout.as_ref(), &mut ids);
        }
        if !self.focused.as_ref().is_some_and(|id| ids.contains(id)) {
            self.focused =
                self.window
                    .as_ref()
                    .and_then(|window| self.remembered.get(window))
                    .filter(|id| ids.contains(id))
                    .cloned()
                    .or_else(|| {
                        ids.iter()
                            .find(|id| {
                                registry.panes.iter().any(|pane| {
                                    pane.id == **id && pane.status == PaneStatus::Running
                                })
                            })
                            .cloned()
                    })
                    .or_else(|| ids.first().cloned());
        }
        if let (Some(workspace), Some(window)) = (&self.workspace, &self.window) {
            self.remembered_windows
                .insert(workspace.clone(), window.clone());
        }
        if let (Some(window), Some(pane)) = (&self.window, &self.focused) {
            self.remembered.insert(window.clone(), pane.clone());
        }
        self.sidebar.reconcile(
            registry,
            self.workspace.as_deref(),
            self.window.as_deref(),
            self.focused.as_deref(),
        );
    }
    pub fn focus(&mut self, pane: String) {
        self.focused = Some(pane.clone());
        if let Some(window) = &self.window {
            self.remembered.insert(window.clone(), pane);
        }
    }
    pub fn switch(&mut self, id: String, registry: &RegistrySnapshot) {
        if self.workspace.as_ref() != Some(&id) {
            self.workspace = Some(id);
            self.window = None;
            self.focused = None;
        }
        self.reconcile(registry);
    }
    pub fn switch_window(&mut self, id: String, registry: &RegistrySnapshot) {
        if !self
            .current(registry)
            .is_some_and(|workspace| workspace.windows.iter().any(|window| window.id == id))
        {
            return;
        }
        if self.window.as_ref() != Some(&id) {
            self.window = Some(id);
            self.focused = None;
        }
        self.reconcile(registry);
    }
    pub fn cycle_workspace(&mut self, registry: &RegistrySnapshot, delta: isize) {
        if registry.workspaces.is_empty() {
            return;
        }
        let index = registry
            .workspaces
            .iter()
            .position(|w| Some(&w.id) == self.workspace.as_ref())
            .unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(registry.workspaces.len() as isize) as usize;
        self.switch(registry.workspaces[next].id.clone(), registry);
    }
    pub fn cycle_pane(&mut self, registry: &RegistrySnapshot) {
        let mut ids = Vec::new();
        if let Some(window) = self.current_window(registry) {
            leaves(window.layout.as_ref(), &mut ids);
        }
        if ids.is_empty() {
            return;
        }
        let next = self
            .focused
            .as_ref()
            .and_then(|id| ids.iter().position(|p| p == id))
            .map(|i| (i + 1) % ids.len())
            .unwrap_or(0);
        self.focus(ids[next].clone());
    }
    pub fn open(&mut self, kind: FormKind, value: String) {
        self.form = Some(Form::new(kind, value, self.focused.clone()));
    }
    pub fn cancel(&mut self) {
        if let Some(form) = self.form.take() {
            self.focused = form.restore;
        }
    }
}
fn initial_workspace(registry: &RegistrySnapshot) -> Option<&Workspace> {
    registry
        .workspaces
        .iter()
        .find(|workspace| {
            workspace
                .windows
                .iter()
                .any(|window| has_running_projection(window.layout.as_ref(), registry))
        })
        .or_else(|| registry.workspaces.first())
}

fn initial_window<'a>(workspace: &'a Workspace, registry: &RegistrySnapshot) -> Option<&'a Window> {
    workspace
        .windows
        .iter()
        .find(|window| has_running_projection(window.layout.as_ref(), registry))
        .or_else(|| workspace.windows.first())
}

fn contains_leaf(layout: Option<&LayoutNode>, pane: &str) -> bool {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) => pane_id == pane,
        Some(LayoutNode::Split { first, second, .. }) => {
            contains_leaf(Some(first), pane) || contains_leaf(Some(second), pane)
        }
        None => false,
    }
}

fn has_running_projection(layout: Option<&LayoutNode>, registry: &RegistrySnapshot) -> bool {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) => registry
            .panes
            .iter()
            .any(|pane| pane.id == *pane_id && pane.status == PaneStatus::Running),
        Some(LayoutNode::Split { first, second, .. }) => {
            has_running_projection(Some(first), registry)
                || has_running_projection(Some(second), registry)
        }
        None => false,
    }
}

pub fn leaves(layout: Option<&LayoutNode>, result: &mut Vec<String>) {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) => result.push(pane_id.clone()),
        Some(LayoutNode::Split { first, second, .. }) => {
            leaves(Some(first), result);
            leaves(Some(second), result);
        }
        None => {}
    }
}
pub fn split_leaf(
    layout: Option<LayoutNode>,
    target: &str,
    pane: &str,
    axis: Axis,
) -> Option<LayoutNode> {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) if pane_id == target => Some(LayoutNode::Split {
            axis,
            ratio: 500,
            first: Box::new(LayoutNode::Leaf { pane_id }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: pane.into(),
            }),
        }),
        Some(LayoutNode::Split {
            axis: old,
            ratio,
            first,
            second,
        }) => Some(LayoutNode::Split {
            axis: old,
            ratio,
            first: Box::new(split_leaf(Some(*first), target, pane, axis)?),
            second: Box::new(split_leaf(Some(*second), target, pane, axis)?),
        }),
        None => Some(LayoutNode::Leaf {
            pane_id: pane.into(),
        }),
        other => other,
    }
}
pub fn remove_leaf(layout: Option<LayoutNode>, target: &str) -> Option<LayoutNode> {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) if pane_id == target => None,
        Some(LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        }) => match (
            remove_leaf(Some(*first), target),
            remove_leaf(Some(*second), target),
        ) {
            (Some(first), Some(second)) => Some(LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (first, second) => first.or(second),
        },
        other => other,
    }
}
pub fn exchange_leaves(layout: &mut LayoutNode, first_id: &str, second_id: &str) {
    match layout {
        LayoutNode::Leaf { pane_id } if pane_id == first_id => {
            *pane_id = second_id.to_owned();
        }
        LayoutNode::Leaf { pane_id } if pane_id == second_id => {
            *pane_id = first_id.to_owned();
        }
        LayoutNode::Split { first, second, .. } => {
            exchange_leaves(first, first_id, second_id);
            exchange_leaves(second, first_id, second_id);
        }
        LayoutNode::Leaf { .. } => {}
    }
}

/// Horizontal partitions columns; vertical partitions rows. Every header and
/// content rectangle is derived from this single source, including hit testing.
pub fn layout_rects(layout: Option<&LayoutNode>, rect: Rect, result: &mut Vec<(String, Rect)>) {
    let rect = tiling::visible_area(rect);
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    match layout {
        Some(LayoutNode::Leaf { pane_id }) => result.push((pane_id.clone(), rect)),
        Some(LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        }) => {
            if let Some((a, _, b)) = tiling::partition(rect, *axis, *ratio) {
                layout_rects(Some(first), a, result);
                layout_rects(Some(second), b, result);
            } else {
                layout_rects(Some(first), rect, result);
            }
        }
        None => {}
    }
}
pub fn screen_geometry(size: Size, preferred_columns: Option<u16>) -> (Rect, Rect) {
    // Main's launcher defaults to 38 cells, shrinking automatically on small
    // terminals. A dragged width instead leaves at least three native cells.
    let sidebar = if size.cols >= 24 {
        preferred_columns.map_or_else(
            || 38.min(size.cols / 2),
            |columns| columns.clamp(3, size.cols - 3),
        )
    } else {
        0
    };
    (
        Rect {
            x: 0,
            y: 0,
            width: sidebar,
            height: size.rows,
        },
        Rect {
            x: sidebar,
            y: 0,
            width: size.cols - sidebar,
            height: size.rows,
        },
    )
}
/// The same reserved perimeter is used by plain and graphics-backed chrome.
/// Tiny panes retain a header-only fallback rather than losing their last cell.
pub fn content(rect: Rect) -> Rect {
    let border = crate::appearance::PANE_BORDER;
    if rect.width > border.saturating_mul(2) && rect.height > border.saturating_mul(2) {
        Rect {
            x: rect.x.saturating_add(border),
            y: rect.y.saturating_add(border),
            width: rect.width - border * 2,
            height: rect.height - border * 2,
        }
    } else {
        Rect {
            y: rect.y.saturating_add(u16::from(rect.height > 1)),
            height: rect.height.saturating_sub(u16::from(rect.height > 1)),
            ..rect
        }
    }
}
pub(crate) fn recognized_harness<'a>(
    pane: &'a PaneInfo,
    snapshot: Option<&PaneSnapshot>,
) -> Option<&'a HarnessInfo> {
    let harness = pane.harness.as_ref()?;
    if pane.status != PaneStatus::Running {
        return None;
    }
    // Registry and pane streams arrive independently. Do not combine a launch
    // from one observation with a contradictory terminal identity from another.
    if snapshot.is_some_and(|snapshot| {
        snapshot.pane_id != pane.id
            || matches!(snapshot.lifecycle.as_str(), "exited" | "terminated")
            || snapshot.program_identity.as_ref().is_none_or(|identity| {
                identity.pid != harness.pid || identity.birth != harness.birth
            })
    }) {
        return None;
    }
    Some(harness)
}

/// Terminal status belongs to the observed foreground identity, not its title,
/// a previous launch, or another application's OSC record.
pub(crate) fn harness_status<'a>(
    pane: &'a PaneInfo,
    snapshot: Option<&PaneSnapshot>,
) -> Option<&'a ProgramStatus> {
    let harness = recognized_harness(pane, snapshot)?;
    pane.harness_status.as_ref().filter(|record| {
        let matching_app = match record.app.as_deref() {
            Some(app) => HarnessKind::from_command(app) == Some(harness.kind),
            None => record.id.is_empty(),
        };
        matching_app
            && matches!(
                record.state.as_str(),
                "idle" | "working" | "done" | "blocked" | "error"
            )
    })
}

pub(crate) fn harness_icon(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Omp | HarnessKind::Pi => "\u{f03ff}",
        HarnessKind::Claude => "\u{ec82}",
        HarnessKind::Codex => "\u{ec81}",
        HarnessKind::Opencode => "\u{f0174}",
        HarnessKind::Agy => "\u{f0463}",
        HarnessKind::Cursor => "\u{f01bf}",
        HarnessKind::Copilot => "\u{ec1e}",
        HarnessKind::Aider => "\u{f03eb}",
        HarnessKind::Cline => "\u{f06a9}",
        HarnessKind::Goose => "\u{f01e5}",
    }
}

pub(crate) fn harness_role(kind: HarnessKind) -> Role {
    match kind {
        HarnessKind::Omp | HarnessKind::Pi => Role::Secondary,
        HarnessKind::Codex | HarnessKind::Aider => Role::Success,
        HarnessKind::Claude | HarnessKind::Goose => Role::Warning,
        HarnessKind::Agy => Role::Info,
        HarnessKind::Cline => Role::BrightInfo,
        HarnessKind::Copilot => Role::BrightAccent,
        HarnessKind::Opencode => Role::Accent,
        HarnessKind::Cursor => Role::Base,
    }
}

fn pane_activity(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>) -> &'static str {
    match pane.status {
        PaneStatus::Stopped => "■",
        PaneStatus::Exited => "×",
        PaneStatus::Running => {
            match harness_status(pane, snapshot).map(|status| status.state.as_str()) {
                Some("idle") => "\u{f03e6}",
                Some("working") => "\u{f0150}",
                Some("done") => "\u{f058}",
                Some("blocked") => "\u{f059}",
                Some("error") => "\u{f06a}",
                _ => "",
            }
        }
    }
}

fn sidebar_pane_label(
    pane: &PaneInfo,
    snapshot: Option<&PaneSnapshot>,
    width: usize,
) -> (String, std::ops::Range<u16>) {
    let status = bkpr_tui_framework::window::fit(pane_activity(pane, snapshot), width as u16);
    let suffix_width = unicode_width::UnicodeWidthStr::width(status.as_str());
    let icon = recognized_harness(pane, snapshot).map(|harness| harness_icon(harness.kind));
    // Main keeps the whole logo and its trailing spacer, or omits both. Never
    // let a narrow row display a clipped logo without its wide-glyph clearance.
    let prefix = icon
        .filter(|icon| {
            unicode_width::UnicodeWidthStr::width(*icon) < width.saturating_sub(suffix_width)
        })
        .map(|icon| format!("{icon} "))
        .unwrap_or_default();
    let reserved = unicode_width::UnicodeWidthStr::width(prefix.as_str()) + suffix_width;
    let name = SidebarCaptionPart::fit(
        crate::metadata::pane_title(pane, snapshot),
        width.saturating_sub(reserved) as u16,
    );
    let title_start = unicode_width::UnicodeWidthStr::width(prefix.as_str());
    let padding = width.saturating_sub(reserved + usize::from(name.width));
    let mut label = String::with_capacity(width * 4);
    label.push_str(&prefix);
    name.push_to(&mut label);
    label.extend(std::iter::repeat_n(' ', padding));
    label.push_str(&status);
    (label, title_start as u16..title_start as u16 + name.width)
}

fn sidebar_branch(pane: &PaneInfo, width: u16) -> String {
    let Some(project) = &pane.project else {
        return String::new();
    };
    let Some(repository) = &project.repository else {
        return match project.state {
            ProjectState::Loading => "Git loading…".into(),
            ProjectState::Error => "Git unavailable".into(),
            _ => String::new(),
        };
    };
    // Freshness owns the existing icon cell, not branch/count columns. Cached
    // facts keep their geometry and semantic ink while their status changes.
    let marker = match project.state {
        ProjectState::Ready => project
            .ci
            .as_ref()
            .and_then(|ci| ci.change_request.as_ref())
            .map_or("\u{f062c}", |request| crate::ci::request(request.state).0),
        ProjectState::Loading => "\u{f0996}",
        ProjectState::Error | ProjectState::NotRepository => "!",
    };
    if width < 2 {
        return if width == 0 {
            String::new()
        } else {
            marker.into()
        };
    }
    let badge = if project.ci.is_some() && width >= 6 {
        format!(" {} ", crate::ci::WORKFLOW)
    } else {
        String::new()
    };
    let width = width.saturating_sub(unicode_width::UnicodeWidthStr::width(badge.as_str()) as u16);
    let branch = repository.branch.as_deref().unwrap_or("detached");
    // Only observed committed totals correspond to main's branch-versus-base
    // counts. Missing comparison data is not a fabricated zero.
    let counts = repository
        .committed
        .map(|changes| format!(" +{} -{}", changes.insertions, changes.deletions))
        .unwrap_or_default();
    let reserved = unicode_width::UnicodeWidthStr::width(counts.as_str());
    let counts = if reserved + 5 <= usize::from(width) {
        counts.as_str()
    } else {
        ""
    };
    let available =
        width.saturating_sub((unicode_width::UnicodeWidthStr::width(counts) + 2) as u16);
    let branch = bkpr_tui_framework::window::fit(branch, available);
    format!("{marker} {branch}{counts}{badge}")
}

fn project_label(project: &ProjectSnapshot) -> String {
    let state = match project.state {
        ProjectState::Loading => "loading",
        ProjectState::Error => "error",
        ProjectState::NotRepository => "not a repository",
        ProjectState::Ready => "",
    };
    let Some(repository) = &project.repository else {
        return format!("Git: {state}");
    };
    let branch = repository
        .branch
        .as_deref()
        .unwrap_or(if repository.head_sha.is_some() {
            "detached"
        } else {
            "unborn"
        });
    let worktree = repository
        .root
        .file_name()
        .unwrap_or(repository.root.as_os_str())
        .to_string_lossy();
    let suffix = if state.is_empty() {
        String::new()
    } else {
        format!(" / {state}; stale")
    };
    if worktree == repository.name {
        format!("{} / {branch}{suffix}", repository.name)
    } else {
        format!("{} / {worktree} / {branch}{suffix}", repository.name)
    }
}

fn change_label(summary: &ChangeSummary) -> String {
    format!(
        "{} files +{} -{} ({} binary)",
        summary.files, summary.insertions, summary.deletions, summary.binary_files
    )
}

pub(crate) fn project_details(project: Option<&ProjectSnapshot>) -> Vec<String> {
    let Some(project) = project else {
        return vec!["Project: awaiting foreground directory".into()];
    };
    let mut lines = vec![
        format!("CWD: {}", project.cwd.display()),
        format!("Project: {}", project_label(project)),
    ];
    if let Some(error) = &project.error {
        lines.push(format!("Git error: {error}"));
    }
    if let Some(repository) = &project.repository {
        lines.extend([
            format!("Worktree: {}", repository.root.display()),
            format!("Common Git directory: {}", repository.common_dir.display()),
            format!("Worktree Git directory: {}", repository.git_dir.display()),
            format!(
                "HEAD: {}",
                repository
                    .head_sha
                    .as_deref()
                    .unwrap_or("unborn / unavailable")
            ),
            format!("Staged: {}", change_label(&repository.staged)),
            format!("Unstaged: {}", change_label(&repository.unstaged)),
            format!("Untracked: {} files", repository.untracked_files),
            format!(
                "Base ref: {}",
                repository.base_ref.as_deref().unwrap_or("unknown")
            ),
            format!(
                "Merge base: {}",
                repository.base_sha.as_deref().unwrap_or("unknown")
            ),
            format!(
                "Committed HEAD vs merge base: {}",
                repository
                    .committed
                    .as_ref()
                    .map(change_label)
                    .unwrap_or_else(|| "unknown (no suitable local base)".into())
            ),
        ]);
        if project.state != ProjectState::Ready {
            lines.push("Repository values above are last-known, not a ready result.".into());
        }
    }
    lines
}

fn wrapped(value: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation as _;
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for glyph in value.graphemes(true) {
        let columns = unicode_width::UnicodeWidthStr::width(glyph);
        if used + columns > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if columns <= width {
            line.push_str(glyph);
            used += columns;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
fn line(
    x: u16,
    y: u16,
    width: u16,
    label: impl Into<String>,
    selected: bool,
) -> (Rect, String, bool) {
    (
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        label.into(),
        selected,
    )
}
pub(crate) fn tree_popup_target(
    tree: &crate::tree::Tree,
    blocked: bool,
) -> Option<&crate::tree::Target> {
    if blocked {
        None
    } else {
        tree.hover
            .as_ref()
            .or_else(|| tree.active.then_some(tree.cursor.as_ref()).flatten())
    }
}

pub struct ChromeContext<'a> {
    pub size: Size,
    pub state: &'a Controller,
    pub registry: &'a RegistrySnapshot,
    pub rects: &'a [(String, Rect)],
    /// Frame text is omitted only for material-ready rectangles.
    pub pane_graphics: &'a [Rect],
    /// Rows from the passive guide's top through the sidebar's bottom.
    pub guide_rows: u16,
}

/// Global sidebar presentation plus the active owner's native pane chrome.
pub(crate) struct TreeChromeContext<'a> {
    pub native: ChromeContext<'a>,
    pub tree: &'a crate::tree::Tree,
    pub active: crate::tree::Owner,
    pub popup: Option<(&'a crate::tree::Target, ColorSource)>,
}
fn focused_tree_pane(
    entry: &crate::tree::TreeRow,
    session: &crate::tree::Session,
    active: crate::tree::Owner,
) -> bool {
    let index = match entry.kind {
        crate::tree::Kind::Native(
            SidebarRowKind::Pane { index, .. } | SidebarRowKind::Detail { index },
        ) => index,
        _ => return false,
    };
    let pane = &session.registry.panes[index];
    entry.owner == active
        && session.focused.as_ref() == Some(&pane.id)
        && session.workspace.as_ref() == pane.workspace_id.as_ref()
        && session.window.as_ref() == pane.window_id.as_ref()
}

struct NativeRowContext<'a> {
    registry: &'a RegistrySnapshot,
    entry: &'a crate::tree::TreeRow,
    known: Option<&'a PaneSnapshot>,
    width: u16,
    row: u16,
    highlighted: bool,
}

/// Keep chrome text, hit targets, artwork and keyboard paging in one viewport.
pub(crate) fn sidebar_content_rows(sidebar: Rect, guide_rows: u16) -> usize {
    if sidebar.width == 0 {
        return 0;
    }
    usize::from(sidebar.height.saturating_sub(guide_rows).saturating_sub(2))
}

/// Shared native formatting: the registry and row list always belong to one owner.
fn native_row_label(
    kind: SidebarRowKind,
    context: NativeRowContext<'_>,
    project_spans: &mut Vec<ProjectSpan>,
    working_titles: &mut Vec<Rect>,
) -> (String, u16) {
    let NativeRowContext {
        registry,
        entry,
        known,
        width,
        row,
        highlighted,
    } = context;
    let marker_x = entry.icon_column();
    let mut label = String::with_capacity(usize::from(width) * 2);
    entry.write_prefix(&mut label, highlighted);
    match kind {
        SidebarRowKind::Workspace { index, collapsed } => {
            let workspace = &registry.workspaces[index];
            let sources = registry
                .panes
                .iter()
                .filter(|pane| pane.workspace_id.as_ref() == Some(&workspace.id))
                .filter_map(|pane| pane.project.as_ref())
                .filter_map(|project| project.ci.as_ref());
            let badge = collapsed
                && sources.count() > 0
                && width.saturating_sub(entry.content_column()) >= 6;
            let available = width.saturating_sub(entry.content_column());
            let suffix = if badge {
                3
            } else if collapsed && available >= 5 {
                4
            } else {
                0
            };
            label.push_str("● ");
            SidebarCaptionPart::fit(&workspace.name, available.saturating_sub(suffix))
                .push_to(&mut label);
            if badge {
                write!(label, " {} ", crate::ci::WORKFLOW).unwrap();
            } else if suffix > 0 {
                label.push_str(" ···");
            }
        }
        SidebarRowKind::Projects { index } => {
            let workspace = &registry.workspaces[index];
            let mut repositories = Vec::new();
            for pane in &registry.panes {
                if pane.workspace_id.as_ref() != Some(&workspace.id) {
                    continue;
                }
                if let Some(project) = &pane.project
                    && let Some(repository) = &project.repository
                    && !repositories
                        .iter()
                        .any(|(identity, _, _)| *identity == &repository.common_dir)
                {
                    repositories.push((
                        &repository.common_dir,
                        repository.name.as_str(),
                        project.color_slot,
                    ));
                }
            }
            let mut column = usize::from(entry.content_column());
            for (position, (_, name, slot)) in repositories.into_iter().enumerate() {
                if position > 0 {
                    label.push_str(" · ");
                    column += 3;
                }
                let columns = unicode_width::UnicodeWidthStr::width(name);
                if column < usize::from(width) && columns > 0 {
                    project_spans.push(ProjectSpan {
                        rect: Rect {
                            x: column as u16,
                            y: row,
                            width: columns.min(usize::from(width) - column) as u16,
                            height: 1,
                        },
                        slot,
                    });
                }
                label.push_str(name);
                column += columns;
            }
        }
        SidebarRowKind::Window {
            workspace,
            index,
            collapsed,
        } => {
            let window = &registry.workspaces[workspace].windows[index];
            if width.saturating_sub(marker_x) >= 2 {
                label.push_str("\u{e0cc} ");
                let available = width.saturating_sub(entry.content_column());
                let suffix = if collapsed && available >= 5 { 4 } else { 0 };
                SidebarCaptionPart::fit(&window.name, available.saturating_sub(suffix))
                    .push_to(&mut label);
                if suffix > 0 {
                    label.push_str(" ···");
                }
            }
        }
        SidebarRowKind::Pane { index, projected } => {
            let pane = &registry.panes[index];
            project_spans.push(ProjectSpan {
                rect: Rect {
                    x: marker_x,
                    y: row,
                    width: 2.min(width.saturating_sub(marker_x)),
                    height: 1,
                },
                slot: pane.project.as_ref().and_then(|project| project.color_slot),
            });
            let (text, title) = sidebar_pane_label(
                pane,
                known,
                usize::from(width.saturating_sub(entry.content_column())),
            );
            if pane.status == PaneStatus::Running
                && harness_status(pane, known).is_some_and(|status| status.state == "working")
                && !title.is_empty()
            {
                working_titles.push(Rect {
                    x: entry.content_column() + title.start,
                    y: row,
                    width: title.end - title.start,
                    height: 1,
                });
            }
            write!(label, "{} {text}", if projected { "●" } else { "◌" }).unwrap();
        }
        SidebarRowKind::Detail { index } => {
            let pane = &registry.panes[index];
            label.push_str(&sidebar_branch(
                pane,
                width.saturating_sub(entry.content_column()),
            ));
        }
        SidebarRowKind::Spacer => {}
    }
    (label, marker_x)
}
pub(crate) fn tree_chrome<'a>(
    surface: &mut UiSurface,
    context: TreeChromeContext<'_>,
    pane_status: impl Fn(&str) -> Option<&'static str>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    history: impl Fn(&str) -> Option<(u64, u64)>,
    project_spans: &mut Vec<ProjectSpan>,
    working_titles: &mut Vec<Rect>,
) -> Vec<(crate::tree::Target, u16)> {
    let mut hits = Vec::new();
    render_chrome(
        surface,
        context,
        pane_status,
        snapshot,
        history,
        (project_spans, working_titles),
        |node, owner, row| hits.push((crate::tree::Target { owner, node }, row)),
    );
    hits
}

/// The same visible rows and owner-local grouping used by text and hit testing.
/// Session icons remain text (beekeeper/spinner), never a native pane's artwork.
pub(crate) fn tree_artwork_rows(
    tree: &crate::tree::Tree,
    size: Size,
    guide_rows: u16,
) -> impl Iterator<
    Item = (
        crate::tree::Owner,
        crate::chrome_graphics::Artwork,
        u16,
        u16,
    ),
> + '_ {
    let sidebar = screen_geometry(size, tree.preferred_columns).0;
    tree.rows
        .iter()
        .skip(tree.scroll)
        .take(sidebar_content_rows(sidebar, guide_rows))
        .enumerate()
        .filter_map(move |(index, row)| {
            let artwork = match row.kind {
                crate::tree::Kind::Native(SidebarRowKind::Workspace { .. }) => {
                    crate::chrome_graphics::Artwork::Hive
                }
                crate::tree::Kind::Native(SidebarRowKind::Pane { projected, .. }) => {
                    if projected {
                        crate::chrome_graphics::Artwork::Bee
                    } else {
                        crate::chrome_graphics::Artwork::BeeHidden
                    }
                }
                _ => return None,
            };
            let column = row.icon_column();
            (column + 2 <= sidebar.width.saturating_sub(1)).then_some((
                row.owner,
                artwork,
                column,
                index as u16 + 2,
            ))
        })
}
fn render_chrome<'a>(
    surface: &mut UiSurface,
    context: TreeChromeContext<'_>,
    pane_status: impl Fn(&str) -> Option<&'static str>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    history: impl Fn(&str) -> Option<(u64, u64)>,
    (project_spans, working_titles): (&mut Vec<ProjectSpan>, &mut Vec<Rect>),
    mut hit: impl FnMut(crate::tree::Node, crate::tree::Owner, u16),
) {
    let TreeChromeContext {
        native: context,
        tree,
        active,
        popup: tree_popup,
    } = context;
    let ChromeContext {
        size,
        state,
        registry,
        rects,
        pane_graphics,
        guide_rows,
    } = context;
    project_spans.clear();
    working_titles.clear();
    let (sidebar, area) = screen_geometry(size, tree.preferred_columns);
    let mut lines = Vec::new();
    // Hidden cursors still determine terminal cursor trails. Only a rendered
    // keyboard selection supplies an anchor; passive hover cards never do.
    let mut cursor = Cursor {
        x: size.cols,
        y: size.rows,
        visible: false,
        blinking: false,
        shape: CursorShape::Block,
        color: None,
    };
    if sidebar.width > 0 && sidebar.height > 0 {
        lines.push(line(
            1,
            0,
            sidebar.width.saturating_sub(1),
            if tree
                .rows
                .first()
                .is_some_and(|row| matches!(row.kind, crate::tree::Kind::Session))
            {
                "Sessions · Ctrl-b S"
            } else {
                "Workspaces"
            },
            false,
        ));
        if tree.sessions_len() == 1
            && let Some(session) = tree.session(active)
            && session.group.status == SessionStatus::Online
            && let Some(host) = session.group.host.as_deref()
            && sidebar.width >= 3
        {
            let host = SidebarCaptionPart::fit(host, sidebar.width - 3);
            let width = host.width + 2;
            let mut label = String::with_capacity(usize::from(width) * 4);
            label.push_str("\u{f048d} ");
            host.push_to(&mut label);
            lines.push(line(sidebar.width - width, 1, width, label, false));
        }
    }
    for (index, entry) in tree
        .rows
        .iter()
        .skip(tree.scroll)
        .take(sidebar_content_rows(sidebar, guide_rows))
        .enumerate()
    {
        let Some(session) = tree.session(entry.owner) else {
            continue;
        };
        let row = index as u16 + 2;
        let highlighted =
            tree_popup.is_some_and(|(target, _)| entry.target.as_ref() == Some(target));
        let plate = highlighted
            && tree_popup.is_some_and(|(_, background)| matches!(background, ColorSource::Rgb(_)));
        let selected = tree.active && entry.target.is_some() && entry.target == tree.cursor;
        let (mut label, marker_x) = match entry.kind {
            crate::tree::Kind::Session => (
                session_caption(
                    &session.group,
                    state.session_spinner,
                    sidebar.width.saturating_sub(1),
                ),
                1,
            ),
            crate::tree::Kind::Native(kind) => {
                let known = match kind {
                    SidebarRowKind::Pane { index, .. } => {
                        session.snapshots.get(&session.registry.panes[index].id)
                    }
                    _ => None,
                };
                native_row_label(
                    kind,
                    NativeRowContext {
                        registry: &session.registry,
                        entry,
                        known,
                        width: sidebar.width,
                        row,
                        highlighted,
                    },
                    project_spans,
                    working_titles,
                )
            }
        };
        if selected
            && marker_x < sidebar.width
            && matches!(
                entry.kind,
                crate::tree::Kind::Session
                    | crate::tree::Kind::Native(
                        SidebarRowKind::Workspace { .. }
                            | SidebarRowKind::Window { .. }
                            | SidebarRowKind::Pane { .. }
                    )
            )
        {
            cursor.x = marker_x;
            cursor.y = row;
        }
        let row_selected = (selected || focused_tree_pane(entry, session, active)) && !plate;
        if row_selected {
            let columns = unicode_width::UnicodeWidthStr::width(label.as_str());
            label.extend(std::iter::repeat_n(
                ' ',
                usize::from(sidebar.width.saturating_sub(1)).saturating_sub(columns),
            ));
        }
        lines.push(line(
            1,
            row,
            sidebar.width.saturating_sub(1),
            label,
            row_selected,
        ));
        if let Some(target) = &entry.target {
            hit(target.node.clone(), target.owner, row);
        }
    }
    if rects.is_empty()
        && let Some(group) = tree.session(active).map(|session| &session.group)
        && group.status != SessionStatus::Online
    {
        let width = area.width.saturating_sub(4);
        if width > 0 {
            let title = format!("{} {}", group.name, group.status.badge());
            let rows = wrapped(&title, usize::from(width)).into_iter()
                .chain(std::iter::once(String::new()))
                .chain(wrapped(&group.detail, usize::from(width)))
                .chain(std::iter::once(String::new()))
                .chain(wrapped("Ctrl-b S selects a session row. Arrows navigate the tree, Enter returns to the application, R forces reconnect. Quit detaches only.", usize::from(width)));
            for (row, label) in rows
                .take(usize::from(area.height.saturating_sub(4)))
                .enumerate()
            {
                lines.push(line(
                    area.x.saturating_add(2),
                    row as u16 + 2,
                    width,
                    label,
                    false,
                ));
            }
        }
    }
    for (id, rect) in rects {
        if pane_graphics.contains(rect) {
            continue;
        }
        let title = registry
            .panes
            .iter()
            .find(|p| p.id == *id)
            .map(|pane| {
                let known = snapshot(id);
                let icon = recognized_harness(pane, known)
                    .map(|harness| format!("{} ", harness_icon(harness.kind)))
                    .unwrap_or_default();
                let status = pane_activity(pane, known);
                format!(
                    "{icon}{}{}",
                    crate::metadata::pane_title(pane, known),
                    if status.is_empty() {
                        String::new()
                    } else {
                        format!(" {status}")
                    }
                )
            })
            .unwrap_or_else(|| "Lost pane".into());
        let title = if let Some(status) = pane_status(id) {
            format!("[{status}] {title}")
        } else {
            title
        };
        let title = if let Some((offset, total)) = history(id) {
            format!("[HISTORY -{offset}/{total}] {title}")
        } else {
            title
        };
        let title = format!(
            " {} {}",
            if state.focused.as_ref() == Some(id) {
                "●"
            } else {
                "○"
            },
            title
        );
        if rect.width >= 3 && rect.height >= 3 {
            let local = rect.size(size);
            lines.extend(
                bkpr_tui_framework::window::border_rows(
                    local,
                    &title,
                    bkpr_tui_framework::window::Border::Rounded,
                )
                .into_iter()
                .map(|(local, text, selected)| {
                    (
                        Rect {
                            x: rect.x + local.x,
                            y: rect.y + local.y,
                            ..local
                        },
                        text,
                        selected,
                    )
                }),
            );
        } else if rect.height > 1 {
            lines.push(line(rect.x, rect.y, rect.width, title, false));
        }
    }
    surface.paint(size, lines, Some(cursor));
}

/// Product hierarchy only: the framework resolves every role against the host.
/// These spans cover our chrome frame, never a native pane's application scene.
pub(crate) fn theme_tree_chrome<'a>(
    surface: &mut UiSurface,
    theme: &Theme,
    context: TreeChromeContext<'_>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    palette: &ProjectPalette,
    projects: &[ProjectSpan],
    foregrounds: &mut Vec<ForegroundSpan>,
) {
    render_theme_chrome(
        surface,
        theme,
        context,
        snapshot,
        palette,
        projects,
        foregrounds,
    );
}

fn render_theme_chrome<'a>(
    surface: &mut UiSurface,
    theme: &Theme,
    context: TreeChromeContext<'_>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    palette: &ProjectPalette,
    projects: &[ProjectSpan],
    foregrounds: &mut Vec<ForegroundSpan>,
) {
    let TreeChromeContext {
        native: context,
        tree,
        popup: tree_popup,
        active,
    } = context;
    let ChromeContext {
        state,
        registry,
        rects,
        pane_graphics,
        guide_rows,
        ..
    } = context;
    fn activity_role(status: &ProgramStatus) -> Role {
        match status.state.as_str() {
            "idle" => Role::Success,
            "working" => Role::Warning,
            "done" => Role::Info,
            "blocked" | "error" => Role::Error,
            _ => Role::Muted,
        }
    }
    let size = surface.frame.size;
    let sidebar = screen_geometry(size, tree.preferred_columns).0;
    let mut spans = Vec::new();
    let mut backgrounds = Vec::new();
    foregrounds.clear();
    let muted = theme.ink(Role::Muted, false);
    spans.push(RoleSpan {
        rect: Rect {
            height: 1,
            ..sidebar
        },
        role: Role::Title,
    });
    if tree.sessions_len() == 1
        && tree.session(active).is_some_and(|session| {
            session.group.host.is_some() && session.group.status == SessionStatus::Online
        })
    {
        spans.push(RoleSpan {
            rect: Rect {
                x: 1,
                y: 1,
                width: sidebar.width.saturating_sub(1),
                height: 1,
            },
            role: Role::Muted,
        });
    }
    let available = sidebar_content_rows(sidebar, guide_rows);
    let focus_edges = tree.focus_edges();
    let inactive_guide_ink = crate::appearance::inactive_tree_guide_ink(theme);
    for index in 0..available {
        let row = index as u16 + 2;
        let Some(entry) = tree.rows.get(tree.scroll + index) else {
            break;
        };
        let Some(session) = tree.session(entry.owner) else {
            continue;
        };
        let background = tree_popup
            .filter(|(target, _)| entry.target.as_ref() == Some(*target))
            .map(|(_, background)| background)
            .or_else(|| {
                (entry.target.is_some() && entry.target == tree.hover)
                    .then(|| theme.background(Role::Hover, false))
            });
        if matches!(entry.kind, crate::tree::Kind::Session) {
            let rect = Rect {
                x: 1,
                y: row,
                width: sidebar.width.saturating_sub(1),
                height: 1,
            };
            spans.push(RoleSpan {
                rect,
                role: if entry.owner == active {
                    Role::Title
                } else {
                    Role::Base
                },
            });
            let tail_width = session_tail_width(&session.group, rect.width);
            if tail_width > 0 {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: rect.x + rect.width - tail_width,
                        width: tail_width,
                        ..rect
                    },
                    role: session.group.tail_role(),
                });
            }
            if let Some(background) = background {
                backgrounds.push(BackgroundSpan { rect, background });
            }
            continue;
        }
        let crate::tree::Kind::Native(kind) = entry.kind else {
            unreachable!()
        };
        let registry = session.registry.as_ref();
        let known = match kind {
            SidebarRowKind::Pane { index, .. } => session.snapshots.get(&registry.panes[index].id),
            _ => None,
        };
        let role = match kind {
            SidebarRowKind::Workspace { .. } => Role::Title,
            SidebarRowKind::Window { .. } => Role::Base,
            SidebarRowKind::Pane { .. } => {
                if focused_tree_pane(entry, session, active) {
                    Role::Title
                } else {
                    Role::Base
                }
            }
            SidebarRowKind::Projects { .. }
            | SidebarRowKind::Detail { .. }
            | SidebarRowKind::Spacer => Role::Muted,
        };
        spans.push(RoleSpan {
            rect: Rect {
                x: 1,
                y: row,
                width: sidebar.width.saturating_sub(1),
                height: 1,
            },
            role,
        });
        let row_rect = Rect {
            x: 1,
            y: row,
            width: sidebar.width.saturating_sub(1),
            height: 1,
        };
        if let Some(background) = background {
            backgrounds.push(BackgroundSpan {
                rect: row_rect,
                background,
            });
        }
        let prefix_width = if matches!(kind, SidebarRowKind::Spacer) {
            entry.depth * 3
        } else if matches!(
            kind,
            SidebarRowKind::Projects { .. } | SidebarRowKind::Detail { .. }
        ) {
            entry.content_column().saturating_sub(1)
        } else {
            entry.icon_column().saturating_sub(1)
        };
        for level in 1..=prefix_width.div_ceil(3) {
            let x = 1 + (level - 1) * 3;
            let rect = Rect {
                x,
                width: 3
                    .min(prefix_width - (x - 1))
                    .min(sidebar.width.saturating_sub(x)),
                ..row_rect
            };
            let bright = focus_edges.iter().flatten().any(|edge| {
                edge.depth == level && (edge.start..=edge.end).contains(&(tree.scroll + index))
            });
            spans.push(RoleSpan {
                rect,
                role: if bright { Role::Base } else { Role::Muted },
            });
            if !bright {
                foregrounds.push(ForegroundSpan {
                    rect,
                    foreground: inactive_guide_ink,
                });
            }
        }
        let ci_status = match kind {
            SidebarRowKind::Detail { index } => registry.panes[index]
                .project
                .as_ref()
                .and_then(|project| project.ci.as_ref())
                .map(crate::ci::state),
            SidebarRowKind::Workspace {
                index,
                collapsed: true,
            } => {
                let mut sources = registry
                    .panes
                    .iter()
                    .filter(|pane| {
                        pane.workspace_id.as_ref() == Some(&registry.workspaces[index].id)
                    })
                    .filter_map(|pane| pane.project.as_ref())
                    .filter_map(|project| project.ci.as_ref())
                    .map(crate::ci::state)
                    .peekable();
                sources
                    .peek()
                    .is_some()
                    .then(|| bkpr_protocol::CiStatus::aggregate(sources))
            }
            _ => None,
        };
        if let Some(status) = ci_status {
            let cells = &surface.frame.rows[usize::from(row)].cells[..usize::from(sidebar.width)];
            if let Some(x) = cells
                .iter()
                .position(|cell| cell.text == crate::ci::WORKFLOW)
            {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: x as u16,
                        y: row,
                        width: 1,
                        height: 1,
                    },
                    role: crate::ci::role(status),
                });
            }
        }
        if let SidebarRowKind::Detail { index } = kind
            && let Some(project) = &registry.panes[index].project
            && project.state == ProjectState::Ready
            && let Some(request) = project
                .ci
                .as_ref()
                .and_then(|ci| ci.change_request.as_ref())
        {
            spans.push(RoleSpan {
                rect: Rect {
                    x: entry.content_column(),
                    y: row,
                    width: 1,
                    height: 1,
                },
                role: crate::ci::request(request.state).1,
            });
        }
        if let SidebarRowKind::Detail { index: pane_index } = kind
            && let Some(project) = &registry.panes[pane_index].project
            && project.state != ProjectState::Ready
        {
            spans.push(RoleSpan {
                rect: Rect {
                    x: entry.content_column(),
                    y: row,
                    width: 1.min(sidebar.width.saturating_sub(entry.content_column())),
                    height: 1,
                },
                role: if project.state == ProjectState::Loading {
                    Role::Warning
                } else {
                    Role::Error
                },
            });
        }
        if let SidebarRowKind::Detail { index: pane_index } = kind
            && let Some(project) = &registry.panes[pane_index].project
            && let Some(changes) = project
                .repository
                .as_ref()
                .and_then(|repository| repository.committed)
        {
            let added = changes.insertions.checked_ilog10().unwrap_or(0) as usize + 1;
            let removed = changes.deletions.checked_ilog10().unwrap_or(0) as usize + 1;
            let badge_width = if project.ci.is_some() { 3 } else { 0 };
            let available = usize::from(sidebar.width.saturating_sub(entry.content_column()))
                .saturating_sub(badge_width);
            if added + removed + 9 <= available {
                let y = row;
                let cells = &surface.frame.rows[usize::from(y)].cells[..usize::from(sidebar.width)];
                if let Some(end) = cells
                    .iter()
                    .rposition(|cell| cell.text != " " && cell.text != crate::ci::WORKFLOW)
                    && let Some(remove_x) = (end + 1).checked_sub(removed + 1)
                    && let Some(add_x) = remove_x.checked_sub(added + 2)
                    && cells[remove_x].text == "-"
                    && cells[add_x].text == "+"
                {
                    spans.push(RoleSpan {
                        rect: Rect {
                            x: add_x as u16,
                            y,
                            width: added as u16 + 1,
                            height: 1,
                        },
                        role: Role::Success,
                    });
                    spans.push(RoleSpan {
                        rect: Rect {
                            x: remove_x as u16,
                            y,
                            width: removed as u16 + 1,
                            height: 1,
                        },
                        role: Role::Error,
                    });
                }
            }
        }
        if let SidebarRowKind::Pane {
            index: pane_index, ..
        } = kind
        {
            let pane = &registry.panes[pane_index];
            if let Some(harness) = recognized_harness(pane, known) {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: entry.content_column(),
                        y: row,
                        width: 2,
                        height: 1,
                    },
                    role: harness_role(harness.kind),
                });
            }
            if let Some(status) = harness_status(pane, known) {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: sidebar.width.saturating_sub(1),
                        y: row,
                        width: 1,
                        height: 1,
                    },
                    role: activity_role(status),
                });
            }
        }
    }
    for (id, rect) in rects {
        if pane_graphics.contains(rect) {
            // Main's decorate_characters fills rich frame cells with the
            // original pane background. The atlas paints the opaque matte
            // outside its opening; its transparent interior must see normal
            // pane color, including the opened half of horizontal gutters.
            spans.push(RoleSpan {
                rect: *rect,
                role: Role::Panel,
            });
            continue;
        }
        if rect.width >= 3 && rect.height >= 3 {
            spans.extend(
                bkpr_tui_framework::window::theme_spans(rect.size(size))
                    .into_iter()
                    .map(|span| RoleSpan {
                        rect: Rect {
                            x: rect.x + span.rect.x,
                            y: rect.y + span.rect.y,
                            ..span.rect
                        },
                        role: span.role,
                    }),
            );
        }
        spans.push(RoleSpan {
            rect: Rect { height: 1, ..*rect },
            role: if state.focused.as_ref() == Some(id) {
                Role::Title
            } else {
                Role::Muted
            },
        });
        if state.focused.as_ref() == Some(id) {
            spans.push(RoleSpan {
                rect: Rect {
                    width: 3.min(rect.width),
                    height: 1,
                    ..*rect
                },
                role: Role::Accent,
            });
        }
        if let Some(pane) = registry.panes.iter().find(|pane| pane.id == *id)
            && let Some(status) = harness_status(pane, snapshot(id))
        {
            spans.push(RoleSpan {
                rect: Rect {
                    width: 3.min(rect.width),
                    height: 1,
                    ..*rect
                },
                role: activity_role(status),
            });
        }
    }
    for project in projects {
        let (role, foreground) = palette.ink(project.slot, muted);
        spans.push(RoleSpan {
            rect: project.rect,
            role,
        });
        foregrounds.push(ForegroundSpan {
            rect: project.rect,
            foreground,
        });
    }
    // The shared sidebar separator belongs to sidebar chrome in both
    // rendering modes, even though pane frame geometry starts in this cell.
    if sidebar.width > 0 && sidebar.width < size.cols {
        backgrounds.push(BackgroundSpan {
            rect: Rect {
                x: sidebar.x + sidebar.width,
                y: sidebar.y,
                width: 1,
                height: sidebar.height,
            },
            background: theme.background(Role::Base, false),
        });
    }
    for (index, entry) in tree
        .rows
        .iter()
        .skip(tree.scroll)
        .take(available)
        .enumerate()
    {
        let Some(session) = tree.session(entry.owner) else {
            continue;
        };
        // A rich attached popup owns its rounded item outline. Do not
        // extend its flat plate into the sidebar's exterior padding.
        if !pane_graphics.is_empty()
            && tree_popup.is_some_and(|(target, _)| entry.target.as_ref() == Some(target))
        {
            continue;
        }
        let selected = tree.active && entry.target.is_some() && entry.target == tree.cursor;
        let background = tree_popup
            .filter(|(target, _)| entry.target.as_ref() == Some(*target))
            .map(|(_, background)| background)
            .or_else(|| {
                (selected || focused_tree_pane(entry, session, active))
                    .then(|| theme.background(Role::Base, true))
            })
            .or_else(|| {
                (entry.target.is_some() && entry.target == tree.hover)
                    .then(|| theme.background(Role::Hover, false))
            });
        if let Some(background) = background {
            backgrounds.push(BackgroundSpan {
                rect: Rect {
                    x: sidebar.x,
                    y: index as u16 + 2,
                    width: sidebar
                        .width
                        .saturating_add(u16::from(sidebar.width < size.cols)),
                    height: 1,
                },
                background,
            });
        }
    }
    surface.apply_theme_with_colors(theme, Role::Base, &spans, foregrounds, &backgrounds);
}
pub(crate) fn project_detail_lines(
    size: Size,
    form: &Form,
    registry: &RegistrySnapshot,
) -> Vec<String> {
    let width = size.cols;
    let details = form
        .restore
        .as_ref()
        .and_then(|id| registry.panes.iter().find(|pane| &pane.id == id))
        .map(|pane| project_details(pane.project.as_ref()))
        .unwrap_or_else(|| vec!["Focused pane is unavailable".into()]);
    details
        .iter()
        .flat_map(|detail| wrapped(detail, width as usize))
        .collect()
}

pub fn modal(surface: &mut UiSurface, size: Size, form: &Form, registry: &RegistrySnapshot) {
    let x = 0;
    let y = 0;
    let width = size.cols;
    let title = match &form.kind {
        FormKind::CreateWorkspace => "Create workspace",
        FormKind::CreateWindow => "Create terminal window",
        FormKind::RenameWorkspace => "Rename workspace",
        FormKind::RenameWindow => "Rename window",
        FormKind::RenamePane => "Rename pane",
        FormKind::TerminatePane => "Close terminal and its owned processes?",
        FormKind::ReopenPane { .. } => "Open a new shell here",
        FormKind::CloseWorkspace => "Close workspace, all its windows and owned terminals?",
        FormKind::CloseWindow => "Close window and all its owned terminals?",
        FormKind::ProjectDetails => "Focused project (arrows scroll)",
    };
    if matches!(
        form.kind,
        FormKind::CreateWorkspace
            | FormKind::CreateWindow
            | FormKind::RenameWorkspace
            | FormKind::RenameWindow
            | FormKind::RenamePane
    ) {
        form.fields.paint(
            surface,
            size,
            title,
            matches!(
                form.kind,
                FormKind::CreateWorkspace | FormKind::CreateWindow
            ),
            form.error.as_deref(),
            form.pending.is_some() || form.ambiguous,
        );
        return;
    }
    let mut lines = vec![line(x, y, width, title, false)];
    if form.kind == FormKind::ProjectDetails {
        let details = project_detail_lines(size, form, registry);
        let available = size.rows.saturating_sub(y.saturating_add(4)) as usize;
        let offset = form.selection.min(details.len().saturating_sub(1));
        for (index, detail) in details.iter().skip(offset).take(available).enumerate() {
            lines.push(line(
                x,
                y.saturating_add(2).saturating_add(index as u16),
                width,
                detail,
                false,
            ));
        }
    } else if let FormKind::ReopenPane { cwd, .. } = &form.kind {
        lines.push(line(
            x,
            y.saturating_add(2),
            width,
            "A fresh shell replaces this layout leaf; the old record remains.",
            false,
        ));
        lines.push(line(
            x,
            y.saturating_add(3),
            width,
            format!(
                "{} Retained directory: {}",
                if form.selection == 0 { "▸" } else { " " },
                cwd.display()
            ),
            form.selection == 0,
        ));
        lines.push(line(
            x,
            y.saturating_add(4),
            width,
            format!(
                "{} Default launch directory: {}",
                if form.selection == 1 { "▸" } else { " " },
                form.fields.primary.value
            ),
            form.selection == 1,
        ));
        lines.push(line(
            x,
            y.saturating_add(5),
            width,
            "↑/↓ choose directory · no old program or conversation is resumed",
            false,
        ));
    } else {
        let target = match form.target.as_ref().map(|(target, _)| target) {
            Some(SidebarTarget::Workspace(id)) => registry
                .workspaces
                .iter()
                .find(|workspace| &workspace.id == id)
                .map(|workspace| workspace.name.as_str()),
            Some(SidebarTarget::Window { workspace, window }) => registry
                .workspaces
                .iter()
                .find(|entry| &entry.id == workspace)
                .and_then(|workspace| workspace.windows.iter().find(|entry| &entry.id == window))
                .map(|window| window.name.as_str()),
            Some(SidebarTarget::Pane { pane, .. }) => registry
                .panes
                .iter()
                .find(|entry| &entry.id == pane)
                .map(|pane| pane.title.as_str()),
            None => form
                .restore
                .as_ref()
                .and_then(|id| registry.panes.iter().find(|pane| &pane.id == id))
                .map(|pane| pane.title.as_str()),
        };
        if let Some(target) = target {
            lines.push(line(
                0,
                2.min(size.rows.saturating_sub(1)),
                width,
                target,
                false,
            ));
        }
    }
    lines.push(line(
        x,
        size.rows.saturating_sub(2),
        width,
        if let Some(error) = &form.error {
            error.as_str()
        } else if form.pending.is_some() {
            "Waiting for backend receipt · Esc leaves without cancelling accepted work"
        } else if form.kind == FormKind::ProjectDetails {
            "↑/↓ scroll · Enter / Esc close"
        } else if matches!(
            form.kind,
            FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow
        ) {
            "y / Enter confirms · n / Esc cancels"
        } else {
            "Enter applies · Esc cancels"
        },
        false,
    ));
    surface.paint(size, lines, None);
}

pub(crate) fn theme_modal(surface: &mut UiSurface, theme: &Theme, form: &Form) {
    if matches!(
        form.kind,
        FormKind::CreateWorkspace
            | FormKind::CreateWindow
            | FormKind::RenameWorkspace
            | FormKind::RenameWindow
            | FormKind::RenamePane
    ) {
        form.fields.apply_theme(
            surface,
            theme,
            matches!(
                form.kind,
                FormKind::CreateWorkspace | FormKind::CreateWindow
            ),
            form.error.as_deref(),
            form.pending.is_some() || form.ambiguous,
        );
        return;
    }
    let size = surface.frame.size;
    let mut spans = vec![
        RoleSpan {
            rect: Rect {
                x: 0,
                y: 0,
                width: size.cols,
                height: 1,
            },
            role: Role::Title,
        },
        RoleSpan {
            rect: Rect {
                x: 0,
                y: size.rows.saturating_sub(2),
                width: size.cols,
                height: 1,
            },
            role: if form.error.is_some() {
                Role::Error
            } else if form.ambiguous {
                Role::Warning
            } else {
                Role::Muted
            },
        },
    ];
    if matches!(
        form.kind,
        FormKind::TerminatePane | FormKind::CloseWorkspace | FormKind::CloseWindow
    ) {
        spans.push(RoleSpan {
            rect: Rect {
                x: 0,
                y: 0,
                width: size.cols,
                height: 1,
            },
            role: Role::Warning,
        });
    }
    surface.apply_theme(theme, Role::Panel, &spans);
}

#[cfg(test)]
mod tests {
    #[derive(Clone, Copy)]
    struct FixtureChrome<'a> {
        size: Size,
        state: &'a Controller,
        registry: &'a RegistrySnapshot,
        rects: &'a [(String, Rect)],
        pane_graphics: &'a [Rect],
        guide_rows: u16,
        sidebar_popup: Option<(&'a SidebarTarget, ColorSource)>,
    }
    impl<'a> FixtureChrome<'a> {
        fn native(self) -> ChromeContext<'a> {
            ChromeContext {
                size: self.size,
                state: self.state,
                registry: self.registry,
                rects: self.rects,
                pane_graphics: self.pane_graphics,
                guide_rows: self.guide_rows,
            }
        }
    }
    const FIXTURE_OWNER: crate::tree::Owner = crate::tree::Owner {
        slot: 0,
        generation: 1,
    };
    fn fixture_tree<'a>(
        context: &FixtureChrome<'_>,
        pane_status: impl Fn(&str) -> Option<&'static str>,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
        history: impl Fn(&str) -> Option<(u64, u64)>,
    ) -> crate::tree::Tree {
        use std::{collections::HashMap, sync::Arc};
        let mut tree = crate::tree::Tree::new();
        let state = context.state;
        tree.reconcile(
            vec![Arc::new(crate::tree::Session {
                owner: FIXTURE_OWNER,
                group: SessionGroup {
                    name: "fixture".into(),
                    host: None,
                    status: SessionStatus::Online,
                    detail: String::new(),
                },
                registry: Arc::new(context.registry.clone()),
                rows: state.sidebar.rows.clone().into(),
                workspace: state.workspace.clone(),
                window: state.window.clone(),
                focused: state.focused.clone(),
                snapshots: Arc::new(
                    context
                        .registry
                        .panes
                        .iter()
                        .filter_map(|pane| {
                            snapshot(&pane.id)
                                .cloned()
                                .map(|snapshot| (pane.id.clone(), snapshot))
                        })
                        .collect::<HashMap<_, _>>(),
                ),
                statuses: Arc::new(
                    context
                        .registry
                        .panes
                        .iter()
                        .filter_map(|pane| {
                            pane_status(&pane.id).map(|status| (pane.id.clone(), status))
                        })
                        .collect(),
                ),
                history: Arc::new(
                    context
                        .registry
                        .panes
                        .iter()
                        .filter_map(|pane| {
                            history(&pane.id).map(|history| (pane.id.clone(), history))
                        })
                        .collect(),
                ),
            })],
            FIXTURE_OWNER,
        );
        let qualify = |native| crate::tree::Target {
            owner: FIXTURE_OWNER,
            node: crate::tree::Node::Native(native),
        };
        tree.active = state.sidebar.active;
        tree.cursor = state.sidebar.cursor.clone().map(qualify);
        tree.hover = state.sidebar.hover.clone().map(qualify);
        tree.preferred_columns = state.sidebar.preferred_columns;
        let height = sidebar_content_rows(
            screen_geometry(context.size, tree.preferred_columns).0,
            context.guide_rows,
        );
        tree.viewport(height);
        tree.scroll = state.sidebar.scroll;
        tree
    }
    fn paint_fixture<'a>(
        surface: &mut UiSurface,
        context: FixtureChrome<'_>,
        pane_status: impl Fn(&str) -> Option<&'static str>,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
        history: impl Fn(&str) -> Option<(u64, u64)>,
        projects: &mut Vec<ProjectSpan>,
    ) -> Vec<(SidebarTarget, u16)> {
        let tree = fixture_tree(&context, &pane_status, &snapshot, &history);
        let popup = context.sidebar_popup.map(|(target, color)| {
            (
                crate::tree::Target {
                    owner: FIXTURE_OWNER,
                    node: crate::tree::Node::Native(target.clone()),
                },
                color,
            )
        });
        tree_chrome(
            surface,
            TreeChromeContext {
                native: context.native(),
                tree: &tree,
                active: FIXTURE_OWNER,
                popup: popup.as_ref().map(|(target, color)| (target, *color)),
            },
            pane_status,
            snapshot,
            history,
            projects,
            &mut Vec::new(),
        )
        .into_iter()
        .filter_map(|(target, row)| match target.node {
            crate::tree::Node::Native(native) => Some((native, row)),
            _ => None,
        })
        .collect()
    }
    fn style_fixture<'a>(
        surface: &mut UiSurface,
        theme: &Theme,
        context: FixtureChrome<'_>,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
        palette: &ProjectPalette,
        projects: &[ProjectSpan],
        foregrounds: &mut Vec<ForegroundSpan>,
    ) {
        let tree = fixture_tree(&context, |_| None, &snapshot, |_| None);
        let popup = context.sidebar_popup.map(|(target, color)| {
            (
                crate::tree::Target {
                    owner: FIXTURE_OWNER,
                    node: crate::tree::Node::Native(target.clone()),
                },
                color,
            )
        });
        theme_tree_chrome(
            surface,
            theme,
            TreeChromeContext {
                native: context.native(),
                tree: &tree,
                active: FIXTURE_OWNER,
                popup: popup.as_ref().map(|(target, color)| (target, *color)),
            },
            snapshot,
            palette,
            projects,
            foregrounds,
        );
    }

    fn session_fixture(
        owner: crate::tree::Owner,
        label: &str,
    ) -> std::sync::Arc<crate::tree::Session> {
        let (mut pane, snapshot) = capture_fixture();
        pane.title = label.into();
        pane.project = Some(project_fixture());
        let registry = capture_registry(&pane);
        let state = Controller::new(&registry);
        std::sync::Arc::new(crate::tree::Session {
            owner,
            group: super::SessionGroup {
                name: label.into(),
                host: None,
                status: super::SessionStatus::Online,
                detail: format!("{label} session details"),
            },
            registry: std::sync::Arc::new(registry),
            rows: state.sidebar.rows.clone().into(),
            workspace: state.workspace,
            window: state.window,
            focused: state.focused,
            snapshots: std::sync::Arc::new(std::collections::HashMap::from([(pane.id, snapshot)])),
            statuses: Default::default(),
            history: Default::default(),
        })
    }

    #[test]
    fn unavailable_body_uses_active_owner_not_hovered_or_selected_session() {
        use crate::tree::{Node, Owner, Target, Tree};
        let active = Owner {
            slot: 0,
            generation: 4,
        };
        let other = Owner {
            slot: 1,
            generation: 8,
        };
        let mut first = session_fixture(active, "first");
        let mut second = session_fixture(other, "second");
        std::sync::Arc::get_mut(&mut first).unwrap().group.status = super::SessionStatus::Offline;
        std::sync::Arc::get_mut(&mut second).unwrap().group.status = super::SessionStatus::Offline;
        let state = Controller::new(&first.registry);
        let mut tree = Tree::new();
        tree.reconcile(vec![first.clone(), second], active);
        tree.enter();
        tree.select(Target {
            owner: other,
            node: Node::Session,
        });
        let mut surface = capture_surface();
        let size = surface.frame.size;
        super::tree_chrome(
            &mut surface,
            super::TreeChromeContext {
                native: ChromeContext {
                    size,
                    state: &state,
                    registry: &first.registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                tree: &tree,
                active,
                popup: None,
            },
            |_| panic!("No native pane frame"),
            |_| panic!("Unavailable body cannot poll panes"),
            |_| panic!("No native pane frame"),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let text = painted_text(&surface);
        assert!(text.contains("first session details"));
        assert!(!text.contains("second session details"));
    }

    #[test]
    fn unified_tree_uses_owner_metadata_and_one_geometry_for_hits_cursor_and_artwork() {
        use crate::tree::{Node, Owner, Target, Tree};
        let first = Owner {
            slot: 0,
            generation: 4,
        };
        let second = Owner {
            slot: 1,
            generation: 8,
        };
        let local = session_fixture(first, "local-native");
        let foreign = session_fixture(second, "foreign-native");
        let size = Size {
            rows: 40,
            ..capture_surface().frame.size
        };
        let mut tree = Tree::new();
        tree.reconcile(vec![local.clone(), foreign.clone()], first);
        tree.viewport(sidebar_content_rows(screen_geometry(size, None).0, 0));
        tree.enter();
        let target = Target {
            owner: second,
            node: Node::Native(SidebarTarget::Pane {
                workspace: "work".into(),
                pane: foreign.registry.panes[0].id.clone(),
            }),
        };
        tree.select(target.clone());
        let state = Controller::new(&local.registry);
        let mut surface = UiSurface::new(size);
        let hits = super::tree_chrome(
            &mut surface,
            super::TreeChromeContext {
                native: ChromeContext {
                    size,
                    state: &state,
                    registry: &local.registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                tree: &tree,
                active: first,
                popup: Some((&target, ColorSource::Rgb([40, 50, 60]))),
            },
            |_| panic!("No native pane frame"),
            |_| panic!("Tree snapshots must come from their owner"),
            |_| panic!("No native pane frame"),
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert!(matches!(&hits[0], (Target { owner, node: Node::Session }, 2) if *owner == first));
        let native_row = hits.iter().find(|(hit, _)| hit == &target).unwrap().1;
        let text: String = surface.frame.rows[usize::from(native_row)]
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect();
        assert!(text.contains("foreign-native"));
        assert!(!text.contains("local-native"));
        assert_eq!(
            surface.frame.cursor.y, native_row,
            "Detail rows cannot move the icon anchor"
        );
        let (_, _, column, row) = super::tree_artwork_rows(&tree, size, 0)
            .find(|(owner, artwork, _, row)| {
                *owner == second
                    && *artwork == crate::chrome_graphics::Artwork::Bee
                    && *row == native_row
            })
            .unwrap();
        assert_eq!(
            (surface.frame.cursor.x, surface.frame.cursor.y),
            (column, row)
        );
        assert!(hits.iter().any(|(hit, _)| hit.owner == first));
        assert!(hits.iter().any(|(hit, _)| hit.owner == second));
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &Default::default());
        super::theme_tree_chrome(
            &mut surface,
            &theme,
            super::TreeChromeContext {
                native: ChromeContext {
                    size,
                    state: &state,
                    registry: &local.registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                tree: &tree,
                active: first,
                popup: Some((&target, ColorSource::Rgb([40, 50, 60]))),
            },
            |_| panic!("Foreign theme metadata must use cached source snapshots"),
            &ProjectPalette::default(),
            &[],
            &mut Vec::new(),
        );
        assert_eq!(
            surface.frame.rows[usize::from(native_row)].cells[1]
                .style
                .background_source,
            ColorSource::Rgb([40, 50, 60])
        );
        let local_row = hits
            .iter()
            .find(|(hit, _)| hit.owner == first && hit.node == target.node)
            .unwrap()
            .1;
        assert_ne!(
            surface.frame.rows[usize::from(local_row)].cells[1]
                .style
                .background_source,
            ColorSource::Rgb([40, 50, 60])
        );
    }
    fn caption_group(name: &str, host: Option<&str>, status: SessionStatus) -> SessionGroup {
        SessionGroup {
            name: name.into(),
            host: host.map(str::to_owned),
            status,
            detail: String::new(),
        }
    }

    #[test]
    fn remote_host_is_a_separate_muted_tail_and_status_replaces_its_icon() {
        use bkpr_tui_framework::Role;
        use unicode_width::UnicodeWidthStr as _;
        for (status, icon, role) in [
            (SessionStatus::Online, '\u{f048d}', Role::Muted),
            (SessionStatus::Queued, '\u{f0150}', Role::Muted),
            (SessionStatus::Connecting, '\u{f0aa1}', Role::Warning),
            (SessionStatus::Reconnecting, '\u{f0aa1}', Role::Warning),
            (SessionStatus::Activating, '\u{f0aa1}', Role::Warning),
            (SessionStatus::Offline, '\u{f048e}', Role::Error),
            (SessionStatus::Rejected, '\u{f048e}', Role::Error),
        ] {
            let group = caption_group("literal/session:name", Some("user@host"), status);
            let caption = super::session_caption(&group, '\u{f0aa1}', 40);
            assert!(caption.starts_with("\u{f14e2} literal/session:name "));
            assert!(caption.ends_with(&format!("{icon} user@host")));
            assert_eq!(caption.matches(icon).count(), 1);
            assert_eq!(caption.width(), 40);
            let tail = super::session_tail_width(&group, 40);
            assert_eq!(
                caption[..caption.find(icon).unwrap()].width(),
                usize::from(40 - tail)
            );
            assert_eq!(group.tail_role(), role);
            if status != SessionStatus::Online {
                assert!(!caption.contains('\u{f048d}'));
            }
        }
    }

    #[test]
    fn local_sessions_have_no_host_tail_and_pending_status_stays_icon_only() {
        let mut group = caption_group("local", None, SessionStatus::Online);
        assert_eq!(
            super::session_caption(&group, '\u{f0aa1}', 7),
            "\u{f14e2} local"
        );
        assert_eq!(super::session_tail_width(&group, 7), 0);
        for (status, icon) in [
            (SessionStatus::Queued, '\u{f0150}'),
            (SessionStatus::Connecting, '\u{f0aa1}'),
            (SessionStatus::Offline, '\u{f0c5f}'),
        ] {
            group.status = status;
            let caption = super::session_caption(&group, '\u{f0aa1}', 12);
            assert!(caption.starts_with("\u{f14e2} local "));
            assert!(caption.ends_with(icon));
            assert_eq!(super::session_tail_width(&group, 12), 1);
        }
    }

    #[test]
    fn long_hosts_cannot_crowd_out_session_names_and_short_names_release_space() {
        use unicode_width::UnicodeWidthStr as _;
        let group = caption_group(
            "session-with-a-long-name",
            Some("host-with-a-long-name"),
            SessionStatus::Online,
        );
        let caption = super::session_caption(&group, ' ', 18);
        let (name, host) = caption["\u{f14e2} ".len()..]
            .split_once('\u{f048d}')
            .unwrap();
        assert!(name.trim_end().width() >= 7);
        assert!(name.trim_end().ends_with('…'));
        assert!(host.ends_with('…'));
        assert!(name.ends_with(' '));
        let short = caption_group("x", group.host.as_deref(), SessionStatus::Online);
        assert!(super::session_tail_width(&short, 18) > super::session_tail_width(&group, 18));
    }

    #[test]
    fn narrow_and_cjk_captions_keep_whole_glyphs_root_and_exact_tail_geometry() {
        use unicode_width::UnicodeWidthStr as _;
        for host in [None, Some("遠隔e\u{301}計算機"), Some("👩‍💻開発")] {
            for status in [
                SessionStatus::Queued,
                SessionStatus::Connecting,
                SessionStatus::Online,
                SessionStatus::Offline,
                SessionStatus::Rejected,
            ] {
                let group = caption_group("開発e\u{301}👩‍💻作業", host, status);
                for width in 0..40 {
                    let caption = super::session_caption(&group, '\u{f0aa1}', width);
                    assert_eq!(caption.width(), usize::from(width), "{caption:?}");
                    if width > 0 {
                        assert!(caption.starts_with('\u{f14e2}'));
                    }
                    if width >= 2 {
                        assert!(caption.starts_with("\u{f14e2} "));
                    }
                    let tail = super::session_tail_width(&group, width);
                    if width < 5 {
                        assert_eq!(tail, 0);
                    }
                    if tail > 0 {
                        let icon = group.tail_icon('\u{f0aa1}').unwrap();
                        let start = caption.find(icon).unwrap();
                        assert_eq!(caption[..start].width(), usize::from(width - tail));
                        assert!(caption[..start].ends_with(' '));
                    }
                    for (index, ch) in caption.char_indices() {
                        if ch == '\u{301}' {
                            assert!(caption[..index].ends_with('e'));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn sidebar_item_names_ellipsis_at_unicode_boundaries_without_marking_exact_fits() {
        for (name, budget, expected) in [
            ("e\u{301}日x", 0, ""),
            ("e\u{301}日x", 1, "…"),
            ("e\u{301}日x", 2, "e\u{301}…"),
            ("e\u{301}日x", 3, "e\u{301}…"),
            ("e\u{301}日x", 4, "e\u{301}日x"),
            ("日本", 1, "…"),
            ("日本", 2, "…"),
            ("日本", 3, "日…"),
            ("日本", 4, "日本"),
            ("abc", 3, "abc"),
            ("abcd", 3, "ab…"),
            ("👩‍💻", 2, "👩‍💻"),
            ("👨‍👩‍👧‍👦", 2, "👨‍👩‍👧‍👦"),
            ("🇫🇷", 2, "🇫🇷"),
            ("1\u{fe0f}\u{20e3}", 2, "1\u{fe0f}\u{20e3}"),
            ("♥\u{fe0f}", 2, "♥\u{fe0f}"),
            ("👩‍💻x", 1, "…"),
            ("👩‍💻x", 2, "…"),
            ("👩‍💻x", 3, "👩‍💻x"),
            ("👩‍💻x", 4, "👩‍💻x"),
            ("👩‍💻x", 5, "👩‍💻x"),
            ("👩‍💻xyz", 4, "👩‍💻x…"),
        ] {
            let (mut pane, _) = capture_fixture();
            pane.harness = None;
            pane.title = name.into();
            let mut registry = capture_registry(&pane);
            registry.workspaces[0].name = name.into();
            registry.workspaces[0].windows[0].name = name.into();
            registry.workspaces[0].windows.push(window("other", None));
            for target in [
                SidebarTarget::Workspace("work".into()),
                SidebarTarget::Window {
                    workspace: "work".into(),
                    window: "work-window".into(),
                },
                SidebarTarget::Pane {
                    workspace: "work".into(),
                    pane: pane.id.clone(),
                },
            ] {
                let mut state = Controller::new(&registry);
                let mut surface = capture_surface();
                let context = FixtureChrome {
                    size: surface.frame.size,
                    state: &state,
                    registry: &registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows: 0,
                    sidebar_popup: None,
                };
                let tree = fixture_tree(&context, |_| None, |_| None, |_| None);
                let entry = tree
                    .rows
                    .iter()
                    .find(|entry| {
                        entry.target.as_ref().is_some_and(|candidate| {
                            candidate.node == crate::tree::Node::Native(target.clone())
                        })
                    })
                    .unwrap();
                let start = entry.content_column();
                let width = start + budget;
                state.sidebar.preferred_columns = Some(width);
                let size = surface.frame.size;
                let hits = paint_fixture(
                    &mut surface,
                    FixtureChrome {
                        size,
                        state: &state,
                        registry: &registry,
                        rects: &[],
                        pane_graphics: &[],
                        guide_rows: 0,
                        sidebar_popup: None,
                    },
                    |_| None,
                    |_| None,
                    |_| None,
                    &mut Vec::new(),
                );
                let row = hits.iter().find(|(hit, _)| hit == &target).unwrap().1;
                let visible = surface.frame.rows[usize::from(row)].cells
                    [usize::from(start)..usize::from(width)]
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>();
                assert_eq!(
                    visible.trim_end(),
                    expected,
                    "{target:?}, {name:?}, {budget}"
                );
            }
            let group = caption_group(name, None, SessionStatus::Online);
            let caption = super::session_caption(&group, ' ', budget + 2);
            assert_eq!(caption["\u{f14e2} ".len()..].trim_end(), expected);
        }
    }

    #[test]
    fn wrapped_details_keep_joined_emoji_and_combining_clusters_whole() {
        assert_eq!(super::wrapped("a👩‍💻e\u{301}界", 3), ["a👩‍💻", "e\u{301}界"]);
        assert_eq!(super::wrapped("👩‍💻x", 2), ["👩‍💻", "x"]);
        assert_eq!(super::wrapped("a👩‍💻e\u{301}", 1), ["a", "e\u{301}"]);
        assert!(super::wrapped("👩‍💻", 0).is_empty());
    }

    #[test]
    fn joined_emoji_title_span_ends_before_right_aligned_activity() {
        let (mut pane, snapshot) = capture_fixture();
        pane.title = "👩‍💻xy".into();
        pane.harness_status = Some(working_record(Some("claude")));
        let prefix_width = 2;
        let status_width = bkpr_tui_framework::text_width(pane_activity(&pane, Some(&snapshot)));
        for (budget, expected) in [(1, "…"), (2, "…"), (3, "👩‍💻…"), (4, "👩‍💻xy")]
        {
            let width = prefix_width + budget + status_width;
            let (label, title) = sidebar_pane_label(&pane, Some(&snapshot), width);
            let mut surface = capture_surface();
            let size = Size {
                cols: width as u16,
                rows: 1,
                ..surface.frame.size
            };
            surface.paint(
                size,
                vec![(
                    Rect {
                        x: 0,
                        y: 0,
                        width: width as u16,
                        height: 1,
                    },
                    label,
                    false,
                )],
                None,
            );
            let cells = &surface.frame.rows[0].cells;
            let text: String = cells[usize::from(title.start)..usize::from(title.end)]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            assert_eq!(text, expected);
            assert_eq!(usize::from(title.start), prefix_width);
            assert!(usize::from(title.end) <= width - status_width);
            let status: String = cells[width - status_width..]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            assert_eq!(status, pane_activity(&pane, Some(&snapshot)));
        }
    }

    #[test]
    fn sidebar_hidden_cursor_tracks_navigation_icons_and_item_not_details() {
        let (mut pane, snapshot) = capture_fixture();
        pane.project = Some(project_fixture());
        let mut registry = capture_registry(&pane);
        registry.workspaces[0]
            .windows
            .push(window("other-window", None));
        let size = Size {
            rows: 9,
            ..capture_surface().frame.size
        };
        let mut surface = UiSurface::new(size);
        let mut state = Controller::new(&registry);
        state.sidebar.enter();
        state
            .sidebar
            .select(SidebarTarget::Workspace("work".into()));
        let (sidebar, _) = screen_geometry(size, state.sidebar.preferred_columns);
        let guide_rows = 2;
        state
            .sidebar
            .viewport(sidebar_content_rows(sidebar, guide_rows));
        for (index, icon) in ["●", "\u{e0cc}", "●"].into_iter().enumerate() {
            let popup = state.sidebar.cursor.clone().unwrap();
            let hits = paint_fixture(
                &mut surface,
                FixtureChrome {
                    size,
                    state: &state,
                    registry: &registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows,
                    sidebar_popup: Some((&popup, ColorSource::Rgb([40, 50, 60]))),
                },
                |_| None,
                |_| Some(&snapshot),
                |_| None,
                &mut Vec::new(),
            );
            let row = hits.iter().find(|(target, _)| target == &popup).unwrap().1;
            let column = surface.frame.rows[usize::from(row)]
                .cells
                .iter()
                .position(|cell| cell.text == icon)
                .unwrap() as u16;
            let cursor = &surface.frame.cursor;
            assert_eq!((cursor.x, cursor.y), (column, row));
            assert!(!cursor.visible);
            if index == 2 {
                // The same target's branch-detail row is painted immediately
                // below the item, but must not become its physical anchor.
                assert!(hits.iter().any(|(target, y)| target == &popup && *y > row));
            }
            state.sidebar.navigate(1);
        }
        state.sidebar.active = false;
        paint_fixture(
            &mut surface,
            FixtureChrome {
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows,
                sidebar_popup: None,
            },
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut Vec::new(),
        );
        assert!(!surface.frame.cursor.visible);
        assert_eq!(
            (surface.frame.cursor.x, surface.frame.cursor.y),
            (size.cols, size.rows)
        );
    }

    #[test]
    fn omp_titles_strip_only_recognized_leading_harness_decoration() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.harness.as_mut().unwrap().kind = HarnessKind::Omp;
        for prefix in [
            "π: ", "π > ", "π ! ", "π : ", "π ⠋ ", "π ⠴ ", "π ◑ ", "π / ",
        ] {
            pane.title = format!("{prefix}Fix π > 日本語");
            assert_eq!(
                crate::metadata::pane_title(&pane, Some(&snapshot)),
                "Fix π > 日本語"
            );
            let (label, _) = sidebar_pane_label(&pane, Some(&snapshot), 80);
            assert!(label.contains("Fix π > 日本語"));
            assert!(!label.contains(pane.title.as_str()));
        }
        for title in ["π task", "π >task", "Task > name", "π", "Task"] {
            pane.title = title.into();
            assert_eq!(crate::metadata::pane_title(&pane, Some(&snapshot)), title);
        }
        pane.title = "π > Task".into();
        pane.harness.as_mut().unwrap().kind = HarnessKind::Claude;
        assert_eq!(
            crate::metadata::pane_title(&pane, Some(&snapshot)),
            "π > Task"
        );
        pane.harness.as_mut().unwrap().kind = HarnessKind::Omp;
        snapshot.program_identity.as_mut().unwrap().birth = "stale".into();
        assert_eq!(
            crate::metadata::pane_title(&pane, Some(&snapshot)),
            "π > Task"
        );
        assert_eq!(pane.title, "π > Task");
    }

    #[test]
    fn codex_sidebar_logo_keeps_its_spacer_and_requires_current_identity() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.harness.as_mut().unwrap().kind = HarnessKind::Codex;
        pane.title = "日本語".into();
        for width in 0..12 {
            let (label, _) = sidebar_pane_label(&pane, Some(&snapshot), width);
            assert!(unicode_width::UnicodeWidthStr::width(label.as_str()) <= width);
            assert_eq!(label.contains('\u{ec81}'), width >= 2);
            if width >= 2 {
                assert!(label.starts_with("\u{ec81} "));
            }
        }
        snapshot.program_identity.as_mut().unwrap().birth = "stale".into();
        assert!(
            !sidebar_pane_label(&pane, Some(&snapshot), 20)
                .0
                .contains('\u{ec81}')
        );
        pane.status = PaneStatus::Exited;
        assert!(!sidebar_pane_label(&pane, None, 20).0.contains('\u{ec81}'));
    }

    #[test]
    fn working_title_spans_follow_clipped_native_titles_and_current_semantics() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.title = "日本語 title with enough text to truncate well past the sidebar".into();
        pane.harness_status = Some(working_record(Some("claude")));
        let paint = |pane: &PaneInfo, snapshot: &PaneSnapshot, guide_rows: u16| {
            let registry = capture_registry(pane);
            let state = Controller::new(&registry);
            let mut surface = capture_surface();
            let context = FixtureChrome {
                size: surface.frame.size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows,
                sidebar_popup: None,
            };
            let tree = fixture_tree(&context, |_| None, |_| Some(snapshot), |_| None);
            let mut titles = Vec::new();
            tree_chrome(
                &mut surface,
                TreeChromeContext {
                    native: context.native(),
                    tree: &tree,
                    active: FIXTURE_OWNER,
                    popup: None,
                },
                |_| None,
                |_| Some(snapshot),
                |_| None,
                &mut Vec::new(),
                &mut titles,
            );
            (surface, titles)
        };
        let (surface, titles) = paint(&pane, &snapshot, 0);
        assert_eq!(titles.len(), 1, "No workspace/window/detail spans");
        let title = titles[0];
        let row = &surface.frame.rows[usize::from(title.y)].cells;
        let text = row[usize::from(title.x)..usize::from(title.x + title.width)]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(text.starts_with("日本語"));
        assert!(text.ends_with('…'), "Truncated title is visibly marked");
        assert!(
            text.len() < pane.title.len(),
            "Clipped title excludes padding/status"
        );
        assert!(!text.contains('\u{ec82}'), "Harness logo is static");
        assert!(!text.contains('\u{f0150}'), "Activity glyph is static");
        assert_eq!(row[usize::from(title.x - 1)].text, " ");
        assert!(title.x + title.width < screen_geometry(surface.frame.size, None).0.width);
        assert!(
            paint(&pane, &snapshot, surface.frame.size.rows)
                .1
                .is_empty()
        );
        for state in ["idle", "done", "blocked", "error", "unrecognized"] {
            pane.harness_status.as_mut().unwrap().state = state.into();
            assert!(paint(&pane, &snapshot, 0).1.is_empty(), "{state}");
        }
        pane.harness_status = Some(working_record(Some("codex")));
        assert!(paint(&pane, &snapshot, 0).1.is_empty(), "Wrong harness app");
        pane.harness_status = Some(working_record(Some("claude")));
        snapshot.program_identity.as_mut().unwrap().birth = "stale".into();
        assert!(paint(&pane, &snapshot, 0).1.is_empty(), "Stale identity");
        pane.status = PaneStatus::Exited;
        assert!(paint(&pane, &snapshot, 0).1.is_empty(), "Exited pane");
    }

    #[test]
    fn pane_activity_status_uses_the_full_sidebar_text_viewport() {
        let (mut pane, snapshot) = capture_fixture();
        pane.harness_status = Some(working_record(Some("claude")));
        let mut surface = capture_surface();
        let hits = capture_chrome(&mut surface, &pane, &snapshot);
        let row = hits
            .iter()
            .find_map(|(target, row)| {
                matches!(target, SidebarTarget::Pane { pane: id, .. } if id == "pane")
                    .then_some(*row)
            })
            .unwrap();
        let sidebar = screen_geometry(surface.frame.size, None).0;
        assert_eq!(
            surface.frame.rows[usize::from(row)].cells[usize::from(sidebar.width - 1)].text,
            "\u{f0150}"
        );
    }

    #[test]
    fn native_project_markers_keep_full_ink_while_branch_metadata_stays_muted() {
        let (mut pane, snapshot) = capture_fixture();
        let mut project = project_fixture();
        project.color_slot = Some(1);
        pane.project = Some(project);
        let registry = capture_registry(&pane);
        let state = Controller::new(&registry);
        let mut surface = capture_surface();
        let host = bkpr_tui_framework::ColorProbe::default();
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &host);
        let mut palette = ProjectPalette::default();
        palette.update(&host, 2);
        let size = surface.frame.size;
        let context = || FixtureChrome {
            sidebar_popup: None,
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
        };
        let mut projects = Vec::new();
        paint_fixture(
            &mut surface,
            context(),
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut projects,
        );
        style_fixture(
            &mut surface,
            &theme,
            context(),
            |_| Some(&snapshot),
            &palette,
            &projects,
            &mut Vec::new(),
        );
        let pane_y = state
            .sidebar
            .rows
            .iter()
            .position(|row| matches!(row.kind, SidebarRowKind::Pane { .. }))
            .unwrap()
            + 2;
        let marker = surface.frame.rows[pane_y]
            .cells
            .iter()
            .find(|cell| matches!(cell.text.as_str(), "●" | "◌"))
            .unwrap();
        assert_eq!(
            if marker.style.inverse {
                marker.style.background_source
            } else {
                marker.style.foreground_source
            },
            bkpr_protocol::ColorSource::Palette(5)
        );
        assert!(!marker.style.faint);
        let detail_y = state
            .sidebar
            .rows
            .iter()
            .position(|row| matches!(row.kind, SidebarRowKind::Detail { .. }))
            .unwrap()
            + 2;
        let branch = &surface.frame.rows[detail_y].cells[6];
        let ink = if branch.style.inverse {
            branch.style.background_source
        } else {
            branch.style.foreground_source
        };
        assert_eq!(ink, theme.ink(Role::Muted, true));
    }

    #[test]
    fn refreshing_sidebar_preserves_cached_labels_counts_colors_and_hit_geometry() {
        let (mut pane, snapshot) = capture_fixture();
        let mut project = project_fixture();
        project.color_slot = Some(1);
        project.repository.as_mut().unwrap().committed = Some(ChangeSummary {
            files: 2,
            insertions: 13,
            deletions: 7,
            binary_files: 0,
        });
        pane.project = Some(project);
        for columns in [24, 48, 120] {
            let render = |pane: &PaneInfo| {
                let registry = capture_registry(pane);
                let state = Controller::new(&registry);
                let mut surface = UiSurface::new(Size {
                    cols: columns,
                    ..capture_surface().frame.size
                });
                let context = FixtureChrome {
                    sidebar_popup: None,
                    size: surface.frame.size,
                    state: &state,
                    registry: &registry,
                    rects: &[],
                    pane_graphics: &[],
                    guide_rows: 0,
                };
                let mut projects = Vec::new();
                let hits = paint_fixture(
                    &mut surface,
                    context,
                    |_| None,
                    |_| Some(&snapshot),
                    |_| None,
                    &mut projects,
                );
                let host = bkpr_tui_framework::ColorProbe::default();
                let mut theme = Theme::default();
                crate::appearance::update_theme(&mut theme, &host);
                let mut palette = ProjectPalette::default();
                palette.update(&host, 2);
                style_fixture(
                    &mut surface,
                    &theme,
                    context,
                    |_| Some(&snapshot),
                    &palette,
                    &projects,
                    &mut Vec::new(),
                );
                (surface, hits)
            };
            let (ready, ready_hits) = render(&pane);
            for state in [ProjectState::Loading, ProjectState::Error] {
                let mut stale = pane.clone();
                stale.project.as_mut().unwrap().state = state;
                let (stale, hits) = render(&stale);
                assert_eq!(ready_hits, hits);
                for (ready_row, stale_row) in ready.frame.rows.iter().zip(&stale.frame.rows) {
                    for (ready_cell, stale_cell) in ready_row.cells.iter().zip(&stale_row.cells) {
                        if ready_cell.text != "\u{f062c}" {
                            assert_eq!(ready_cell.text, stale_cell.text);
                            assert_eq!(ready_cell.style, stale_cell.style);
                        }
                    }
                }
            }
        }
        for width in 0..=40 {
            let ready = sidebar_branch(&pane, width);
            let mut stale = pane.clone();
            stale.project.as_mut().unwrap().state = ProjectState::Loading;
            let loading = sidebar_branch(&stale, width);
            assert!(unicode_width::UnicodeWidthStr::width(loading.as_str()) <= usize::from(width));
            assert_eq!(
                ready.chars().skip(1).collect::<String>(),
                loading.chars().skip(1).collect::<String>()
            );
        }
    }

    #[test]
    fn focused_native_pane_keeps_inverse_highlight_when_hover_background_is_unknown() {
        let (mut pane, snapshot) = capture_fixture();
        pane.project = Some(project_fixture());
        let registry = capture_registry(&pane);
        let mut state = Controller::new(&registry);
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: pane.id.clone(),
        };
        state.sidebar.hover = Some(target.clone());
        let mut surface = capture_surface();
        let context = FixtureChrome {
            size: surface.frame.size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
            sidebar_popup: Some((&target, ColorSource::Default)),
        };
        let mut projects = Vec::new();
        let hits = paint_fixture(
            &mut surface,
            context,
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut projects,
        );
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &bkpr_tui_framework::ColorProbe::default());
        style_fixture(
            &mut surface,
            &theme,
            context,
            |_| Some(&snapshot),
            &ProjectPalette::default(),
            &projects,
            &mut Vec::new(),
        );
        let row = hits.iter().find(|(hit, _)| hit == &target).unwrap().1;
        let sidebar = screen_geometry(surface.frame.size, state.sidebar.preferred_columns).0;
        assert!(
            [row, row + 1].into_iter().all(|row| {
                surface.frame.rows[usize::from(row)].cells[1..usize::from(sidebar.width)]
                    .iter()
                    .all(|cell| cell.style.inverse)
            }),
            "a default-color hover card must preserve both rows of the native inverse highlight"
        );
    }

    #[test]
    fn focused_guides_end_at_each_ancestor_and_leave_window_labels_neutral() {
        let (mut pane, snapshot) = capture_fixture();
        pane.project = Some(project_fixture());
        let mut registry = capture_registry(&pane);
        let mut other = pane.clone();
        other.id = "other-pane".into();
        other.window_id = Some("other-window".into());
        registry.workspaces[0]
            .windows
            .push(window("other-window", Some(leaf(&other.id))));
        registry.panes.push(other);
        let mut state = Controller::new(&registry);
        state.sidebar.cursor = Some(SidebarTarget::Pane {
            workspace: "work".into(),
            pane: pane.id.clone(),
        });
        let mut surface = capture_surface();
        let host = bkpr_tui_framework::ColorProbe {
            truecolor: true,
            foreground: Some([220; 3]),
            background: Some([20; 3]),
            ..Default::default()
        };
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &host);
        let context = FixtureChrome {
            sidebar_popup: None,
            size: surface.frame.size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
        };
        let mut projects = Vec::new();
        paint_fixture(
            &mut surface,
            context,
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut projects,
        );
        style_fixture(
            &mut surface,
            &theme,
            context,
            |_| Some(&snapshot),
            &ProjectPalette::default(),
            &projects,
            &mut Vec::new(),
        );
        let row = |kind| {
            state
                .sidebar
                .rows
                .iter()
                .position(|row| row.kind == kind)
                .unwrap()
                + 2
        };
        let window_y = row(SidebarRowKind::Window {
            workspace: 0,
            index: 0,
            collapsed: false,
        });
        let other_y = row(SidebarRowKind::Window {
            workspace: 0,
            index: 1,
            collapsed: false,
        });
        let pane_y = row(SidebarRowKind::Pane {
            index: 0,
            projected: true,
        });
        let detail_y = row(SidebarRowKind::Detail { index: 0 });
        let icon_x = surface.frame.rows[window_y]
            .cells
            .iter()
            .position(|cell| cell.text == "\u{e0cc}")
            .unwrap();
        let ink = |y: usize, x: usize| surface.frame.rows[y].cells[x].style.foreground_source;
        let bright = theme.ink(Role::Base, false);
        let muted = crate::appearance::inactive_tree_guide_ink(&theme);
        for y in [pane_y, detail_y] {
            assert_eq!(
                surface.frame.rows[y].cells[2].style.background_source,
                theme.background(Role::Base, true)
            );
        }
        assert_ne!(bright, muted);
        assert_eq!(ink(window_y, icon_x - 3), bright);
        assert_eq!(ink(pane_y, icon_x - 3), muted);
        assert_eq!(ink(detail_y, icon_x - 3), muted);
        assert_eq!(ink(pane_y, icon_x), bright);
        assert_eq!(ink(other_y, icon_x - 3), muted);
        assert_eq!(ink(window_y, icon_x), bright);
        assert_eq!(ink(other_y, icon_x), bright);
    }

    #[test]
    fn sidebar_hover_composes_semantic_inks_and_selection_wins() {
        let (mut pane, snapshot) = capture_fixture();
        pane.harness.as_mut().unwrap().kind = HarnessKind::Codex;
        pane.harness_status = Some(working_record(Some("codex")));
        pane.project = Some(project_fixture());
        pane.project
            .as_mut()
            .unwrap()
            .repository
            .as_mut()
            .unwrap()
            .committed = Some(ChangeSummary {
            insertions: 10,
            deletions: 4,
            ..ChangeSummary::default()
        });
        let mut registry = capture_registry(&pane);
        let mut native = pane.clone();
        native.id = "native-active".into();
        native.harness = None;
        native.harness_status = None;
        registry.workspaces[0].windows[0].layout = Some(LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(leaf(&pane.id)),
            second: Box::new(leaf(&native.id)),
        });
        let native_id = native.id.clone();
        registry.panes.push(native);
        let mut state = Controller::new(&registry);
        state.focused = Some(native_id);
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: pane.id.clone(),
        };
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let host = bkpr_tui_framework::ColorProbe {
            truecolor: true,
            foreground: Some([220; 3]),
            background: Some([20; 3]),
            ..Default::default()
        };
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &host);
        for (active, hovered) in [(false, false), (true, true), (false, true)] {
            state.sidebar.active = active;
            state.sidebar.cursor = Some(target.clone());
            state.sidebar.hover = hovered.then(|| target.clone());
            let context = || FixtureChrome {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
            };
            paint_fixture(
                &mut surface,
                context(),
                |_| None,
                |_| Some(&snapshot),
                |_| None,
                &mut Vec::new(),
            );
            style_fixture(
                &mut surface,
                &theme,
                context(),
                |_| Some(&snapshot),
                &Default::default(),
                &[],
                &mut Vec::new(),
            );
            let pane_y = state
                .sidebar
                .rows
                .iter()
                .position(|row| matches!(row.kind, SidebarRowKind::Pane { .. }))
                .unwrap()
                + 2;
            let cells = &surface.frame.rows[pane_y].cells;
            let logo = cells.iter().find(|cell| cell.text == "\u{ec81}").unwrap();
            assert_eq!(
                logo.style.foreground_source,
                theme.ink(Role::Success, active)
            );
            let status = cells.iter().find(|cell| cell.text == "\u{f0150}").unwrap();
            assert_eq!(
                status.style.foreground_source,
                theme.ink(Role::Warning, active)
            );
            let background = if active {
                theme.background(Role::Base, true)
            } else if hovered {
                theme.background(Role::Hover, false)
            } else {
                theme.background(Role::Base, false)
            };
            let separator = usize::from(
                screen_geometry(size, state.sidebar.preferred_columns)
                    .0
                    .width,
            );
            for y in [pane_y, pane_y + 1] {
                for x in [0, separator] {
                    assert_eq!(
                        surface.frame.rows[y].cells[x].style.background_source, background,
                        "the entire two-line item must share its highlight"
                    );
                }
            }
            assert_eq!(logo.style.background_source, background);
            let detail = &surface.frame.rows[pane_y + 1].cells;
            let added = detail.iter().find(|cell| cell.text == "+").unwrap();
            let removed = detail.iter().find(|cell| cell.text == "-").unwrap();
            assert_eq!(
                added.style.foreground_source,
                theme.ink(Role::Success, active)
            );
            assert_eq!(
                removed.style.foreground_source,
                theme.ink(Role::Error, active)
            );
            assert_eq!(added.style.background_source, background);
        }
    }
    #[test]
    fn sidebar_geometry_clamps_width_without_leaving_gaps_or_starving_native_cells() {
        for cols in 0..=120 {
            let size = Size {
                cols,
                rows: 30,
                cell_width: 9,
                cell_height: 18,
            };
            for preferred in [None, Some(0), Some(3), Some(19), Some(67), Some(u16::MAX)] {
                let (sidebar, native) = screen_geometry(size, preferred);
                assert_eq!((sidebar.x, sidebar.y, native.y), (0, 0, 0));
                assert_eq!((sidebar.height, native.height), (size.rows, size.rows));
                assert_eq!(native.x, sidebar.width);
                assert_eq!(sidebar.width + native.width, cols);
                if cols < 24 {
                    assert_eq!(sidebar.width, 0);
                } else if let Some(preferred) = preferred {
                    assert_eq!(sidebar.width, preferred.clamp(3, cols - 3));
                    assert!(native.width >= 3);
                } else {
                    assert_eq!(sidebar.width, 38.min(cols / 2));
                }
            }
        }
    }

    #[test]
    fn requested_sidebar_width_survives_clamping_and_temporary_hiding() {
        let mut sidebar = Sidebar::default();
        sidebar.preferred_columns = Some(67);
        for (cols, visible_width) in [(120, 67), (50, 47), (23, 0), (0, 0), (120, 67)] {
            let size = Size {
                cols,
                rows: 30,
                cell_width: 9,
                cell_height: 18,
            };
            let (visible, _) = screen_geometry(size, sidebar.preferred_columns);
            sidebar.viewport(sidebar_content_rows(visible, 0));
            assert_eq!(visible.width, visible_width);
            assert_eq!(sidebar.preferred_columns, Some(67));
        }
    }

    #[test]
    fn pane_body_reserves_all_chrome_edges_without_losing_tiny_content() {
        let outer = Rect {
            x: 24,
            y: 2,
            width: 9,
            height: 7,
        };
        let body = content(outer);
        assert!(body.width > 0 && body.height > 0);
        for (x, y) in [
            (body.x, body.y),
            (body.x + body.width - 1, body.y + body.height - 1),
        ] {
            assert!(outer.contains(x, y));
            assert!(x > outer.x && x < outer.x + outer.width - 1);
            assert!(y > outer.y && y < outer.y + outer.height - 1);
        }
        for width in 1..=3 {
            for height in 1..=3 {
                let tiny = content(Rect {
                    width,
                    height,
                    ..outer
                });
                assert!(tiny.width > 0 && tiny.height > 0);
                assert!(tiny.x + tiny.width <= outer.x + width);
                assert!(tiny.y + tiny.height <= outer.y + height);
            }
        }
    }
    #[test]
    fn sidebar_hit_rows_are_item_owned_and_do_not_cover_the_contextual_guide() {
        let (pane, snapshot) = capture_fixture();
        let mut surface = capture_surface();
        let hits = capture_chrome(&mut surface, &pane, &snapshot);
        let sidebar = screen_geometry(surface.frame.size, None).0;
        assert!(!hits.is_empty());
        for (target, y) in hits {
            assert!(
                y >= sidebar.y + 2
                    && usize::from(y - sidebar.y - 2) < sidebar_content_rows(sidebar, 0)
            );
            assert!(
                surface.frame.rows[y as usize].cells[..usize::from(sidebar.width)]
                    .iter()
                    .any(|cell| !cell.text.trim().is_empty())
            );
            assert!(matches!(
                target,
                SidebarTarget::Workspace(_) | SidebarTarget::Pane { .. }
            ));
        }
    }

    #[test]
    fn passive_guide_reservation_bounds_items_and_hits() {
        let registry = RegistrySnapshot {
            revision: 1,
            workspaces: (0..20)
                .map(|index| Workspace {
                    id: format!("work-{index}"),
                    name: format!("Workspace {index}"),
                    revision: 1,
                    windows: Vec::new(),
                })
                .collect(),
            panes: Vec::new(),
        };
        let state = Controller::new(&registry);
        let size = Size {
            cols: 100,
            rows: 14,
            cell_width: 9,
            cell_height: 18,
        };
        let guide_rows = 6;
        let mut surface = UiSurface::new(size);
        let hits = paint_fixture(
            &mut surface,
            FixtureChrome {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows,
            },
            |_| None,
            |_| None,
            |_| None,
            &mut Vec::new(),
        );
        let sidebar = screen_geometry(size, state.sidebar.preferred_columns).0;
        let guide_y = size.rows - guide_rows;
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|(_, y)| *y < guide_y));
        for row in &surface.frame.rows[usize::from(guide_y)..] {
            assert!(
                row.cells[..usize::from(sidebar.width)]
                    .iter()
                    .all(|cell| cell.text.trim().is_empty())
            );
        }
        for reserved in 0..=size.rows + 2 {
            let rows = sidebar_content_rows(sidebar, reserved);
            assert!(rows + usize::from(reserved.min(size.rows)) <= usize::from(size.rows));
        }
    }
    fn project_fixture() -> ProjectSnapshot {
        ProjectSnapshot {
            color_slot: None,
            ci: None,
            cwd: "/repo/linked/日本語".into(),
            state: ProjectState::Ready,
            repository: Some(RepositoryInfo {
                name: "repo".into(),
                root: "/repo/linked".into(),
                common_dir: "/repo/.git".into(),
                git_dir: "/repo/.git/worktrees/linked".into(),
                branch: Some("feature/日本語".into()),
                head_sha: Some("123456789abcdef".into()),
                staged: ChangeSummary {
                    files: 2,
                    insertions: 13,
                    deletions: 7,
                    binary_files: 1,
                },
                unstaged: ChangeSummary::default(),
                untracked_files: 3,
                base_ref: None,
                base_sha: None,
                committed: None,
            }),
            error: None,
        }
    }

    #[test]
    fn narrow_project_inspector_keeps_wrapped_details_accessible() {
        let (mut pane, _) = capture_fixture();
        pane.project = Some(project_fixture());
        let registry = capture_registry(&pane);
        let size = Size {
            cols: 23,
            rows: 10,
            cell_width: 9,
            cell_height: 18,
        };
        let mut surface = UiSurface::new(size);
        let mut form = Form::new(
            FormKind::ProjectDetails,
            String::new(),
            Some(pane.id.clone()),
        );
        let details = project_detail_lines(size, &form, &registry);
        let width = size.cols;
        for (index, detail) in details.iter().enumerate() {
            assert!(unicode_width::UnicodeWidthStr::width(detail.as_str()) <= width as usize);
            form.selection = index;
            modal(&mut surface, size, &form, &registry);
            assert!(painted_text(&surface).contains(detail));
            assert!(!surface.frame.cursor.visible);
        }
    }

    use super::*;
    fn leaf(id: &str) -> LayoutNode {
        LayoutNode::Leaf { pane_id: id.into() }
    }
    fn window(id: &str, layout: Option<LayoutNode>) -> Window {
        Window {
            id: id.into(),
            name: id.into(),
            revision: 1,
            layout,
        }
    }
    #[test]
    fn startup_prefers_live_projected_execution_and_keeps_explicit_history_selection() {
        let (mut stopped, _) = capture_fixture();
        stopped.id = "stopped".into();
        stopped.status = PaneStatus::Stopped;
        stopped.workspace_id = Some("history".into());
        stopped.window_id = Some("history-window".into());
        let mut live = stopped.clone();
        live.id = "live".into();
        live.status = PaneStatus::Running;
        live.workspace_id = Some("current".into());
        live.window_id = Some("current-window".into());
        let mut registry = RegistrySnapshot {
            revision: 1,
            workspaces: vec![
                Workspace {
                    id: "history".into(),
                    name: "History".into(),
                    revision: 1,
                    windows: vec![window("history-window", Some(leaf("stopped")))],
                },
                Workspace {
                    id: "current".into(),
                    name: "Current".into(),
                    revision: 1,
                    windows: vec![window("current-window", Some(leaf("live")))],
                },
            ],
            panes: vec![stopped, live],
        };
        let mut state = Controller::new(&registry);
        assert_eq!(state.workspace.as_deref(), Some("current"));
        assert_eq!(state.focused.as_deref(), Some("live"));
        state.switch("history".into(), &registry);
        state.reconcile(&registry);
        assert_eq!(state.workspace.as_deref(), Some("history"));
        assert_eq!(state.focused.as_deref(), Some("stopped"));

        // Registry ownership alone is not a usable projection.
        registry.workspaces[1].windows[0].layout = None;
        let state = Controller::new(&registry);
        assert_eq!(state.workspace.as_deref(), Some("history"));
        assert_eq!(state.focused.as_deref(), Some("stopped"));
    }

    fn selection_registry() -> RegistrySnapshot {
        let (base, _) = capture_fixture();
        let pane = |id: &str, workspace: &str, window: &str, status| {
            let mut pane = base.clone();
            pane.id = id.into();
            pane.workspace_id = Some(workspace.into());
            pane.window_id = Some(window.into());
            pane.status = status;
            pane
        };
        RegistrySnapshot {
            revision: 1,
            workspaces: vec![
                Workspace {
                    id: "work".into(),
                    name: "Work".into(),
                    revision: 10,
                    windows: vec![
                        window(
                            "first",
                            split_leaf(Some(leaf("a")), "a", "b", Axis::Horizontal),
                        ),
                        window("history", Some(leaf("old"))),
                        window("second", Some(leaf("c"))),
                    ],
                },
                Workspace {
                    id: "other".into(),
                    name: "Other".into(),
                    revision: 1,
                    windows: vec![window("third", Some(leaf("d")))],
                },
            ],
            panes: vec![
                pane("a", "work", "first", PaneStatus::Running),
                pane("b", "work", "first", PaneStatus::Stopped),
                pane("old", "work", "history", PaneStatus::Stopped),
                pane("c", "work", "second", PaneStatus::Running),
                pane("d", "other", "third", PaneStatus::Running),
            ],
        }
    }

    #[test]
    fn clients_remember_windows_per_workspace_and_focus_per_window_independently() {
        let mut registry = selection_registry();
        let mut state = Controller::new(&registry);
        let other_client = Controller::new(&registry);
        state.focus("b".into());
        state.switch_window("second".into(), &registry);
        assert_eq!(state.focused.as_deref(), Some("c"));
        state.switch("other".into(), &registry);
        state.switch("work".into(), &registry);
        assert_eq!(state.window.as_deref(), Some("second"));
        assert_eq!(state.focused.as_deref(), Some("c"));
        state.switch_window("first".into(), &registry);
        assert_eq!(state.focused.as_deref(), Some("b"));
        registry.workspaces[0].windows.reverse();
        registry.revision += 1;
        state.reconcile(&registry);
        assert_eq!(state.window.as_deref(), Some("first"));
        assert_eq!(state.focused.as_deref(), Some("b"));
        assert_eq!(other_client.window.as_deref(), Some("first"));
        assert_eq!(other_client.focused.as_deref(), Some("a"));
        state.switch_window("third".into(), &registry);
        assert_eq!(state.window.as_deref(), Some("first"));
    }

    #[test]
    fn closure_reconciliation_keeps_valid_selection_then_prefers_a_live_window() {
        let mut registry = selection_registry();
        let mut state = Controller::new(&registry);
        state.focus("b".into());
        registry.workspaces[0].windows[0].layout = Some(leaf("a"));
        registry.panes.retain(|pane| pane.id != "b");
        registry.revision += 1;
        state.reconcile(&registry);
        assert_eq!(state.window.as_deref(), Some("first"));
        assert_eq!(state.focused.as_deref(), Some("a"));
        registry.workspaces[0].windows.remove(0);
        registry.panes.retain(|pane| pane.id != "a");
        registry.revision += 1;
        state.reconcile(&registry);
        assert_eq!(state.window.as_deref(), Some("second"));
        assert_eq!(state.focused.as_deref(), Some("c"));
        state.switch_window("history".into(), &registry);
        state.reconcile(&registry);
        assert_eq!(state.window.as_deref(), Some("history"));
        assert_eq!(state.focused.as_deref(), Some("old"));
        registry.workspaces.remove(0);
        registry
            .panes
            .retain(|pane| pane.workspace_id.as_deref() == Some("other"));
        registry.revision += 1;
        state.reconcile(&registry);
        assert_eq!(state.workspace.as_deref(), Some("other"));
        assert_eq!(state.window.as_deref(), Some("third"));
        assert_eq!(state.focused.as_deref(), Some("d"));
        registry.workspaces.clear();
        registry.panes.clear();
        registry.revision += 1;
        state.reconcile(&registry);
        assert!(state.workspace.is_none() && state.window.is_none() && state.focused.is_none());
    }

    #[test]
    fn captured_window_form_identity_survives_local_selection_and_registry_reorder() {
        let mut registry = selection_registry();
        let mut state = Controller::new(&registry);
        state.switch_window("second".into(), &registry);
        state.open(FormKind::RenameWindow, "Captured name".into());
        let target = SidebarTarget::Window {
            workspace: "work".into(),
            window: "second".into(),
        };
        state.form.as_mut().unwrap().target = Some((target.clone(), 23));
        state.switch_window("first".into(), &registry);
        registry.workspaces.reverse();
        registry.revision += 1;
        state.reconcile(&registry);
        let form = state.form.as_ref().unwrap();
        assert_eq!(form.target, Some((target, 23)));
        assert_eq!(form.fields.primary.value, "Captured name");
        assert_eq!(state.window.as_deref(), Some("first"));
        state.cancel();
        state.reconcile(&registry);
        assert_eq!(state.window.as_deref(), Some("first"));
        assert_eq!(state.focused.as_deref(), Some("a"));
    }

    #[test]
    fn exchanging_nested_panes_preserves_split_geometry_and_unrelated_projection() {
        let mut layout = LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 650,
            first: Box::new(LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 300,
                first: Box::new(leaf("a")),
                second: Box::new(leaf("b")),
            }),
            second: Box::new(leaf("c")),
        };
        let original = layout.clone();
        let area = Rect {
            x: 5,
            y: 2,
            width: 100,
            height: 40,
        };
        let mut before = Vec::new();
        layout_rects(Some(&layout), area, &mut before);
        exchange_leaves(&mut layout, "a", "c");
        let mut after = Vec::new();
        layout_rects(Some(&layout), area, &mut after);
        assert_eq!(
            after,
            vec![
                ("c".into(), before[0].1),
                ("b".into(), before[1].1),
                ("a".into(), before[2].1),
            ]
        );
        exchange_leaves(&mut layout, "c", "a");
        assert_eq!(layout, original);
    }

    #[test]
    fn remove_layout_never_describes_termination() {
        let tree = split_leaf(Some(leaf("a")), "a", "b", Axis::Horizontal);
        assert_eq!(remove_leaf(tree, "a"), Some(leaf("b")));
    }
    #[test]
    fn modal_cancel_restores_focus() {
        let registry = RegistrySnapshot {
            revision: 0,
            workspaces: Vec::new(),
            panes: Vec::new(),
        };
        let mut state = Controller::new(&registry);
        state.focused = Some("pane".into());
        state.open(FormKind::RenamePane, "old".into());
        state.focused = None;
        state.cancel();
        assert_eq!(state.focused.as_deref(), Some("pane"));
    }
    #[test]
    fn sidebar_omits_closed_records_but_keeps_faults_and_explicit_history_projections() {
        let panes = [
            ("closed", "Closed shell", PaneStatus::Exited),
            ("live", "Live shell", PaneStatus::Running),
            ("fault", "Faulted shell", PaneStatus::Stopped),
            ("history", "Retained scene", PaneStatus::Exited),
        ]
        .into_iter()
        .map(|(id, title, status)| PaneInfo {
            id: id.into(),
            workspace_id: Some("work".into()),
            window_id: Some("work-window".into()),
            title: title.into(),
            status,
            exit_code: None,
            cwd: "/".into(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: None,
            home: "/".into(),
            isolated_environment: true,
            harness_status_changed_at_ms: None,
        })
        .collect();
        let registry = RegistrySnapshot {
            revision: 1,
            workspaces: vec![Workspace {
                id: "work".into(),
                name: "Work".into(),
                revision: 1,
                windows: vec![window("work-window", Some(leaf("history")))],
            }],
            panes,
        };
        let size = Size {
            cols: 100,
            rows: 24,
            cell_width: 9,
            cell_height: 18,
        };
        let mut surface = UiSurface::new(size);
        let hit = paint_fixture(
            &mut surface,
            FixtureChrome {
                sidebar_popup: None,
                size,
                state: &Controller::new(&registry),
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
            },
            |_| None,
            |_| None,
            |_| None,
            &mut Vec::new(),
        );
        assert!(!hit.iter().any(
            |(target, _)| matches!(target, SidebarTarget::Pane { pane, .. } if pane == "closed")
        ));
        for expected in ["live", "fault", "history"] {
            assert!(hit.iter().any(
                |(target, _)| matches!(target, SidebarTarget::Pane { pane, .. } if pane == expected)
            ));
        }
    }
    fn capture_fixture() -> (PaneInfo, PaneSnapshot) {
        let pane = PaneInfo {
            id: "pane".into(),
            workspace_id: Some("work".into()),
            window_id: Some("work-window".into()),
            title: "Design notes".into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: "/initial".into(),
            foreground_cwd: Some("/current/project".into()),
            harness: Some(HarnessInfo {
                kind: HarnessKind::Claude,
                pid: 41,
                birth: "100".into(),
                cwd: "/current/project".into(),
                managed: None,
            }),
            harness_status: None,
            project: None,
            home: "/home/fixture".into(),
            isolated_environment: true,
            harness_status_changed_at_ms: None,
        };
        let snapshot = PaneSnapshot {
            pane_id: pane.id.clone(),
            pid: Some(40),
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
            program_identity: Some(ProcessIdentity {
                pid: 41,
                birth: "100".into(),
            }),
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
        (pane, snapshot)
    }

    fn capture_registry(pane: &PaneInfo) -> RegistrySnapshot {
        RegistrySnapshot {
            revision: 1,
            workspaces: vec![Workspace {
                id: "work".into(),
                name: "User workspace".into(),
                revision: 1,
                windows: vec![window("work-window", Some(leaf(&pane.id)))],
            }],
            panes: vec![pane.clone()],
        }
    }

    fn capture_surface() -> UiSurface {
        UiSurface::new(Size {
            cols: 120,
            rows: 36,
            cell_width: 9,
            cell_height: 18,
        })
    }

    fn painted_text(surface: &UiSurface) -> String {
        surface
            .frame
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn capture_chrome(
        surface: &mut UiSurface,
        pane: &PaneInfo,
        snapshot: &PaneSnapshot,
    ) -> Vec<(SidebarTarget, u16)> {
        let registry = capture_registry(pane);
        let state = Controller::new(&registry);
        let size = surface.frame.size;
        let (_, rect) = screen_geometry(size, state.sidebar.preferred_columns);
        paint_fixture(
            surface,
            FixtureChrome {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[(pane.id.clone(), rect)],
                pane_graphics: &[],
                guide_rows: 0,
            },
            |_| None,
            |_| Some(snapshot),
            |_| None,
            &mut Vec::new(),
        )
    }

    #[test]
    fn retained_chrome_matches_fresh_projection_after_layout_change() {
        let (first, _) = capture_fixture();
        let mut second = first.clone();
        second.id = "second".into();
        second.title = "Second shell".into();
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let mut registry = capture_registry(&first);
            registry.panes.push(second.clone());
            registry.workspaces[0].windows[0].layout =
                split_leaf(Some(leaf(&first.id)), &first.id, &second.id, axis);
            let state = Controller::new(&registry);
            let mut surface = capture_surface();
            let size = surface.frame.size;
            let (_, area) = screen_geometry(size, state.sidebar.preferred_columns);
            let mut rects = Vec::new();
            layout_rects(
                registry.workspaces[0].windows[0].layout.as_ref(),
                area,
                &mut rects,
            );
            paint_fixture(
                &mut surface,
                FixtureChrome {
                    sidebar_popup: None,
                    size,
                    state: &state,
                    registry: &registry,
                    rects: &rects,
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                |_| None,
                |_| None,
                |_| None,
                &mut Vec::new(),
            );
            let LayoutNode::Split { ratio, .. } =
                registry.workspaces[0].windows[0].layout.as_mut().unwrap()
            else {
                unreachable!()
            };
            *ratio = 399;
            rects.clear();
            layout_rects(
                registry.workspaces[0].windows[0].layout.as_ref(),
                area,
                &mut rects,
            );
            paint_fixture(
                &mut surface,
                FixtureChrome {
                    sidebar_popup: None,
                    size,
                    state: &state,
                    registry: &registry,
                    rects: &rects,
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                |_| None,
                |_| None,
                |_| None,
                &mut Vec::new(),
            );
            let mut fresh = capture_surface();
            paint_fixture(
                &mut fresh,
                FixtureChrome {
                    sidebar_popup: None,
                    size,
                    state: &state,
                    registry: &registry,
                    rects: &rects,
                    pane_graphics: &[],
                    guide_rows: 0,
                },
                |_| None,
                |_| None,
                |_| None,
                &mut Vec::new(),
            );
            assert_eq!(
                surface.frame, fresh.frame,
                "retained chrome left obsolete geometry"
            );
        }
    }

    #[test]
    fn sidebar_separator_uses_sidebar_background_in_rich_and_plain_modes() {
        let (pane, _) = capture_fixture();
        let registry = capture_registry(&pane);
        let state = Controller::new(&registry);
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let area = screen_geometry(size, state.sidebar.preferred_columns).1;
        let rects = [(pane.id.clone(), area)];
        let ready = [area];
        let host = bkpr_tui_framework::ColorProbe {
            truecolor: true,
            foreground: Some([220; 3]),
            background: Some([20; 3]),
            ..Default::default()
        };
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &host);
        for pane_graphics in [&ready[..], &[][..]] {
            style_fixture(
                &mut surface,
                &theme,
                FixtureChrome {
                    sidebar_popup: None,
                    size,
                    state: &state,
                    registry: &registry,
                    rects: &rects,
                    pane_graphics,
                    guide_rows: 0,
                },
                |_| None,
                &ProjectPalette::default(),
                &[],
                &mut Vec::new(),
            );
            let y = usize::from(area.y + 1);
            assert_ne!(
                theme.background(Role::Base, false),
                theme.background(Role::Panel, false)
            );
            assert_eq!(
                surface.frame.rows[y].cells[usize::from(area.x)]
                    .style
                    .background_source,
                theme.background(Role::Base, false)
            );
            assert_eq!(
                surface.frame.rows[y].cells[usize::from(area.x + 1)]
                    .style
                    .background_source,
                theme.background(Role::Panel, false)
            );
        }
    }

    #[test]
    fn shared_frames_hide_text_when_ready_and_restore_plain_captions() {
        let (first, _) = capture_fixture();
        let mut second = first.clone();
        second.id = "second".into();
        second.title = "Second 界 shell".into();
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let mut registry = capture_registry(&first);
            registry.panes.push(second.clone());
            registry.workspaces[0].windows[0].layout =
                split_leaf(Some(leaf(&first.id)), &first.id, &second.id, axis);
            let state = Controller::new(&registry);
            let mut surface = capture_surface();
            let size = surface.frame.size;
            let area = screen_geometry(size, state.sidebar.preferred_columns).1;
            let mut rects = Vec::new();
            layout_rects(
                registry.workspaces[0].windows[0].layout.as_ref(),
                area,
                &mut rects,
            );
            let ready: Vec<_> = rects.iter().map(|(_, rect)| *rect).collect();
            let paint = |surface: &mut UiSurface, pane_graphics: &[Rect]| {
                paint_fixture(
                    surface,
                    FixtureChrome {
                        sidebar_popup: None,
                        size,
                        state: &state,
                        registry: &registry,
                        rects: &rects,
                        pane_graphics,
                        guide_rows: 0,
                    },
                    |_| None,
                    |_| None,
                    |_| None,
                    &mut Vec::new(),
                );
            };
            paint(&mut surface, &[]);
            let plain = surface.frame.clone();
            for (pane, rect) in registry.panes.iter().zip(&ready) {
                let title: String = surface.frame.rows[usize::from(rect.y)].cells
                    [usize::from(rect.x)..usize::from(rect.x + rect.width)]
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect();
                assert!(title.contains(&pane.title));
            }
            paint(&mut surface, &ready);
            for rect in &ready {
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        if x == rect.x
                            || x == rect.x + rect.width - 1
                            || y == rect.y
                            || y == rect.y + rect.height - 1
                        {
                            assert_eq!(surface.frame.rows[y as usize].cells[x as usize].text, " ");
                        }
                    }
                }
            }
            let rich = surface.frame.clone();
            paint(&mut surface, &ready);
            assert_eq!(surface.frame, rich);
            paint(&mut surface, &[]);
            assert_eq!(
                surface.frame, plain,
                "capability loss must restore the complete source captions"
            );
        }
    }

    #[test]
    fn presentation_separates_recognition_managed_binding_and_capture_failure() {
        let (mut pane, snapshot) = capture_fixture();
        let mut surface = capture_surface();
        capture_chrome(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("Design notes") && text.contains("User workspace"));
        assert!(!text.contains("working") && !text.contains("[live]"));

        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
            first_prompt_submitted_at_ms: None,
            launch_id: "launch-1".into(),
            conversation: None,
            error: None,
        });
        pane.harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .conversation = Some(NativeConversation {
            id: "native-conversation-EXACT".into(),
            source: ConversationSource::ClaudeHook,
            generation: 1,
        });
        capture_chrome(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("Design notes") && !text.contains("working"));

        pane.harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .error = Some("native hook rejected capture".into());
        capture_chrome(&mut surface, &pane, &snapshot);
        assert!(!painted_text(&surface).contains("working"));
    }

    fn working_record(app: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state: "working".into(),
            kind: None,
            app: app.map(str::to_owned),
            message: None,
            id: String::new(),
            title: "Not a pane name".into(),
            progress: Some(50),
        }
    }

    #[test]
    fn only_current_recognized_harness_osc_records_supply_semantic_status() {
        let (mut pane, snapshot) = capture_fixture();
        let mut surface = capture_surface();
        capture_chrome(&mut surface, &pane, &snapshot);
        let without_status = surface.frame.clone();
        for app in [Some("claude-code"), None] {
            pane.harness_status = Some(working_record(app));
            capture_chrome(&mut surface, &pane, &snapshot);
            assert_ne!(surface.frame, without_status);
        }
        for app in [Some("nvim"), Some("omp")] {
            pane.harness_status = Some(working_record(app));
            capture_chrome(&mut surface, &pane, &snapshot);
            assert_eq!(surface.frame, without_status);
        }
        pane.harness_status = Some(working_record(None));
        pane.harness_status.as_mut().unwrap().id = "nested-program".into();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert_eq!(surface.frame, without_status);

        pane.harness = None;
        pane.title = "Claude named ordinary shell".into();
        pane.harness_status = Some(working_record(Some("claude")));
        capture_chrome(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("Claude named ordinary shell"));
        assert!(!text.contains("working") && !text.contains("recognized"));
        assert!(!text.contains("Not a pane name"));
    }

    #[test]
    fn hidden_harness_status_updates_without_attaching_a_terminal_view() {
        let (mut pane, _) = capture_fixture();
        pane.harness_status = Some(working_record(Some("claude")));
        let mut registry = capture_registry(&pane);
        registry.workspaces[0].windows[0].layout = None;
        let state = Controller::new(&registry);
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let hits = paint_fixture(
            &mut surface,
            FixtureChrome {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
            },
            |_| None,
            |_| None,
            |_| None,
            &mut Vec::new(),
        );
        assert!(hits.iter().any(
            |(target, _)| matches!(target, SidebarTarget::Pane { pane, .. } if pane == "pane")
        ));
        let working_frame = surface.frame.clone();

        registry.panes[0].harness_status = None;
        paint_fixture(
            &mut surface,
            FixtureChrome {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
            },
            |_| None,
            |_| None,
            |_| None,
            &mut Vec::new(),
        );
        assert_ne!(surface.frame, working_frame);
    }

    #[test]
    fn replacement_and_completed_execution_clear_previous_status_and_conversation() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
            first_prompt_submitted_at_ms: None,
            launch_id: "launch-1".into(),
            conversation: Some(NativeConversation {
                id: "previous-native-conversation".into(),
                source: ConversationSource::ClaudeHook,
                generation: 1,
            }),
            error: None,
        });
        pane.harness_status = Some(working_record(Some("claude")));
        let mut surface = capture_surface();

        let replacement = pane.harness.as_mut().unwrap();
        replacement.pid = 42; // Same birth tick is not the same process.
        replacement.managed = Some(ManagedHarness {
            first_prompt_submitted_at_ms: None,
            launch_id: "launch-2".into(),
            conversation: None,
            error: None,
        });

        snapshot.program_identity = Some(ProcessIdentity {
            pid: 42,
            birth: "100".into(),
        });
        capture_chrome(&mut surface, &pane, &snapshot);
        let working_frame = surface.frame.clone();
        pane.harness.as_mut().unwrap().birth = "200".into();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert_ne!(surface.frame, working_frame);

        for lifecycle in ["exited", "terminated"] {
            snapshot.lifecycle = lifecycle.into();
            assert!(recognized_harness(&pane, Some(&snapshot)).is_none());
        }
        snapshot.lifecycle = "running".into();
        pane.harness = None;
        assert!(recognized_harness(&pane, Some(&snapshot)).is_none());
    }

    #[test]
    fn metadata_popover_retires_harness_on_snapshot_identity_change_without_registry_change() {
        let (pane, mut snapshot) = capture_fixture();
        let registry = capture_registry(&pane);
        let size = capture_surface().frame.size;
        let state = Controller::new(&registry);
        let context = FixtureChrome {
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
            sidebar_popup: None,
        };
        for target in [
            SidebarTarget::Pane {
                workspace: "work".into(),
                pane: pane.id.clone(),
            },
            SidebarTarget::Workspace("work".into()),
            SidebarTarget::Window {
                workspace: "work".into(),
                window: "work-window".into(),
            },
        ] {
            let target = crate::tree::Target {
                owner: FIXTURE_OWNER,
                node: crate::tree::Node::Native(target),
            };
            snapshot.program_identity = Some(ProcessIdentity {
                pid: 41,
                birth: "100".into(),
            });
            let mut popover = crate::popover::Popover::new(size);
            let tree = fixture_tree(&context, |_| None, |_| Some(&snapshot), |_| None);
            popover
                .paint_tree(&tree, Some(&target), 2, 24, size)
                .unwrap();
            let recognized = painted_text(&popover.surface);
            assert!(
                recognized.contains('\u{ec82}'),
                "recognized harness artwork must be presented"
            );
            assert!(recognized.contains(&pane.title));
            snapshot.program_identity = None;
            let tree = fixture_tree(&context, |_| None, |_| Some(&snapshot), |_| None);
            popover
                .paint_tree(&tree, Some(&target), 2, 24, size)
                .unwrap();
            let retired = painted_text(&popover.surface);
            assert!(
                !retired.contains('\u{ec82}'),
                "retired identity cannot retain harness artwork"
            );
            assert!(
                retired.contains(&pane.title),
                "terminal identity must remain present"
            );
            assert_ne!(recognized, retired);
        }
    }
    #[test]
    fn finder_reindexes_harness_facts_on_held_identity_change_without_registry_change() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.harness_status = Some(working_record(Some("claude")));
        let registry = capture_registry(&pane);
        let mut finder = crate::finder::Finder::new(&registry, None, |_| Some(&snapshot));
        finder.paste("harness:claude status:working");
        assert_eq!(finder.selected_target().unwrap().pane_id, pane.id);
        snapshot.program_identity = None;
        finder.update_registry(&registry, |_| Some(&snapshot));
        assert!(finder.selected_target().is_none());
        snapshot.program_identity = Some(ProcessIdentity {
            pid: 41,
            birth: "100".into(),
        });
        finder.update_registry(&registry, |_| Some(&snapshot));
        assert_eq!(finder.selected_target().unwrap().pane_id, pane.id);
    }

    #[test]
    fn newer_pane_identity_cannot_keep_an_old_registry_conversation_visible() {
        let (pane, mut snapshot) = capture_fixture();
        let mut pane = pane;
        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
            first_prompt_submitted_at_ms: None,
            launch_id: "previous-launch".into(),
            conversation: Some(NativeConversation {
                id: "previous-conversation".into(),
                source: ConversationSource::ClaudeHook,
                generation: 1,
            }),
            error: None,
        });
        pane.harness_status = Some(working_record(Some("claude")));
        let mut surface = capture_surface();

        // Pane-stream-first return to shell, replacement at the same birth
        // tick, and PID reuse must all retire the old registry association.
        for identity in [
            None,
            Some(ProcessIdentity {
                pid: 42,
                birth: "100".into(),
            }),
            Some(ProcessIdentity {
                pid: 41,
                birth: "200".into(),
            }),
        ] {
            snapshot.program_identity = identity;
            capture_chrome(&mut surface, &pane, &snapshot);
            let text = painted_text(&surface);
            assert!(!text.contains("Claude") && !text.contains("[claude"));
            assert!(!text.contains("bound") && !text.contains("working"));
            assert!(text.contains("Design notes") && text.contains("User workspace"));
        }
    }

    #[test]
    fn narrow_capture_chrome_keeps_geometry_hit_targets_and_passive_cursor() {
        let (mut pane, snapshot) = capture_fixture();
        pane.title = "界界界 user title".into();
        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
            first_prompt_submitted_at_ms: None,
            launch_id: "launch".into(),
            conversation: Some(NativeConversation {
                id: "native-conversation-that-wraps-without-overflow".into(),
                source: ConversationSource::ClaudeHook,
                generation: 1,
            }),
            error: None,
        });
        let size = Size {
            cols: 33,
            rows: 12,
            cell_width: 9,
            cell_height: 18,
        };
        let mut surface = UiSurface::new(size);
        let hits = capture_chrome(&mut surface, &pane, &snapshot);
        assert!(hits.iter().any(
            |(target, _)| matches!(target, SidebarTarget::Pane { pane, .. } if pane == "pane")
        ));
        assert!(!surface.frame.cursor.visible);
        let unchanged = surface.frame.clone();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert_eq!(
            serde_json::to_value(&surface.frame).unwrap(),
            serde_json::to_value(unchanged).unwrap()
        );
        pane.harness_status = Some(working_record(Some("claude")));
        let next_hits = capture_chrome(&mut surface, &pane, &snapshot);
        assert_eq!(
            hits.iter().map(|(_, y)| y).collect::<Vec<_>>(),
            next_hits.iter().map(|(_, y)| y).collect::<Vec<_>>()
        );
    }
    #[test]
    fn popup_highlight_clears_guides_and_shares_one_background_then_restores() {
        let (mut pane, snapshot) = capture_fixture();
        pane.project = Some(project_fixture());
        let registry = capture_registry(&pane);
        let mut state = Controller::new(&registry);
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: pane.id.clone(),
        };
        state.sidebar.cursor = Some(target.clone());
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let host = bkpr_tui_framework::ColorProbe {
            truecolor: true,
            foreground: Some([220; 3]),
            background: Some([20; 3]),
            ..Default::default()
        };
        let mut theme = Theme::default();
        let mut raised = Theme::default();
        crate::appearance::update_theme(&mut theme, &host);
        crate::appearance::update_region_theme(&mut raised, &host, None);
        let raised_bg = raised.background(Role::Panel, false);
        assert_ne!(raised_bg, theme.background(Role::Base, false));
        for (visible, keyboard) in [(true, false), (true, true), (false, false)] {
            state.sidebar.active = keyboard;
            state.sidebar.hover = (visible && !keyboard).then(|| target.clone());
            let context = || FixtureChrome {
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
                sidebar_popup: visible.then_some((&target, raised_bg)),
            };
            let mut projects = Vec::new();
            paint_fixture(
                &mut surface,
                context(),
                |_| None,
                |_| Some(&snapshot),
                |_| None,
                &mut projects,
            );
            style_fixture(
                &mut surface,
                &theme,
                context(),
                |_| Some(&snapshot),
                &ProjectPalette::default(),
                &projects,
                &mut Vec::new(),
            );
            let sidebar = screen_geometry(size, state.sidebar.preferred_columns).0;
            for (index, entry) in state.sidebar.rows.iter().enumerate() {
                if entry.target.as_ref() != Some(&target) {
                    continue;
                }
                let cells = &surface.frame.rows[index + 2].cells;
                let expected = if visible {
                    raised_bg
                } else {
                    theme.background(Role::Base, true)
                };
                assert!(
                    cells[1..usize::from(sidebar.width)]
                        .iter()
                        .all(|cell| cell.style.background_source == expected)
                );
                if visible {
                    assert!(cells[1..4].iter().all(|cell| cell.text == " "));
                } else if matches!(entry.kind, SidebarRowKind::Pane { .. }) {
                    assert!(
                        cells[1..4]
                            .iter()
                            .any(|cell| matches!(cell.text.as_str(), "╰" | "├"))
                    );
                }
            }
            assert_eq!(
                surface.frame.rows[0].cells[1].style.background_source,
                theme.background(Role::Base, false)
            );
        }
    }
    #[test]
    fn popup_keeps_native_keyboard_selection_when_rgb_is_unavailable() {
        let (pane, snapshot) = capture_fixture();
        let registry = capture_registry(&pane);
        let mut state = Controller::new(&registry);
        state.sidebar.active = true;
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: pane.id.clone(),
        };
        state.sidebar.cursor = Some(target.clone());
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let mut theme = Theme::default();
        crate::appearance::update_theme(&mut theme, &Default::default());
        let context = || FixtureChrome {
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
            sidebar_popup: Some((&target, ColorSource::Default)),
        };
        paint_fixture(
            &mut surface,
            context(),
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut Vec::new(),
        );
        style_fixture(
            &mut surface,
            &theme,
            context(),
            |_| Some(&snapshot),
            &ProjectPalette::default(),
            &[],
            &mut Vec::new(),
        );
        let row = state
            .sidebar
            .rows
            .iter()
            .position(|entry| matches!(entry.kind, SidebarRowKind::Pane { .. }))
            .unwrap()
            + 2;
        assert!(
            surface.frame.rows[row]
                .cells
                .iter()
                .any(|cell| cell.text != " " && cell.style.inverse)
        );
        assert!(
            surface.frame.rows[row].cells[1..4]
                .iter()
                .all(|cell| cell.text == " ")
        );
    }
}
