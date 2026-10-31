use bkpr_protocol::*;
use bkpr_tui_framework::{Rect, blank, text};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormKind {
    CreateWorkspace,
    RenameWorkspace,
    RenamePane,
    TerminatePane,
    RemoveWorkspace,
    ChooseWorkspace,
    ChoosePane,
}
#[derive(Clone, Debug)]
pub struct Form {
    pub kind: FormKind,
    pub value: String,
    pub selection: usize,
    pub restore: Option<String>,
}
pub struct Controller {
    pub workspace: Option<String>,
    pub focused: Option<String>,
    remembered: HashMap<String, String>,
    pub prefix: bool,
    pub form: Option<Form>,
    pub status: String,
    pub suppressed: std::collections::HashSet<String>,
}
impl Controller {
    pub fn new(registry: &RegistrySnapshot) -> Self {
        let mut state = Self {
            workspace: registry.workspaces.first().map(|w| w.id.clone()),
            focused: None,
            remembered: HashMap::new(),
            prefix: false,
            form: None,
            status: String::new(),
            suppressed: Default::default(),
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
    pub fn reconcile(&mut self, registry: &RegistrySnapshot) {
        if self.current(registry).is_none() {
            self.workspace = registry.workspaces.first().map(|w| w.id.clone());
            self.focused = None;
        }
        let mut ids = Vec::new();
        if let Some(w) = self.current(registry) {
            leaves(w.layout.as_ref(), &mut ids);
        }
        if !self.focused.as_ref().is_some_and(|id| ids.contains(id)) {
            self.focused = self
                .workspace
                .as_ref()
                .and_then(|w| self.remembered.get(w))
                .filter(|id| ids.contains(id))
                .cloned()
                .or_else(|| ids.first().cloned());
        }
    }
    pub fn focus(&mut self, pane: String) {
        self.focused = Some(pane.clone());
        if let Some(workspace) = &self.workspace {
            self.remembered.insert(workspace.clone(), pane);
        }
    }
    pub fn switch(&mut self, id: String, registry: &RegistrySnapshot) {
        self.workspace = Some(id);
        self.focused = None;
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
        if let Some(w) = self.current(registry) {
            leaves(w.layout.as_ref(), &mut ids);
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
        self.form = Some(Form {
            kind,
            value,
            selection: 0,
            restore: self.focused.clone(),
        });
    }
    pub fn cancel(&mut self) {
        if let Some(form) = self.form.take() {
            self.focused = form.restore;
        }
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
/// Horizontal partitions columns; vertical partitions rows. Every header and
/// content rectangle is derived from this single source, including hit testing.
pub fn layout_rects(layout: Option<&LayoutNode>, rect: Rect, result: &mut Vec<(String, Rect)>) {
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
            let length = if *axis == Axis::Horizontal {
                rect.width
            } else {
                rect.height
            };
            if length < 2 {
                layout_rects(Some(first), rect, result);
                return;
            }
            let n = ((u32::from(length) * u32::from(*ratio)) / 1000).clamp(1, u32::from(length - 1))
                as u16;
            let (a, b) = if *axis == Axis::Horizontal {
                (
                    Rect { width: n, ..rect },
                    Rect {
                        x: rect.x + n,
                        width: rect.width - n,
                        ..rect
                    },
                )
            } else {
                (
                    Rect { height: n, ..rect },
                    Rect {
                        y: rect.y + n,
                        height: rect.height - n,
                        ..rect
                    },
                )
            };
            layout_rects(Some(first), a, result);
            layout_rects(Some(second), b, result);
        }
        None => {}
    }
}
pub fn screen_geometry(size: Size) -> (Rect, Rect) {
    let sidebar = if size.cols >= 60 {
        24
    } else if size.cols >= 24 {
        size.cols / 3
    } else {
        0
    };
    (
        Rect {
            x: 0,
            y: 0,
            width: sidebar,
            height: size.rows.saturating_sub(1),
        },
        Rect {
            x: sidebar,
            y: 0,
            width: size.cols - sidebar,
            height: size.rows.saturating_sub(1),
        },
    )
}
pub fn content(rect: Rect) -> Rect {
    Rect {
        y: rect.y.saturating_add(1),
        height: rect.height.saturating_sub(1),
        ..rect
    }
}
#[derive(Clone, Debug)]
pub enum SidebarTarget {
    Workspace(String),
    Pane(String),
}
fn clipped(value: &str, width: usize) -> String {
    let mut used = 0;
    value
        .chars()
        .take_while(|c| {
            let n = unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            if used + n > width {
                false
            } else {
                used += n;
                true
            }
        })
        .collect()
}
fn recognized_harness<'a>(
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
        matches!(snapshot.lifecycle.as_str(), "exited" | "terminated")
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
fn harness_status<'a>(
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
fn sidebar_pane_label(pane: &PaneInfo, snapshot: Option<&PaneSnapshot>, width: usize) -> String {
    let Some(harness) = recognized_harness(pane, snapshot) else {
        return pane.title.clone();
    };
    let binding = match &harness.managed {
        None => "",
        Some(managed) if managed.error.is_some() => " error",
        Some(managed) if managed.conversation.is_some() => " bound",
        Some(_) => " binding",
    };
    let status = harness_status(pane, snapshot).map_or("", |status| match status.state.as_str() {
        "idle" => ".",
        "working" => "~",
        "done" => "+",
        "blocked" => "?",
        "error" => "!",
        _ => "",
    });
    let identity = format!("[{status}{}{binding}]", harness.kind.as_str());
    let identity_width = unicode_width::UnicodeWidthStr::width(identity.as_str());
    let title_width = width.saturating_sub(identity_width + 1).max(2).min(width);
    format!("{} {identity}", clipped(&pane.title, title_width))
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
/// Retained UI cells: text changes repaint narrow widget lines, not a fresh
/// full-screen grid. This is scene construction, never a competing output layer.
pub struct UiSurface {
    pub frame: Frame,
    lines: Vec<(Rect, String, bool)>,
}
impl UiSurface {
    pub fn new(size: Size) -> Self {
        Self {
            frame: blank(size),
            lines: Vec::new(),
        }
    }
    pub fn paint(&mut self, size: Size, lines: Vec<(Rect, String, bool)>, cursor: Option<Cursor>) {
        if self.frame.size != size {
            self.frame = blank(size);
            self.lines.clear();
        }
        if self.lines != lines {
            for (rect, _, _) in self.lines.iter().chain(&lines) {
                bkpr_tui_framework::clear(&mut self.frame, *rect);
            }
            for (rect, label, selected) in &lines {
                text(
                    &mut self.frame,
                    rect.x,
                    rect.y,
                    &clipped(label, rect.width as usize),
                    *selected,
                );
            }
            self.lines = lines;
        }
        self.frame.cursor.visible = false;
        if let Some(cursor) = cursor {
            self.frame.cursor = cursor;
        }
    }
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
pub struct ChromeContext<'a> {
    pub size: Size,
    pub state: &'a Controller,
    pub registry: &'a RegistrySnapshot,
    pub rects: &'a [(String, Rect)],
    pub rgb: bool,
}
pub fn chrome<'a>(
    surface: &mut UiSurface,
    context: ChromeContext<'_>,
    pane_status: impl Fn(&str) -> Option<&'static str>,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
) -> Vec<(SidebarTarget, u16)> {
    let ChromeContext {
        size,
        state,
        registry,
        rects,
        rgb,
    } = context;
    let (sidebar, _) = screen_geometry(size);
    let mut hit = Vec::new();
    let mut lines = vec![line(0, 0, sidebar.width, " Bkpr / workspaces", false)];
    let mut entries = Vec::new();
    for workspace in &registry.workspaces {
        let selected = Some(&workspace.id) == state.workspace.as_ref();
        entries.push((
            SidebarTarget::Workspace(workspace.id.clone()),
            format!("{} {}", if selected { "▸" } else { " " }, workspace.name),
            selected,
        ));
        if selected {
            let mut visible = Vec::new();
            leaves(workspace.layout.as_ref(), &mut visible);
            for pane in &registry.panes {
                let projected = visible.contains(&pane.id);
                if !projected
                    && (pane.status == PaneStatus::Exited
                        || pane.workspace_id.as_ref() != Some(&workspace.id))
                {
                    continue;
                }
                let focused = state.focused.as_ref() == Some(&pane.id);
                let label_width = sidebar.width.saturating_sub(5) as usize;
                let label = format!(
                    "  {} {}",
                    if focused {
                        "●"
                    } else if projected {
                        "·"
                    } else {
                        "◌"
                    },
                    sidebar_pane_label(pane, snapshot(&pane.id), label_width)
                );
                entries.push((SidebarTarget::Pane(pane.id.clone()), label, focused));
            }
        }
    }
    let available = sidebar.height.saturating_sub(2) as usize;
    let selected = entries
        .iter()
        .rposition(|(_, _, selected)| *selected)
        .unwrap_or(0);
    let offset = selected.saturating_add(1).saturating_sub(available);
    for (index, (target, label, selected)) in
        entries.into_iter().skip(offset).take(available).enumerate()
    {
        let row = index as u16 + 2;
        lines.push(line(
            0,
            row,
            sidebar.width.saturating_sub(1),
            label,
            selected,
        ));
        hit.push((target, row));
    }
    for (id, rect) in rects {
        let title = registry
            .panes
            .iter()
            .find(|p| p.id == *id)
            .map(|p| {
                let identity = harness_label(p, snapshot(id));
                format!(
                    "{} [{}{:?}]",
                    p.title,
                    identity
                        .map(|label| format!("{label} / "))
                        .unwrap_or_default(),
                    p.status
                )
            })
            .unwrap_or_else(|| "Lost pane".into());
        let title = if let Some(status) = pane_status(id) {
            format!("[{status}] {title}")
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
        lines.push(line(
            rect.x,
            rect.y,
            rect.width,
            title,
            state.focused.as_ref() == Some(id),
        ));
    }
    let status = if state.prefix {
        "Ctrl-b: c workspace  n pane  % / \" split  r/p rename  x unlayout  ! close  w/o choose  d detach".into()
    } else if !state.status.is_empty() {
        state.status.clone()
    } else {
        format!(
            "Ctrl-b commands  Ctrl-] detach | {}",
            if rgb {
                "RGB confirmed"
            } else {
                "ANSI fallback"
            }
        )
    };
    lines.push(line(
        0,
        size.rows.saturating_sub(1),
        size.cols,
        status,
        true,
    ));
    surface.paint(size, lines, None);
    hit
}
pub fn modal<'a>(
    surface: &mut UiSurface,
    size: Size,
    form: &Form,
    registry: &RegistrySnapshot,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
) {
    let x = (size.cols / 8).min(8);
    let y = (size.rows / 3).min(size.rows.saturating_sub(1));
    let width = size.cols - x;
    let mut cursor = None;
    let title = match form.kind {
        FormKind::CreateWorkspace => "Create workspace",
        FormKind::RenameWorkspace => "Rename workspace",
        FormKind::RenamePane => "Rename pane",
        FormKind::TerminatePane => "Terminate focused pane and its owned processes? Type yes",
        FormKind::RemoveWorkspace => "Remove workspace layout only (panes survive)? Type yes",
        FormKind::ChooseWorkspace => "Switch workspace (arrows / Enter)",
        FormKind::ChoosePane => "Show existing pane (membership unchanged)",
    };
    let mut lines = vec![line(x, y, width, title, true)];
    if matches!(form.kind, FormKind::ChooseWorkspace | FormKind::ChoosePane) {
        let labels = if form.kind == FormKind::ChooseWorkspace {
            registry
                .workspaces
                .iter()
                .map(|w| w.name.clone())
                .collect::<Vec<_>>()
        } else {
            registry
                .panes
                .iter()
                .map(|p| {
                    format!(
                        "{} [{:?}] {}{}",
                        p.title,
                        p.status,
                        p.workspace_id
                            .as_ref()
                            .and_then(|id| registry.workspaces.iter().find(|w| w.id == *id))
                            .map(|w| w.name.as_str())
                            .unwrap_or("unassigned"),
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
        let row = (y + 2).min(size.rows.saturating_sub(1));
        lines.push(line(x, row, width, form.value.clone(), false));
        cursor = Some(Cursor {
            x: (x + unicode_width::UnicodeWidthStr::width(form.value.as_str()) as u16)
                .min(size.cols.saturating_sub(1)),
            y: row,
            visible: true,
            blinking: false,
            shape: CursorShape::Bar,
            color: None,
        });
    }
    lines.push(line(
        x,
        size.rows.saturating_sub(2),
        width,
        "Enter apply · Esc cancel (returns focus to pane)",
        false,
    ));
    surface.paint(size, lines, cursor);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn leaf(id: &str) -> LayoutNode {
        LayoutNode::Leaf { pane_id: id.into() }
    }
    #[test]
    fn remove_layout_never_describes_termination() {
        let tree = split_leaf(Some(leaf("a")), "a", "b", Axis::Horizontal);
        assert_eq!(remove_leaf(tree, "a"), Some(leaf("b")));
    }
    #[test]
    fn split_geometry_tiles_without_overlap() {
        let tree = split_leaf(Some(leaf("a")), "a", "b", Axis::Horizontal);
        let mut rects = Vec::new();
        layout_rects(
            tree.as_ref(),
            Rect {
                x: 5,
                y: 0,
                width: 101,
                height: 30,
            },
            &mut rects,
        );
        assert_eq!(rects[0].1.width + rects[1].1.width, 101);
        assert_eq!(rects[0].1.x + rects[0].1.width, rects[1].1.x);
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
            title: title.into(),
            status,
            exit_code: None,
            cwd: "/".into(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
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
                layout: Some(leaf("history")),
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
                size,
                state: &Controller::new(&registry),
                registry: &registry,
                rects: &[],
                rgb: false,
            },
            |_| None,
            |_| None,
        );
        let displayed: String = surface
            .frame
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .map(|cell| cell.text.as_str())
            .collect();
        assert!(!displayed.contains("Closed shell"));
        for title in ["Live shell", "Faulted shell", "Retained scene"] {
            assert!(displayed.contains(title));
        }
        assert!(
            !hit.iter()
                .any(|(target, _)| matches!(target, SidebarTarget::Pane(id) if id == "closed"))
        );
        for expected in ["live", "fault", "history"] {
            assert!(
                hit.iter()
                    .any(|(target, _)| matches!(target, SidebarTarget::Pane(id) if id == expected))
            );
        }
    }
    fn capture_fixture() -> (PaneInfo, PaneSnapshot) {
        let pane = PaneInfo {
            id: "pane".into(),
            workspace_id: Some("work".into()),
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
            clipboard_pending: false,
            capabilities: PaneCapabilities {
                sixel: false,
                normalized_frames: true,
                clipboard_timeout_ms: 5000,
                view_timeout_ms: 5000,
                poll_wait_max_ms: 1000,
                row_deltas: true,
                scene_encodings: vec!["compact_v1".into()],
            },
            row_delta: None,
            scene: None,
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
                layout: Some(leaf(&pane.id)),
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
        let (_, rect) = screen_geometry(size);
        chrome(
            surface,
            ChromeContext {
                size,
                state: &state,
                registry: &registry,
                rects: &[(pane.id.clone(), rect)],
                rgb: false,
            },
            |_| None,
            |_| Some(snapshot),
        )
    }

    fn capture_chooser(surface: &mut UiSurface, pane: &PaneInfo, snapshot: &PaneSnapshot) {
        let registry = capture_registry(pane);
        let form = Form {
            kind: FormKind::ChoosePane,
            value: String::new(),
            selection: 0,
            restore: Some(pane.id.clone()),
        };
        let size = surface.frame.size;
        modal(surface, size, &form, &registry, |_| Some(snapshot));
    }

    #[test]
    fn presentation_separates_recognition_managed_binding_and_capture_failure() {
        let (mut pane, snapshot) = capture_fixture();
        let mut surface = capture_surface();
        capture_chrome(&mut surface, &pane, &snapshot);
        let text = painted_text(&surface);
        assert!(text.contains("Design notes") && text.contains("User workspace"));
        assert!(text.contains("Claude") && !text.contains("working"));

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
        assert!(text.contains("bound") && text.contains("Design notes"));
        assert!(!text.contains("working"));
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
        for app in [Some("claude-code"), None] {
            pane.harness_status = Some(working_record(app));
            capture_chrome(&mut surface, &pane, &snapshot);
            assert!(painted_text(&surface).contains("working"));
        }
        for app in [Some("nvim"), Some("omp")] {
            pane.harness_status = Some(working_record(app));
            capture_chrome(&mut surface, &pane, &snapshot);
            assert!(!painted_text(&surface).contains("working"));
        }
        pane.harness_status = Some(working_record(None));
        pane.harness_status.as_mut().unwrap().id = "nested-program".into();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert!(!painted_text(&surface).contains("working"));

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
        registry.workspaces[0].layout = None;
        let state = Controller::new(&registry);
        let mut surface = capture_surface();
        let size = surface.frame.size;
        let hits = chrome(
            &mut surface,
            ChromeContext {
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                rgb: false,
            },
            |_| None,
            |_| None,
        );
        assert!(
            hits.iter()
                .any(|(target, _)| matches!(target, SidebarTarget::Pane(id) if id == "pane"))
        );
        assert!(painted_text(&surface).contains("[~claude]"));

        registry.panes[0].harness_status = None;
        chrome(
            &mut surface,
            ChromeContext {
                size,
                state: &state,
                registry: &registry,
                rects: &[],
                rgb: false,
            },
            |_| None,
            |_| None,
        );
        assert!(!painted_text(&surface).contains("[~claude]"));
        assert!(painted_text(&surface).contains("[claude]"));
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
        assert!(painted_text(&surface).contains("working"));
        pane.harness.as_mut().unwrap().birth = "200".into();
        capture_chrome(&mut surface, &pane, &snapshot);
        assert!(!painted_text(&surface).contains("working"));

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
            let selected_row = surface.frame.rows[4]
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>();
            assert!(selected_row.contains("Design notes"));
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
        assert!(
            hits.iter()
                .any(|(target, _)| matches!(target, SidebarTarget::Pane(id) if id == "pane"))
        );
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
}
