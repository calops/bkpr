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
pub fn chrome(
    surface: &mut UiSurface,
    size: Size,
    state: &Controller,
    registry: &RegistrySnapshot,
    rects: &[(String, Rect)],
    rgb: bool,
    pane_status: impl Fn(&str) -> Option<&'static str>,
) -> Vec<(SidebarTarget, u16)> {
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
                let label = format!(
                    "  {} {}",
                    if focused {
                        "●"
                    } else if projected {
                        "·"
                    } else {
                        "◌"
                    },
                    pane.title
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
            .map(|p| format!("{} [{:?}]", p.title, p.status))
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
pub fn modal(surface: &mut UiSurface, size: Size, form: &Form, registry: &RegistrySnapshot) {
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
                        "{} [{:?}] {}",
                        p.title,
                        p.status,
                        p.workspace_id
                            .as_ref()
                            .and_then(|id| registry.workspaces.iter().find(|w| w.id == *id))
                            .map(|w| w.name.as_str())
                            .unwrap_or("unassigned")
                    )
                })
                .collect::<Vec<_>>()
        };
        let available = size.rows.saturating_sub(y + 5) as usize;
        let offset = form
            .selection
            .saturating_add(1)
            .saturating_sub(available.max(1));
        for (index, label) in labels.iter().enumerate().skip(offset).take(available) {
            lines.push(line(
                x,
                y + 2 + (index - offset) as u16,
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
            size,
            &Controller::new(&registry),
            &registry,
            &[],
            false,
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
}
