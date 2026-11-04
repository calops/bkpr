use bkpr_protocol::{LayoutNode, PaneStatus, ProjectState, RegistrySnapshot};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarTarget {
    Workspace(String),
    Window { workspace: String, window: String },
    Pane { workspace: String, pane: String },
}
impl SidebarTarget {
    pub fn workspace(&self) -> &str {
        match self {
            Self::Workspace(id)
            | Self::Window { workspace: id, .. }
            | Self::Pane { workspace: id, .. } => id,
        }
    }
    pub fn window(&self) -> Option<&str> {
        match self {
            Self::Window { window, .. } => Some(window),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RowKind {
    Workspace {
        index: usize,
        collapsed: bool,
    },
    Projects {
        index: usize,
    },
    Window {
        workspace: usize,
        index: usize,
        collapsed: bool,
    },
    Pane {
        index: usize,
        projected: bool,
    },
    Detail {
        index: usize,
    },
    Spacer,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub target: Option<SidebarTarget>,
    pub kind: RowKind,
}

#[derive(Default)]
pub struct Sidebar {
    /// Client-local requested width, retained while the terminal is too narrow.
    pub preferred_columns: Option<u16>,
    pub active: bool,
    pub cursor: Option<SidebarTarget>,
    pub scroll: usize,
    pub rows: Vec<Row>,
    pub hover: Option<SidebarTarget>,
    pub pointer_row: Option<u16>,
    last_click: Option<(SidebarTarget, Instant)>,
    collapsed: HashSet<String>,
    collapsed_windows: HashSet<String>,
    revision: Option<u64>,
    height: usize,
    reveal: bool,
}
impl Sidebar {
    pub fn reconcile(
        &mut self,
        registry: &RegistrySnapshot,
        workspace: Option<&str>,
        window: Option<&str>,
        focused: Option<&str>,
    ) {
        if self.revision != Some(registry.revision) {
            self.collapsed
                .retain(|id| registry.workspaces.iter().any(|w| w.id == *id));
            self.collapsed_windows.retain(|id| {
                registry
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.windows)
                    .any(|window| window.id == *id)
            });
            self.rows.clear();
            let mut projected = HashSet::new();
            for (index, workspace) in registry.workspaces.iter().enumerate() {
                if index > 0 {
                    self.rows.push(Row {
                        target: None,
                        kind: RowKind::Spacer,
                    });
                }
                let collapsed = self.collapsed.contains(&workspace.id);
                self.rows.push(Row {
                    target: Some(SidebarTarget::Workspace(workspace.id.clone())),
                    kind: RowKind::Workspace { index, collapsed },
                });
                if collapsed {
                    continue;
                }
                if registry.panes.iter().any(|pane| {
                    pane.workspace_id.as_ref() == Some(&workspace.id)
                        && pane
                            .project
                            .as_ref()
                            .is_some_and(|project| project.repository.is_some())
                }) {
                    self.rows.push(Row {
                        target: Some(SidebarTarget::Workspace(workspace.id.clone())),
                        kind: RowKind::Projects { index },
                    });
                }
                for (window_index, window) in workspace.windows.iter().enumerate() {
                    // A lone window is still an exact backend identity, but does not
                    // need an extra visual/navigation level between workspace and pane.
                    let grouped = workspace.windows.len() > 1;
                    let collapsed = grouped && self.collapsed_windows.contains(&window.id);
                    if grouped {
                        self.rows.push(Row {
                            target: Some(SidebarTarget::Window {
                                workspace: workspace.id.clone(),
                                window: window.id.clone(),
                            }),
                            kind: RowKind::Window {
                                workspace: index,
                                index: window_index,
                                collapsed,
                            },
                        });
                    }
                    if collapsed {
                        continue;
                    }
                    projected.clear();
                    fn leaves<'a>(node: &'a LayoutNode, result: &mut HashSet<&'a str>) {
                        match node {
                            LayoutNode::Leaf { pane_id } => {
                                result.insert(pane_id);
                            }
                            LayoutNode::Split { first, second, .. } => {
                                leaves(first, result);
                                leaves(second, result);
                            }
                        }
                    }
                    if let Some(layout) = &window.layout {
                        leaves(layout, &mut projected);
                    }
                    for (index, pane) in registry.panes.iter().enumerate() {
                        if pane.workspace_id.as_deref() != Some(workspace.id.as_str())
                            || pane.window_id.as_deref() != Some(window.id.as_str())
                        {
                            continue;
                        }
                        let visible = projected.contains(pane.id.as_str());
                        if !visible && pane.status == PaneStatus::Exited {
                            continue;
                        }
                        let target = SidebarTarget::Pane {
                            workspace: workspace.id.clone(),
                            pane: pane.id.clone(),
                        };
                        self.rows.push(Row {
                            target: Some(target.clone()),
                            kind: RowKind::Pane {
                                index,
                                projected: visible,
                            },
                        });
                        if pane.project.as_ref().is_some_and(|project| {
                            project.repository.is_some()
                                || matches!(
                                    project.state,
                                    ProjectState::Loading | ProjectState::Error
                                )
                        }) {
                            self.rows.push(Row {
                                target: Some(target),
                                kind: RowKind::Detail { index },
                            });
                        }
                    }
                }
            }
            self.revision = Some(registry.revision);
        }
        let desired = workspace.map(|workspace| {
            if self.collapsed.contains(workspace) {
                return SidebarTarget::Workspace(workspace.into());
            }
            match window {
                Some(window)
                    if focused.is_some()
                        && (!self.collapsed_windows.contains(window)
                            || registry.workspaces.iter().any(|entry| {
                                entry.id == workspace && entry.windows.len() == 1
                            })) =>
                {
                    SidebarTarget::Pane {
                        workspace: workspace.into(),
                        pane: focused.unwrap().into(),
                    }
                }
                Some(window)
                    if registry
                        .workspaces
                        .iter()
                        .any(|entry| entry.id == workspace && entry.windows.len() > 1) =>
                {
                    SidebarTarget::Window {
                        workspace: workspace.into(),
                        window: window.into(),
                    }
                }
                Some(_) => SidebarTarget::Workspace(workspace.into()),
                None => SidebarTarget::Workspace(workspace.into()),
            }
        });
        if !self.active && self.cursor != desired {
            self.reveal = true;
            self.cursor = desired;
        }
        if !self.cursor.as_ref().is_some_and(|cursor| {
            self.rows
                .iter()
                .any(|row| row.target.as_ref() == Some(cursor))
        }) {
            self.reveal = true;
            self.cursor = self
                .cursor
                .as_ref()
                .map(|cursor| SidebarTarget::Workspace(cursor.workspace().into()))
                .filter(|cursor| {
                    self.rows
                        .iter()
                        .any(|row| row.target.as_ref() == Some(cursor))
                })
                .or_else(|| self.rows.iter().find_map(|row| row.target.clone()));
        }
        if self.hover.as_ref().is_some_and(|target| {
            !self
                .rows
                .iter()
                .any(|row| row.target.as_ref() == Some(target))
        }) {
            self.hover = None;
        }
        self.viewport(self.height);
    }

    pub fn viewport(&mut self, height: usize) {
        self.reveal |= height != self.height;
        self.height = height;
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
        if self.reveal && height > 0 {
            if let Some(first) = self
                .rows
                .iter()
                .position(|row| row.target == self.cursor && row.target.is_some())
            {
                let last = self
                    .rows
                    .iter()
                    .rposition(|row| row.target == self.cursor)
                    .unwrap_or(first);
                if first < self.scroll {
                    self.scroll = first;
                } else if last >= self.scroll + height {
                    self.scroll = (last + 1).saturating_sub(height).min(first);
                }
            }
            self.reveal = false;
        }
    }

    pub fn enter(&mut self) {
        self.active = true;
        self.reveal = true;
    }
    pub fn select(&mut self, target: SidebarTarget) {
        self.cursor = Some(target);
        self.reveal = true;
    }
    pub fn click(&mut self, target: SidebarTarget, now: Instant) -> bool {
        let double = self.last_click.as_ref().is_some_and(|(previous, at)| {
            previous == &target && now.saturating_duration_since(*at) < Duration::from_millis(350)
        });
        self.select(target.clone());
        self.last_click = if double { None } else { Some((target, now)) };
        double
    }
    pub fn hover(&mut self, target: Option<&SidebarTarget>) {
        if self.hover.as_ref() != target {
            self.hover = target.cloned();
        }
    }
    pub fn navigate(&mut self, delta: isize) {
        self.hover = None;
        self.pointer_row = None;
        self.last_click = None;
        let mut items = self
            .rows
            .iter()
            .filter_map(|row| row.target.as_ref())
            .peekable();
        let mut index = 0isize;
        let mut selected = 0isize;
        let mut count = 0isize;
        while let Some(item) = items.next() {
            while items.peek().is_some_and(|next| *next == item) {
                items.next();
            }
            if Some(item) == self.cursor.as_ref() {
                selected = index;
            }
            index += 1;
            count += 1;
        }
        if count == 0 {
            return;
        }
        let target = selected
            .saturating_add(delta)
            .clamp(0, count.saturating_sub(1));
        let mut previous = None;
        let mut index = 0;
        for row in &self.rows {
            if let Some(item) = &row.target {
                if previous == Some(item) {
                    continue;
                }
                if index == target {
                    self.cursor = Some(item.clone());
                    self.reveal = true;
                    break;
                }
                previous = Some(item);
                index += 1;
            }
        }
        self.viewport(self.height);
    }
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(self.height));
        self.reveal = false;
    }
    pub fn fold(&mut self, collapsed: Option<bool>) {
        let Some(cursor) = &self.cursor else {
            return;
        };
        if let SidebarTarget::Window { workspace, window } = cursor {
            let folded = collapsed.unwrap_or(!self.collapsed_windows.contains(window));
            if folded {
                self.collapsed_windows.insert(window.clone());
                self.cursor = Some(SidebarTarget::Window {
                    workspace: workspace.clone(),
                    window: window.clone(),
                });
            } else {
                self.collapsed_windows.remove(window);
            }
        } else if let SidebarTarget::Pane { workspace, .. } = cursor {
            let mut parent = None;
            for row in &self.rows {
                if row.target.as_ref() == Some(cursor) {
                    break;
                }
                match &row.target {
                    Some(SidebarTarget::Window { window, .. }) => parent = Some(window.clone()),
                    Some(SidebarTarget::Workspace(_)) => parent = None,
                    _ => {}
                }
            }
            if let Some(window) = parent
                && collapsed.unwrap_or(true)
            {
                self.collapsed_windows.insert(window.clone());
                self.cursor = Some(SidebarTarget::Window {
                    workspace: workspace.clone(),
                    window,
                });
            } else if collapsed.unwrap_or(true) {
                self.collapsed.insert(workspace.clone());
                self.cursor = Some(SidebarTarget::Workspace(workspace.clone()));
            }
        } else {
            let workspace = cursor.workspace().to_owned();
            let folded = collapsed.unwrap_or(!self.collapsed.contains(&workspace));
            if folded {
                self.collapsed.insert(workspace.clone());
                self.cursor = Some(SidebarTarget::Workspace(workspace));
            } else {
                self.collapsed.remove(&workspace);
            }
        }
        self.revision = None;
        self.reveal = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PaneInfo, Window, Workspace};
    fn registry() -> RegistrySnapshot {
        let pane = |id: &str, workspace: &str, window: &str| PaneInfo {
            id: id.into(),
            workspace_id: Some(workspace.into()),
            window_id: Some(window.into()),
            title: "terminal".into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: "/fixture".into(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: None,
            home: "/fixture".into(),
            isolated_environment: true,
        };
        let window = |id: &str, pane: &str| Window {
            id: id.into(),
            name: id.into(),
            revision: 1,
            layout: Some(LayoutNode::Leaf {
                pane_id: pane.into(),
            }),
        };
        RegistrySnapshot {
            revision: 1,
            workspaces: vec![
                Workspace {
                    id: "a".into(),
                    name: "a".into(),
                    revision: 1,
                    windows: vec![window("one", "first"), window("two", "second")],
                },
                Workspace {
                    id: "b".into(),
                    name: "b".into(),
                    revision: 1,
                    windows: vec![window("three", "third")],
                },
            ],
            panes: vec![
                pane("first", "a", "one"),
                pane("second", "a", "two"),
                pane("third", "b", "three"),
            ],
        }
    }
    #[test]
    fn windows_keep_same_workspace_panes_in_distinct_selectable_groups() {
        let registry = registry();
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("a"), Some("one"), Some("first"));
        sidebar.enter();
        sidebar.navigate(1);
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Window {
                workspace: "a".into(),
                window: "two".into()
            })
        );
        sidebar.navigate(1);
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Pane {
                workspace: "a".into(),
                pane: "second".into()
            })
        );
        sidebar.fold(Some(true));
        sidebar.reconcile(&registry, Some("a"), Some("one"), Some("first"));
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Window {
                workspace: "a".into(),
                window: "two".into()
            })
        );
        assert!(!sidebar.rows.iter().any(|row| matches!(&row.target,
            Some(SidebarTarget::Pane { pane, .. }) if pane == "second")));
        assert!(sidebar.rows.iter().any(|row| matches!(&row.target,
            Some(SidebarTarget::Pane { pane, .. }) if pane == "first")));
        sidebar.fold(Some(false));
        sidebar.reconcile(&registry, Some("a"), Some("one"), Some("first"));
        sidebar.navigate(1);
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Pane {
                workspace: "a".into(),
                pane: "second".into()
            })
        );
    }
    #[test]
    fn lone_window_elision_keeps_pane_identity_and_folds_its_workspace() {
        let registry = registry();
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("b"), Some("three"), Some("third"));
        let pane = SidebarTarget::Pane {
            workspace: "b".into(),
            pane: "third".into(),
        };
        assert_eq!(sidebar.cursor.as_ref(), Some(&pane));
        assert!(!sidebar.rows.iter().any(|row| matches!(&row.target,
            Some(SidebarTarget::Window { workspace, .. }) if workspace == "b")));
        assert!(
            sidebar
                .rows
                .iter()
                .any(|row| row.target.as_ref() == Some(&pane))
        );
        sidebar.enter();
        sidebar.fold(Some(true));
        sidebar.reconcile(&registry, Some("b"), Some("three"), Some("third"));
        assert_eq!(sidebar.cursor, Some(SidebarTarget::Workspace("b".into())));
        assert!(
            !sidebar
                .rows
                .iter()
                .any(|row| row.target.as_ref() == Some(&pane))
        );
    }

    #[test]
    fn manual_scrolling_survives_unchanged_focus_until_keyboard_navigation_reveals_cursor() {
        let mut registry = registry();
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("a"), Some("one"), Some("first"));
        sidebar.viewport(2);
        sidebar.scroll_by(isize::MAX);
        let bottom = sidebar.scroll;
        assert!(bottom > 0);
        registry.revision += 1;
        sidebar.reconcile(&registry, Some("a"), Some("one"), Some("first"));
        assert_eq!(sidebar.scroll, bottom);
        sidebar.enter();
        sidebar.navigate(isize::MIN);
        assert_eq!(sidebar.cursor, Some(SidebarTarget::Workspace("a".into())));
        assert_eq!(sidebar.scroll, 0);
        registry.workspaces.clear();
        registry.panes.clear();
        registry.revision += 1;
        sidebar.reconcile(&registry, None, None, None);
        sidebar.navigate(1);
        assert!(sidebar.cursor.is_none());
        assert_eq!(sidebar.scroll, 0);
    }
    #[test]
    fn double_click_requires_same_qualified_target_within_the_click_window() {
        let mut sidebar = Sidebar::default();
        let now = Instant::now();
        let first = SidebarTarget::Window {
            workspace: "a".into(),
            window: "one".into(),
        };
        let other = SidebarTarget::Window {
            workspace: "a".into(),
            window: "two".into(),
        };
        assert!(!sidebar.click(first.clone(), now));
        assert!(!sidebar.click(other.clone(), now + Duration::from_millis(100)));
        assert!(sidebar.click(other, now + Duration::from_millis(449)));
        assert!(!sidebar.click(first.clone(), now + Duration::from_millis(450)));
        assert!(!sidebar.click(first, now + Duration::from_millis(800)));
    }
}
