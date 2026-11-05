use bkpr_protocol::{LayoutNode, PaneStatus, ProjectState, RegistrySnapshot};
use bkpr_tui_framework::{Navigation, NavigationRow};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub target: Option<SidebarTarget>,
    pub kind: RowKind,
}

impl NavigationRow<SidebarTarget> for Row {
    fn target(&self) -> Option<&SidebarTarget> {
        self.target.as_ref()
    }
}

#[derive(Default)]
pub struct Sidebar {
    navigation: Navigation<SidebarTarget, Row>,
    revision: Option<u64>,
}
impl std::ops::Deref for Sidebar {
    type Target = Navigation<SidebarTarget, Row>;
    fn deref(&self) -> &Self::Target {
        &self.navigation
    }
}
impl std::ops::DerefMut for Sidebar {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.navigation
    }
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
            self.rows.clear();
            let mut projected = HashSet::new();
            for (index, workspace) in registry.workspaces.iter().enumerate() {
                if index > 0 {
                    self.rows.push(Row {
                        target: None,
                        kind: RowKind::Spacer,
                    });
                }
                self.rows.push(Row {
                    target: Some(SidebarTarget::Workspace(workspace.id.clone())),
                    kind: RowKind::Workspace {
                        index,
                        collapsed: false,
                    },
                });
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
                    if grouped {
                        self.rows.push(Row {
                            target: Some(SidebarTarget::Window {
                                workspace: workspace.id.clone(),
                                window: window.id.clone(),
                            }),
                            kind: RowKind::Window {
                                workspace: index,
                                index: window_index,
                                collapsed: false,
                            },
                        });
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
        let desired = workspace.map(|workspace| match window {
            Some(_) if focused.is_some() => SidebarTarget::Pane {
                workspace: workspace.into(),
                pane: focused.unwrap().into(),
            },
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
        });
        if !self.active && self.cursor != desired {
            self.request_reveal();
            self.cursor = desired;
        }
        if !self.cursor.as_ref().is_some_and(|cursor| {
            self.rows
                .iter()
                .any(|row| row.target.as_ref() == Some(cursor))
        }) {
            self.request_reveal();
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
        self.refresh_viewport();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PaneInfo, Window, Workspace};
    use std::time::{Duration, Instant};
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
