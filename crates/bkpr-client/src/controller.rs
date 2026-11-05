use crate::tiling;
use crate::{metadata::ProjectSpan, project_colors::ProjectPalette};
use bkpr_protocol::*;
use bkpr_tui_framework::{BackgroundSpan, ForegroundSpan, Rect, Role, RoleSpan, Theme, UiSurface};
use std::collections::HashMap;
#[path = "sidebar.rs"]
mod sidebar;
pub use sidebar::{RowKind as SidebarRowKind, Sidebar, SidebarTarget};

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
    ChooseWorkspace,
    ChooseWindow,
    ChoosePane,
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
            _ => Context::Chooser,
        }
    }
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
        };
        state.reconcile(registry);
        state
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

fn harness_label(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>) -> Option<String> {
    let harness = recognized_harness(pane, snapshot)?;
    let binding = match &harness.managed {
        None => "recognized",
        Some(managed) if managed.error.is_some() => "capture error",
        Some(managed) if managed.conversation.is_some() => "bound",
        Some(_) => "binding",
    };
    let mut label = format!("{} / {binding}", harness.kind.display_name());
    if let Some(status) = harness_status(pane, snapshot) {
        label.push_str(" / ");
        label.push_str(&status.state);
    }
    Some(label)
}
fn execution_label(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>) -> &'static str {
    match pane.status {
        PaneStatus::Running => "live",
        PaneStatus::Stopped => "stopped / unavailable",
        PaneStatus::Exited if snapshot.is_some_and(|snapshot| snapshot.error.is_none()) => {
            "exited / retained scene"
        }
        PaneStatus::Exited => "exited / scene not loaded",
    }
}

pub(crate) fn harness_icon(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Omp => "\u{f03ff}",
        HarnessKind::Claude => "\u{ec82}",
        HarnessKind::Codex => "\u{ec81}",
        _ => "\u{f120}",
    }
}

pub(crate) fn harness_role(kind: HarnessKind) -> Role {
    match kind {
        HarnessKind::Omp => Role::Secondary,
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

fn sidebar_pane_label(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>, width: usize) -> String {
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
    let name = bkpr_tui_framework::window::fit(&pane.title, width.saturating_sub(reserved) as u16);
    let padding =
        width.saturating_sub(reserved + unicode_width::UnicodeWidthStr::width(name.as_str()));
    format!("{prefix}{name}{}{status}", " ".repeat(padding))
}

fn pane_tree(state: &Controller, registry: &RegistrySnapshot, index: usize) -> (bool, bool) {
    let pane = &registry.panes[index];
    let grouped = registry.workspaces.iter().any(|workspace| {
        Some(&workspace.id) == pane.workspace_id.as_ref() && workspace.windows.len() > 1
    });
    let last = state
        .sidebar
        .rows
        .iter()
        .rev()
        .find_map(|row| match row.kind {
            SidebarRowKind::Pane { index, .. }
                if registry.panes[index].window_id == pane.window_id =>
            {
                Some(index)
            }
            _ => None,
        })
        == Some(index);
    (grouped, last)
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
    let branch = repository.branch.as_deref().unwrap_or("detached");
    // Only observed committed totals correspond to main's branch-versus-base
    // counts. Missing comparison data is not a fabricated zero.
    let counts = repository
        .committed
        .map(|changes| format!(" +{} -{}", changes.insertions, changes.deletions))
        .unwrap_or_default();
    let stale = if project.state == ProjectState::Ready {
        ""
    } else {
        " · stale"
    };
    let reserved = unicode_width::UnicodeWidthStr::width(counts.as_str())
        + unicode_width::UnicodeWidthStr::width(stale);
    let counts = if reserved + 5 <= usize::from(width) {
        counts.as_str()
    } else {
        ""
    };
    let available = width.saturating_sub(
        (unicode_width::UnicodeWidthStr::width(counts)
            + unicode_width::UnicodeWidthStr::width(stale)
            + 2) as u16,
    );
    let branch = bkpr_tui_framework::window::fit(branch, available);
    format!("\u{f062c} {branch}{counts}{stale}")
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

fn pane_details(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(harness) = recognized_harness(pane, snapshot) {
        lines.push(format!(
            "Harness: {} (recognized)",
            harness.kind.display_name()
        ));
        if let Some(managed) = &harness.managed {
            lines.push(format!("Managed launch: {}", managed.launch_id));
            lines.push(match &managed.conversation {
                Some(conversation) => format!("Conversation: {}", conversation.id),
                None => "Conversation: awaiting binding".into(),
            });
            if let Some(error) = &managed.error {
                lines.push(format!("Capture error: {error}"));
            }
        } else {
            lines.push("Capture: unmanaged".into());
        }
        if let Some(status) = harness_status(pane, snapshot) {
            let mut label = format!("OSC 7501: {}", status.state);
            if let Some(kind) = &status.kind {
                label.push_str(" / ");
                label.push_str(kind);
            }
            if let Some(message) = &status.message {
                label.push_str(" / ");
                label.push_str(message);
            }
            lines.push(label);
        }
    }
    if let Some(cwd) = &pane.foreground_cwd {
        lines.push(format!("Foreground CWD: {}", cwd.display()));
    }
    lines
}

fn wrapped(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for character in value.chars() {
        let columns = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + columns > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if columns <= width {
            line.push(character);
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
pub(crate) fn sidebar_popup_target(state: &Controller, blocked: bool) -> Option<&SidebarTarget> {
    if blocked || state.form.is_some() || state.prefix {
        None
    } else {
        state.sidebar.hover.as_ref().or_else(|| {
            state
                .sidebar
                .active
                .then_some(state.sidebar.cursor.as_ref())
                .flatten()
        })
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
    /// The visible passive popup and its shared item/card surface color.
    pub sidebar_popup: Option<(&'a SidebarTarget, ColorSource)>,
}

/// Keep chrome text, hit targets, artwork and keyboard paging in one viewport.
pub(crate) fn sidebar_content_rows(sidebar: Rect, guide_rows: u16) -> usize {
    if sidebar.width == 0 {
        return 0;
    }
    usize::from(sidebar.height.saturating_sub(guide_rows).saturating_sub(2))
}
pub fn chrome<'a>(
    surface: &mut UiSurface,
    context: ChromeContext<'_>,
    pane_status: impl Fn(&str) -> Option<&'static str>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    history: impl Fn(&str) -> Option<(u64, u64)>,
    project_spans: &mut Vec<ProjectSpan>,
) -> Vec<(SidebarTarget, u16)> {
    let ChromeContext {
        size,
        state,
        registry,
        rects,
        pane_graphics,
        guide_rows,
        sidebar_popup,
    } = context;
    project_spans.clear();
    let (sidebar, _) = screen_geometry(size, state.sidebar.preferred_columns);
    let mut hit = Vec::new();
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
            "Workspaces",
            false,
        ));
    }
    let available = sidebar_content_rows(sidebar, guide_rows);
    for (index, entry) in state
        .sidebar
        .rows
        .iter()
        .skip(state.sidebar.scroll)
        .take(available)
        .enumerate()
    {
        let row = index as u16 + 2;
        let highlighted =
            sidebar_popup.is_some_and(|(target, _)| entry.target.as_ref() == Some(target));
        // A confirmed RGB plate owns its selection background. Limited-color
        // clients retain native inverse selection instead of losing the cue.
        let plate = highlighted
            && sidebar_popup
                .is_some_and(|(_, background)| matches!(background, ColorSource::Rgb(_)));
        let selected = !plate
            && state.sidebar.active
            && entry.target.is_some()
            && entry.target == state.sidebar.cursor;
        let mut marker_x = 1;
        let label = match entry.kind {
            SidebarRowKind::Workspace { index, collapsed } => {
                let workspace = &registry.workspaces[index];
                format!(
                    "● {}{}",
                    workspace.name,
                    if collapsed { " ···" } else { "" }
                )
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
                let mut label = String::from(if highlighted { "  " } else { "│ " });
                let mut column = 3usize;
                for (position, (_, name, slot)) in repositories.into_iter().enumerate() {
                    if position > 0 {
                        label.push_str(" · ");
                        column += 3;
                    }
                    let columns = unicode_width::UnicodeWidthStr::width(name);
                    if column < usize::from(sidebar.width) && columns > 0 {
                        project_spans.push(ProjectSpan {
                            rect: Rect {
                                x: column as u16,
                                y: row,
                                width: columns.min(usize::from(sidebar.width) - column) as u16,
                                height: 1,
                            },
                            slot,
                        });
                    }
                    label.push_str(name);
                    column += columns;
                }
                label
            }
            SidebarRowKind::Window {
                workspace,
                index,
                collapsed,
            } => {
                marker_x = 3;
                let window = &registry.workspaces[workspace].windows[index];
                format!(
                    "  {} {} {}",
                    if collapsed { "▸" } else { "▾" },
                    if state.window.as_ref() == Some(&window.id) {
                        "▪"
                    } else {
                        "▫"
                    },
                    window.name
                )
            }
            SidebarRowKind::Pane { index, projected } => {
                let pane = &registry.panes[index];
                let (grouped, last) = pane_tree(state, registry, index);
                let lead = if highlighted {
                    "   "
                } else if last {
                    "╰─╴"
                } else {
                    "├─╴"
                };
                let indent = if grouped { "  " } else { "" };
                marker_x = if grouped { 6 } else { 4 };
                project_spans.push(ProjectSpan {
                    rect: Rect {
                        x: marker_x,
                        y: row,
                        width: 2.min(sidebar.width.saturating_sub(marker_x)),
                        height: 1,
                    },
                    slot: pane.project.as_ref().and_then(|project| project.color_slot),
                });
                format!(
                    "{indent}{lead}{} {}",
                    if projected { "●" } else { "◌" },
                    sidebar_pane_label(
                        pane,
                        snapshot(&pane.id),
                        sidebar.width.saturating_sub(if grouped { 8 } else { 6 }) as usize
                    )
                )
            }
            SidebarRowKind::Detail { index } => {
                let (grouped, last) = pane_tree(state, registry, index);
                let lead = if highlighted || last {
                    "     "
                } else {
                    "│    "
                };
                let indent = if grouped { "  " } else { "" };
                let pane = &registry.panes[index];
                let branch = sidebar_branch(
                    pane,
                    sidebar.width.saturating_sub(if grouped { 8 } else { 6 }),
                );
                format!("{indent}{lead}{branch}")
            }
            SidebarRowKind::Spacer => String::new(),
        };
        if state.sidebar.active
            && entry.target.is_some()
            && entry.target == state.sidebar.cursor
            && marker_x < sidebar.width
            && matches!(
                entry.kind,
                SidebarRowKind::Workspace { .. }
                    | SidebarRowKind::Window { .. }
                    | SidebarRowKind::Pane { .. }
            )
        {
            cursor.x = marker_x;
            cursor.y = row;
        }
        lines.push(line(
            1,
            row,
            sidebar.width.saturating_sub(1),
            label,
            selected,
        ));
        if let Some(target) = &entry.target {
            hit.push((target.clone(), row));
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
                    pane.title,
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
    hit
}

/// Product hierarchy only: the framework resolves every role against the host.
/// These spans cover our chrome frame, never a native pane's application scene.
pub(crate) fn theme_chrome<'a>(
    surface: &mut UiSurface,
    theme: &Theme,
    context: ChromeContext<'_>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    palette: &ProjectPalette,
    projects: &[ProjectSpan],
    foregrounds: &mut Vec<ForegroundSpan>,
) {
    let ChromeContext {
        state,
        registry,
        rects,
        pane_graphics,
        guide_rows,
        sidebar_popup,
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
    let sidebar = screen_geometry(size, state.sidebar.preferred_columns).0;
    let mut spans = Vec::new();
    let mut backgrounds = Vec::new();
    foregrounds.clear();
    let muted = theme.ink(Role::Muted, false);
    // Main inherits bright tree guides throughout the selected pane's workspace,
    // including when the sidebar itself does not own keyboard focus.
    let selected_group = match state.sidebar.cursor.as_ref() {
        Some(SidebarTarget::Pane { workspace, .. }) => Some(workspace.as_str()),
        _ => None,
    };
    spans.push(RoleSpan {
        rect: Rect {
            height: 1,
            ..sidebar
        },
        role: Role::Title,
    });
    for (index, entry) in state
        .sidebar
        .rows
        .iter()
        .skip(state.sidebar.scroll)
        .take(sidebar_content_rows(sidebar, guide_rows))
        .enumerate()
    {
        let role = match entry.kind {
            SidebarRowKind::Workspace { .. } => Role::Title,
            SidebarRowKind::Window {
                workspace, index, ..
            } => {
                if state.window.as_ref() == Some(&registry.workspaces[workspace].windows[index].id)
                {
                    Role::Accent
                } else {
                    Role::Base
                }
            }
            SidebarRowKind::Pane { index, .. } => {
                if state.focused.as_ref() == Some(&registry.panes[index].id)
                    && entry.target.as_ref().is_some_and(|target| {
                        Some(target.workspace()) == state.workspace.as_deref()
                    })
                {
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
                y: index as u16 + 2,
                width: sidebar.width.saturating_sub(1),
                height: 1,
            },
            role,
        });
        let row_rect = Rect {
            x: 1,
            y: index as u16 + 2,
            width: sidebar.width.saturating_sub(1),
            height: 1,
        };
        let background = sidebar_popup
            .filter(|(target, _)| entry.target.as_ref() == Some(*target))
            .map(|(_, background)| background)
            .or_else(|| {
                (entry.target.is_some() && entry.target == state.sidebar.hover)
                    .then(|| theme.background(Role::Hover, false))
            });
        if let Some(background) = background {
            backgrounds.push(BackgroundSpan {
                rect: row_rect,
                background,
            });
        }
        let selected =
            state.sidebar.active && entry.target.is_some() && entry.target == state.sidebar.cursor;
        let inherited = entry
            .target
            .as_ref()
            .is_some_and(|target| selected_group == Some(target.workspace()));
        let (guide_x, guide_width) = match entry.kind {
            SidebarRowKind::Projects { .. } => (1, 2),
            SidebarRowKind::Pane { index, .. } | SidebarRowKind::Detail { index } => (
                if pane_tree(state, registry, index).0 {
                    3
                } else {
                    1
                },
                3,
            ),
            _ => (1, 0),
        };
        if guide_width > 0 {
            spans.push(RoleSpan {
                rect: Rect {
                    x: guide_x,
                    width: guide_width.min(sidebar.width.saturating_sub(guide_x)),
                    ..row_rect
                },
                role: if selected || inherited {
                    Role::Base
                } else {
                    Role::Muted
                },
            });
        }
        if let SidebarRowKind::Detail { index: pane_index } = entry.kind
            && let Some(project) = &registry.panes[pane_index].project
            && project.state == ProjectState::Ready
            && let Some(changes) = project
                .repository
                .as_ref()
                .and_then(|repository| repository.committed)
        {
            let added = changes.insertions.checked_ilog10().unwrap_or(0) as usize + 1;
            let removed = changes.deletions.checked_ilog10().unwrap_or(0) as usize + 1;
            let available =
                sidebar
                    .width
                    .saturating_sub(if pane_tree(state, registry, pane_index).0 {
                        8
                    } else {
                        6
                    }) as usize;
            if added + removed + 9 <= available {
                let y = index as u16 + 2;
                let cells = &surface.frame.rows[usize::from(y)].cells[..usize::from(sidebar.width)];
                if let Some(end) = cells.iter().rposition(|cell| cell.text != " ")
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
        } = entry.kind
        {
            let pane = &registry.panes[pane_index];
            let known = snapshot(&pane.id);
            if let Some(harness) = recognized_harness(pane, known) {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: if pane_tree(state, registry, pane_index).0 {
                            8
                        } else {
                            6
                        },
                        y: index as u16 + 2,
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
                        y: index as u16 + 2,
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

pub fn modal<'a>(
    surface: &mut UiSurface,
    size: Size,
    form: &Form,
    registry: &RegistrySnapshot,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
) {
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
        FormKind::ChooseWorkspace => "Switch workspace (arrows / Enter)",
        FormKind::ChooseWindow => "Switch window (arrows / Enter)",
        FormKind::ChoosePane => "Switch to terminal's owning window",
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
                "{} Client current directory: {}",
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
    } else if matches!(
        form.kind,
        FormKind::ChooseWorkspace | FormKind::ChooseWindow | FormKind::ChoosePane
    ) {
        let labels = if form.kind == FormKind::ChooseWorkspace {
            registry
                .workspaces
                .iter()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>()
        } else if form.kind == FormKind::ChooseWindow {
            form.target
                .as_ref()
                .and_then(|(target, _)| {
                    registry
                        .workspaces
                        .iter()
                        .find(|w| w.id == target.workspace())
                })
                .map(|workspace| {
                    workspace
                        .windows
                        .iter()
                        .map(|window| window.name.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            registry
                .panes
                .iter()
                .map(|p| {
                    format!(
                        "{} [{}] {}{}{}",
                        p.title,
                        execution_label(p, snapshot(&p.id)),
                        p.workspace_id
                            .as_ref()
                            .and_then(|id| registry.workspaces.iter().find(|w| w.id == *id))
                            .map(|w| w.name.as_str())
                            .unwrap_or("unassigned"),
                        p.workspace_id
                            .as_ref()
                            .and_then(|id| registry.workspaces.iter().find(|w| w.id == *id))
                            .and_then(|w| w
                                .windows
                                .iter()
                                .find(|window| Some(&window.id) == p.window_id.as_ref()))
                            .map(|window| format!(" / {}", window.name))
                            .unwrap_or_default(),
                        harness_label(p, snapshot(&p.id))
                            .map(|label| format!(" | {label}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut list_row = y.saturating_add(2);
        let end_row = size.rows.saturating_sub(3);
        if form.kind == FormKind::ChoosePane
            && let Some(pane) = registry.panes.get(form.selection)
        {
            // Keep at least one selectable row; details never take focus or
            // acquire a cursor and use the same clipping/surface as the list.
            let detail_limit = end_row.saturating_sub(list_row + 1) / 2;
            let details = pane_details(pane, snapshot(&pane.id));
            let details = details
                .iter()
                .flat_map(|detail| wrapped(detail, width as usize))
                .collect::<Vec<_>>();
            for detail in details.iter().take(detail_limit as usize) {
                lines.push(line(x, list_row, width, detail, false));
                list_row += 1;
            }
            if details.len() > detail_limit as usize && detail_limit > 0 {
                lines.pop();
                lines.push(line(
                    x,
                    list_row - 1,
                    width,
                    "… details clipped; enlarge terminal",
                    false,
                ));
            }
            if detail_limit > 0 && !details.is_empty() && list_row.saturating_add(1) < end_row {
                list_row += 1;
            }
        }
        let available = end_row.saturating_sub(list_row) as usize;
        let offset = form
            .selection
            .saturating_add(1)
            .saturating_sub(available.max(1));
        for (index, label) in labels.iter().enumerate().skip(offset).take(available) {
            lines.push(line(
                x,
                list_row + (index - offset) as u16,
                width,
                label.clone(),
                index == form.selection,
            ));
        }
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
        for (column, icon) in [(1, "●"), (3, "▾"), (6, "●")] {
            let popup = state.sidebar.cursor.clone().unwrap();
            let hits = chrome(
                &mut surface,
                ChromeContext {
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
            let cursor = &surface.frame.cursor;
            assert_eq!((cursor.x, cursor.y), (column, row));
            assert!(!cursor.visible);
            assert_eq!(
                surface.frame.rows[row as usize].cells[column as usize].text,
                icon
            );
            if column == 6 {
                // The same target's branch-detail row is painted immediately
                // below the item, but must not become its physical anchor.
                assert!(hits.iter().any(|(target, y)| target == &popup && *y > row));
            }
            state.sidebar.navigate(1);
        }
        state.sidebar.active = false;
        chrome(
            &mut surface,
            ChromeContext {
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
    fn codex_sidebar_logo_keeps_its_spacer_and_requires_current_identity() {
        let (mut pane, mut snapshot) = capture_fixture();
        pane.harness.as_mut().unwrap().kind = HarnessKind::Codex;
        pane.title = "日本語".into();
        for width in 0..12 {
            let label = sidebar_pane_label(&pane, Some(&snapshot), width);
            assert!(unicode_width::UnicodeWidthStr::width(label.as_str()) <= width);
            assert_eq!(label.contains('\u{ec81}'), width >= 2);
            if width >= 2 {
                assert!(label.starts_with("\u{ec81} "));
            }
        }
        snapshot.program_identity.as_mut().unwrap().birth = "stale".into();
        assert!(!sidebar_pane_label(&pane, Some(&snapshot), 20).contains('\u{ec81}'));
        pane.status = PaneStatus::Exited;
        assert!(!sidebar_pane_label(&pane, None, 20).contains('\u{ec81}'));
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
        let context = || ChromeContext {
            sidebar_popup: None,
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
        };
        let mut projects = Vec::new();
        chrome(
            &mut surface,
            context(),
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut projects,
        );
        theme_chrome(
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
            marker.style.foreground_source,
            bkpr_protocol::ColorSource::Palette(5)
        );
        assert!(!marker.style.faint && !marker.style.bold);
        let detail_y = state
            .sidebar
            .rows
            .iter()
            .position(|row| matches!(row.kind, SidebarRowKind::Detail { .. }))
            .unwrap()
            + 2;
        let branch = &surface.frame.rows[detail_y].cells[6];
        assert_eq!(
            branch.style.foreground_source,
            theme.ink(Role::Muted, false)
        );
        assert!(branch.style.faint);
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
        let registry = capture_registry(&pane);
        let mut state = Controller::new(&registry);
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
            let context = || ChromeContext {
                sidebar_popup: None,
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
            };
            chrome(
                &mut surface,
                context(),
                |_| None,
                |_| Some(&snapshot),
                |_| None,
                &mut Vec::new(),
            );
            theme_chrome(
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
            assert_eq!(logo.style.background_source, background);
            assert_eq!(
                cells[1].style.foreground_source,
                theme.ink(Role::Base, active)
            );
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
        state.sidebar.active = false;
        state.sidebar.cursor = Some(SidebarTarget::Workspace("work".into()));
        let context = ChromeContext {
            sidebar_popup: None,
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
        };
        // A retained frame must lose the inherited bright guides without repaint.
        theme_chrome(
            &mut surface,
            &theme,
            context,
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
        assert_eq!(
            surface.frame.rows[pane_y].cells[1].style.foreground_source,
            theme.ink(Role::Muted, false)
        );
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
        let hits = chrome(
            &mut surface,
            ChromeContext {
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
        let (mut pane, snapshot) = capture_fixture();
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
            modal(&mut surface, size, &form, &registry, |_| Some(&snapshot));
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
    fn window_chooser_uses_its_captured_workspace_instead_of_registry_position() {
        let mut registry = selection_registry();
        let mut form = Form::new(FormKind::ChooseWindow, String::new(), None);
        form.target = Some((SidebarTarget::Workspace("work".into()), 10));
        registry.workspaces.reverse();
        let mut surface = capture_surface();
        let size = surface.frame.size;
        modal(&mut surface, size, &form, &registry, |_| None);
        let text = painted_text(&surface);
        assert!(text.contains("first") && text.contains("second"));
        assert!(!text.contains("third"));
        assert!(!surface.frame.cursor.visible);
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
        let hit = chrome(
            &mut surface,
            ChromeContext {
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
        chrome(
            surface,
            ChromeContext {
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

    fn capture_chooser(surface: &mut UiSurface, pane: &PaneInfo, snapshot: &PaneSnapshot) {
        let registry = capture_registry(pane);
        let form = Form::new(FormKind::ChoosePane, String::new(), Some(pane.id.clone()));
        let size = surface.frame.size;
        modal(surface, size, &form, &registry, |_| Some(snapshot));
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
            chrome(
                &mut surface,
                ChromeContext {
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
            chrome(
                &mut surface,
                ChromeContext {
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
            chrome(
                &mut fresh,
                ChromeContext {
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
                chrome(
                    surface,
                    ChromeContext {
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

        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("unmanaged") && text.contains("/current/project"));
        assert!(!text.contains("awaiting binding"));

        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
            launch_id: "launch-1".into(),
            conversation: None,
            error: None,
        });
        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("launch-1") && text.contains("awaiting binding"));
        assert!(!text.contains("unmanaged") && !text.contains("working"));

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
        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("native-conversation-EXACT"));
        assert!(!text.contains("awaiting binding"));

        pane.harness
            .as_mut()
            .unwrap()
            .managed
            .as_mut()
            .unwrap()
            .error = Some("native hook rejected capture".into());
        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("native-conversation-EXACT"));
        assert!(text.contains("native hook rejected capture"));
        assert!(!text.contains("working"));
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
        let hits = chrome(
            &mut surface,
            ChromeContext {
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
        chrome(
            &mut surface,
            ChromeContext {
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
        capture_chooser(&mut surface, &pane, &snapshot);
        assert!(painted_text(&surface).contains("previous-native-conversation"));

        let replacement = pane.harness.as_mut().unwrap();
        replacement.pid = 42; // Same birth tick is not the same process.
        replacement.managed = Some(ManagedHarness {
            launch_id: "launch-2".into(),
            conversation: None,
            error: None,
        });
        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(!text.contains("launch-2") && !text.contains("awaiting binding"));
        assert!(!text.contains("previous-native-conversation") && !text.contains("working"));

        snapshot.program_identity = Some(ProcessIdentity {
            pid: 42,
            birth: "100".into(),
        });
        capture_chooser(&mut surface, &pane, &snapshot);
        assert!(painted_text(&surface).contains("awaiting binding"));
        capture_chrome(&mut surface, &pane, &snapshot);
        let working_frame = surface.frame.clone();
        pane.harness.as_mut().unwrap().birth = "200".into();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert_ne!(surface.frame, working_frame);

        for lifecycle in ["exited", "terminated"] {
            snapshot.lifecycle = lifecycle.into();
            capture_chooser(&mut surface, &pane, &snapshot);
            let text = painted_text(&surface);
            assert!(!text.contains("launch-2") && !text.contains("working"));
        }
        snapshot.lifecycle = "running".into();
        pane.harness = None;
        capture_chooser(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(!text.contains("launch-2") && !text.contains("Claude"));
        assert!(text.contains("/current/project"));
    }

    #[test]
    fn metadata_popover_retires_harness_on_snapshot_identity_change_without_registry_change() {
        let (pane, mut snapshot) = capture_fixture();
        let registry = capture_registry(&pane);
        let size = capture_surface().frame.size;
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
            snapshot.program_identity = Some(ProcessIdentity {
                pid: 41,
                birth: "100".into(),
            });
            let mut popover = crate::popover::Popover::new(size);
            popover
                .paint(&registry, Some(&target), 2, 24, size, |_| Some(&snapshot))
                .unwrap();
            assert!(painted_text(&popover.surface).contains("Claude"));
            snapshot.program_identity = None;
            popover
                .paint(&registry, Some(&target), 2, 24, size, |_| Some(&snapshot))
                .unwrap();
            assert!(!painted_text(&popover.surface).contains("Claude"));
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
        capture_chooser(&mut surface, &pane, &snapshot);
        assert!(painted_text(&surface).contains("previous-conversation"));

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
            capture_chooser(&mut surface, &pane, &snapshot);
            let text = painted_text(&surface);
            assert!(!text.contains("previous-conversation") && !text.contains("previous-launch"));
            assert!(!text.contains("Claude") && !text.contains("working"));
            assert!(text.contains("/current/project"));
            capture_chrome(&mut surface, &pane, &snapshot);
            let text = painted_text(&surface);
            assert!(!text.contains("Claude") && !text.contains("[claude"));
            assert!(!text.contains("bound") && !text.contains("working"));
            assert!(text.contains("Design notes") && text.contains("User workspace"));
        }
    }

    #[test]
    fn short_pane_chooser_keeps_selected_item_when_details_do_not_fit() {
        let (mut pane, snapshot) = capture_fixture();
        let size = Size {
            cols: 60,
            rows: 8,
            cell_width: 9,
            cell_height: 18,
        };
        let mut surface = UiSurface::new(size);
        for has_harness in [true, false] {
            if !has_harness {
                pane.harness = None;
                pane.foreground_cwd = None;
            }
            capture_chooser(&mut surface, &pane, &snapshot);
            assert!(surface.frame.rows.iter().any(|row| {
                row.cells.iter().any(|cell| cell.style.inverse)
                    && row
                        .cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("Design notes")
            }));
            assert!(!surface.frame.cursor.visible);
        }
    }

    #[test]
    fn narrow_capture_chrome_keeps_geometry_hit_targets_and_passive_cursor() {
        let (mut pane, snapshot) = capture_fixture();
        pane.title = "界界界 user title".into();
        pane.harness.as_mut().unwrap().managed = Some(ManagedHarness {
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
        capture_chooser(&mut surface, &pane, &snapshot);
        assert!(!surface.frame.cursor.visible);
        assert_eq!(surface.frame.rows.len(), size.rows as usize);
        assert!(
            surface
                .frame
                .rows
                .iter()
                .all(|row| row.cells.len() == size.cols as usize)
        );
        assert!(painted_text(&surface).contains("clipped"));
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
            let context = || ChromeContext {
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                pane_graphics: &[],
                guide_rows: 0,
                sidebar_popup: sidebar_popup_target(&state, !visible)
                    .map(|target| (target, raised_bg)),
            };
            let mut projects = Vec::new();
            chrome(
                &mut surface,
                context(),
                |_| None,
                |_| Some(&snapshot),
                |_| None,
                &mut projects,
            );
            theme_chrome(
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
                    theme.background(Role::Base, false)
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
        let context = || ChromeContext {
            size,
            state: &state,
            registry: &registry,
            rects: &[],
            pane_graphics: &[],
            guide_rows: 0,
            sidebar_popup: Some((&target, ColorSource::Default)),
        };
        chrome(
            &mut surface,
            context(),
            |_| None,
            |_| Some(&snapshot),
            |_| None,
            &mut Vec::new(),
        );
        theme_chrome(
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
