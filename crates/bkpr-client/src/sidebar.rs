use bkpr_protocol::{LayoutNode, PaneStatus, RegistrySnapshot};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarTarget {
    Workspace(String),
    Pane { workspace: String, pane: String },
}
impl SidebarTarget {
    pub fn workspace(&self) -> &str {
        match self {
            Self::Workspace(id) | Self::Pane { workspace: id, .. } => id,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RowKind {
    Workspace { index: usize, collapsed: bool },
    Pane { index: usize, projected: bool },
    Detail { index: usize },
    Spacer,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub target: Option<SidebarTarget>,
    pub kind: RowKind,
}

#[derive(Default)]
pub struct Sidebar {
    pub active: bool,
    pub cursor: Option<SidebarTarget>,
    pub scroll: usize,
    pub rows: Vec<Row>,
    pub hover: Option<SidebarTarget>,
    pub pointer_row: Option<u16>,
    last_click: Option<(SidebarTarget, Instant)>,
    collapsed: HashSet<String>,
    revision: Option<u64>,
    height: usize,
    reveal: bool,
}
impl Sidebar {
    pub fn reconcile(
        &mut self,
        registry: &RegistrySnapshot,
        workspace: Option<&str>,
        focused: Option<&str>,
    ) {
        if self.revision != Some(registry.revision) {
            self.collapsed
                .retain(|id| registry.workspaces.iter().any(|w| w.id == *id));
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
                if let Some(layout) = &workspace.layout {
                    leaves(layout, &mut projected);
                }
                for (index, pane) in registry.panes.iter().enumerate() {
                    let visible = projected.contains(pane.id.as_str());
                    if !visible
                        && (pane.status == PaneStatus::Exited
                            || pane.workspace_id.as_deref() != Some(&workspace.id))
                    {
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
                    if pane.project.is_some() {
                        self.rows.push(Row {
                            target: Some(target),
                            kind: RowKind::Detail { index },
                        });
                    }
                }
            }
            self.revision = Some(registry.revision);
        }
        let focus_matches = match (&self.cursor, workspace, focused) {
            (
                Some(SidebarTarget::Pane {
                    workspace: selected,
                    pane,
                }),
                Some(workspace),
                Some(focused),
            ) => selected == workspace && pane == focused && !self.collapsed.contains(workspace),
            (Some(SidebarTarget::Workspace(selected)), Some(workspace), focused) => {
                selected == workspace && (focused.is_none() || self.collapsed.contains(workspace))
            }
            (None, None, _) => true,
            _ => false,
        };
        if !self.active && !focus_matches {
            self.reveal = true;
            self.cursor = workspace.map(|workspace| match focused {
                Some(pane) if !self.collapsed.contains(workspace) => SidebarTarget::Pane {
                    workspace: workspace.into(),
                    pane: pane.into(),
                },
                _ => SidebarTarget::Workspace(workspace.into()),
            });
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
        let workspace = cursor.workspace().to_owned();
        let folded = collapsed.unwrap_or(!self.collapsed.contains(&workspace));
        if folded {
            self.collapsed.insert(workspace.clone());
            self.cursor = Some(SidebarTarget::Workspace(workspace));
        } else {
            self.collapsed.remove(&workspace);
        }
        self.revision = None;
        self.reveal = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PaneInfo, Workspace};
    fn registry() -> RegistrySnapshot {
        RegistrySnapshot {
            revision: 1,
            workspaces: ["a", "b"]
                .into_iter()
                .map(|id| Workspace {
                    id: id.into(),
                    name: id.into(),
                    revision: 1,
                    layout: Some(LayoutNode::Leaf {
                        pane_id: "shared".into(),
                    }),
                })
                .collect(),
            panes: vec![PaneInfo {
                id: "shared".into(),
                workspace_id: Some("a".into()),
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
            }],
        }
    }
    #[test]
    fn shared_pane_selection_is_workspace_qualified_and_folding_keeps_its_group() {
        let registry = registry();
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("a"), Some("shared"));
        sidebar.enter();
        sidebar.navigate(isize::MAX);
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Pane {
                workspace: "b".into(),
                pane: "shared".into()
            })
        );
        sidebar.fold(Some(true));
        sidebar.reconcile(&registry, Some("a"), Some("shared"));
        assert_eq!(sidebar.cursor, Some(SidebarTarget::Workspace("b".into())));
        assert!(!sidebar.rows.iter().any(|row| matches!(&row.target,
            Some(SidebarTarget::Pane { workspace, .. }) if workspace == "b")));
        assert!(sidebar.rows.iter().any(|row| matches!(&row.target,
            Some(SidebarTarget::Pane { workspace, .. }) if workspace == "a")));
        sidebar.fold(Some(false));
        sidebar.reconcile(&registry, Some("a"), Some("shared"));
        sidebar.navigate(1);
        assert_eq!(
            sidebar.cursor,
            Some(SidebarTarget::Pane {
                workspace: "b".into(),
                pane: "shared".into()
            })
        );
    }
    #[test]
    fn manual_scrolling_survives_unchanged_focus_until_keyboard_navigation_reveals_cursor() {
        let mut registry = registry();
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("a"), Some("shared"));
        sidebar.viewport(2);
        sidebar.scroll_by(isize::MAX);
        let bottom = sidebar.scroll;
        assert!(bottom > 0);
        registry.revision += 1;
        sidebar.reconcile(&registry, Some("a"), Some("shared"));
        assert_eq!(sidebar.scroll, bottom);
        sidebar.enter();
        sidebar.navigate(isize::MIN);
        assert_eq!(sidebar.cursor, Some(SidebarTarget::Workspace("a".into())));
        assert_eq!(sidebar.scroll, 0);
        registry.workspaces.clear();
        registry.panes.clear();
        registry.revision += 1;
        sidebar.reconcile(&registry, None, None);
        sidebar.navigate(1);
        assert!(sidebar.cursor.is_none());
        assert_eq!(sidebar.scroll, 0);
    }
    #[test]
    fn double_click_requires_same_qualified_target_within_the_click_window() {
        let mut sidebar = Sidebar::default();
        let now = Instant::now();
        let first = SidebarTarget::Pane {
            workspace: "a".into(),
            pane: "shared".into(),
        };
        let other = SidebarTarget::Pane {
            workspace: "b".into(),
            pane: "shared".into(),
        };
        assert!(!sidebar.click(first.clone(), now));
        assert!(!sidebar.click(other.clone(), now + Duration::from_millis(100)));
        assert!(sidebar.click(other, now + Duration::from_millis(449)));
        assert!(!sidebar.click(first.clone(), now + Duration::from_millis(450)));
        assert!(!sidebar.click(first, now + Duration::from_millis(800)));
    }
}
