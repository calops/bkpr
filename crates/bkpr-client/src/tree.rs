//! One owner-qualified metadata tree. Native sessions supply fully unfolded rows;
//! this model alone owns visible navigation and presentation-local folding.
use crate::controller::{SessionGroup, SessionStatus, SidebarRow, SidebarRowKind, SidebarTarget};
use bkpr_protocol::{PaneSnapshot, RegistrySnapshot};
use bkpr_tui_framework::{Navigation, NavigationRow};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner {
    pub slot: usize,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Node {
    Session,
    Native(SidebarTarget),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Target {
    pub owner: Owner,
    pub node: Node,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Session,
    Native(SidebarRowKind),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub target: Option<Target>,
    pub owner: Owner,
    pub parent: Option<Target>,
    pub kind: Kind,
    pub depth: u16,
    pub last_sibling: bool,
    pub continuations: u8,
    parent_row: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct FocusEdge {
    pub depth: u16,
    pub start: usize,
    pub end: usize,
}
impl TreeRow {
    fn branch(&self) -> bool {
        matches!(
            self.kind,
            Kind::Native(
                SidebarRowKind::Workspace { .. }
                    | SidebarRowKind::Window { .. }
                    | SidebarRowKind::Pane { .. }
            )
        )
    }

    pub fn icon_column(&self) -> u16 {
        1 + self.depth * 3
    }

    pub fn content_column(&self) -> u16 {
        if self.branch() {
            self.icon_column() + 2
        } else {
            3 + self.depth.saturating_sub(1) * 3
        }
    }

    pub fn write_prefix(&self, label: &mut String, highlighted: bool) {
        let spacer = matches!(self.kind, Kind::Native(SidebarRowKind::Spacer));
        let end = self.depth + u16::from(spacer);
        for level in 1..end {
            label.push_str(if !highlighted && self.continuations & (1 << level) != 0 {
                "│  "
            } else {
                "   "
            });
        }
        if self.branch() && self.depth > 0 {
            label.push_str(if highlighted {
                "   "
            } else if self.last_sibling {
                "╰─╴"
            } else {
                "├─╴"
            });
        } else if matches!(self.kind, Kind::Native(SidebarRowKind::Projects { .. })) {
            label.push_str(if highlighted { "  " } else { "│ " });
        } else if matches!(self.kind, Kind::Native(SidebarRowKind::Detail { .. })) {
            label.push_str("  ");
        }
    }
}

impl NavigationRow<Target> for TreeRow {
    fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }
}

/// Immutable cached metadata only; no endpoint, worker, or native session owner.
#[derive(Clone)]
pub(crate) struct Session {
    pub owner: Owner,
    pub group: SessionGroup,
    pub registry: Arc<RegistrySnapshot>,
    pub rows: Arc<[SidebarRow]>,
    pub workspace: Option<String>,
    pub window: Option<String>,
    pub focused: Option<String>,
    pub snapshots: Arc<HashMap<String, PaneSnapshot>>,
    pub statuses: Arc<HashMap<String, &'static str>>,
    pub history: Arc<HashMap<String, (u64, u64)>>,
}

#[derive(Default)]
pub(crate) struct Tree {
    navigation: Navigation<Target, TreeRow>,
    sessions: Vec<Arc<Session>>,
    collapsed: HashSet<Target>,
    project_capacity: u32,
}
impl std::ops::Deref for Tree {
    type Target = Navigation<Target, TreeRow>;
    fn deref(&self) -> &Self::Target {
        &self.navigation
    }
}
impl std::ops::DerefMut for Tree {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.navigation
    }
}
impl Tree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn session(&self, owner: Owner) -> Option<&Arc<Session>> {
        self.sessions.iter().find(|session| session.owner == owner)
    }
    pub fn sessions_len(&self) -> usize {
        self.sessions.len()
    }
    pub fn project_capacity(&self) -> u32 {
        self.project_capacity
    }
    pub fn focus_edges(&self) -> [Option<FocusEdge>; 3] {
        let mut edges = [None; 3];
        let Some(cursor) = self.cursor.as_ref() else {
            return edges;
        };
        let Some(mut child) = self.rows.iter().position(|row| {
            row.target.as_ref() == Some(cursor)
                && (row.branch() || matches!(row.kind, Kind::Session))
        }) else {
            return edges;
        };
        for edge in &mut edges {
            let Some(parent) = self.rows[child].parent_row else {
                break;
            };
            *edge = Some(FocusEdge {
                depth: self.rows[child].depth,
                start: parent + 1,
                end: child,
            });
            child = parent;
        }
        edges
    }

    pub fn reconcile(&mut self, sessions: Vec<Arc<Session>>, active: Owner) {
        let topology_changed = sessions.len() != self.sessions.len()
            || sessions.iter().zip(&self.sessions).any(|(a, b)| {
                a.owner != b.owner
                    || !Arc::ptr_eq(&a.rows, &b.rows)
                    || (a.group.status == SessionStatus::Online)
                        != (b.group.status == SessionStatus::Online)
            });
        let registry_changed = sessions.len() != self.sessions.len()
            || sessions
                .iter()
                .zip(&self.sessions)
                .any(|(a, b)| !Arc::ptr_eq(&a.registry, &b.registry));
        self.sessions = sessions;
        if registry_changed {
            self.project_capacity = self
                .sessions
                .iter()
                .flat_map(|session| &session.registry.panes)
                .filter_map(|pane| pane.project.as_ref().and_then(|project| project.color_slot))
                .max()
                .map_or(0, |slot| slot.saturating_add(1));
        }
        if topology_changed {
            self.collapsed.retain(|target| {
                self.sessions.iter().any(|session| {
                    session.owner == target.owner
                        && match &target.node {
                            Node::Session => true,
                            Node::Native(native) => session
                                .rows
                                .iter()
                                .any(|row| row.target.as_ref() == Some(native)),
                        }
                })
            });
            self.rebuild();
        }
        self.follow_active(active);
    }
    pub fn follow_active(&mut self, active: Owner) {
        if !self.active {
            let desired = self.desired(active);
            if self.cursor != desired {
                self.navigation.cursor = desired;
                self.navigation.request_reveal();
            }
        }
        self.repair(active);
    }
    fn desired(&self, owner: Owner) -> Option<Target> {
        let session = self.session(owner)?;
        let native = session.workspace.as_ref().map(|workspace| {
            if let Some(pane) = &session.focused {
                SidebarTarget::Pane {
                    workspace: workspace.clone(),
                    pane: pane.clone(),
                }
            } else if let Some(window) = &session.window {
                SidebarTarget::Window {
                    workspace: workspace.clone(),
                    window: window.clone(),
                }
            } else {
                SidebarTarget::Workspace(workspace.clone())
            }
        });
        let target = native
            .map(|native| Target {
                owner,
                node: Node::Native(native),
            })
            .unwrap_or(Target {
                owner,
                node: Node::Session,
            });
        self.visible_ancestor(target)
    }
    fn contains(&self, target: &Target) -> bool {
        self.rows
            .iter()
            .any(|row| row.target.as_ref() == Some(target))
    }
    fn parent(&self, target: &Target) -> Option<Target> {
        let session = self.session(target.owner)?;
        match &target.node {
            Node::Session => None,
            Node::Native(SidebarTarget::Workspace(_)) => (self.sessions.len() > 1
                || session.group.status != SessionStatus::Online)
                .then_some(Target {
                    owner: target.owner,
                    node: Node::Session,
                }),
            Node::Native(SidebarTarget::Window { workspace, .. }) => Some(Target {
                owner: target.owner,
                node: Node::Native(SidebarTarget::Workspace(workspace.clone())),
            }),
            Node::Native(SidebarTarget::Pane { workspace, pane }) => {
                let window = session
                    .registry
                    .workspaces
                    .iter()
                    .find(|entry| entry.id == *workspace)
                    .filter(|entry| entry.windows.len() > 1)
                    .and_then(|entry| {
                        let pane = session
                            .registry
                            .panes
                            .iter()
                            .find(|entry| entry.id == *pane)?;
                        entry
                            .windows
                            .iter()
                            .find(|window| Some(&window.id) == pane.window_id.as_ref())
                    });
                Some(Target {
                    owner: target.owner,
                    node: Node::Native(match window {
                        Some(window) => SidebarTarget::Window {
                            workspace: workspace.clone(),
                            window: window.id.clone(),
                        },
                        None => SidebarTarget::Workspace(workspace.clone()),
                    }),
                })
            }
        }
    }
    fn visible_ancestor(&self, mut target: Target) -> Option<Target> {
        loop {
            if self.contains(&target) {
                return Some(target);
            }
            target = self.parent(&target)?;
        }
    }
    /// Restore captured focus only within the original owner generation.
    /// A retired modal owner must not replace the already repaired selection.
    pub fn restore_focus(&mut self, target: Target) {
        if let Some(target) = self.visible_ancestor(target) {
            self.navigation.select(target);
        }
    }

    fn repair(&mut self, active: Owner) {
        if !self
            .cursor
            .as_ref()
            .is_some_and(|target| self.contains(target))
        {
            self.navigation.cursor = self
                .cursor
                .clone()
                .and_then(|target| self.visible_ancestor(target))
                .or_else(|| {
                    let previous = self.cursor.as_ref()?.owner;
                    let replacement = self
                        .sessions
                        .iter()
                        .find(|session| {
                            session.owner.slot == previous.slot && session.owner != previous
                        })?
                        .owner;
                    // Only the session identity survives reconnect. Never carry
                    // a native identifier across attachment generations.
                    self.visible_ancestor(Target {
                        owner: replacement,
                        node: Node::Session,
                    })
                    .or_else(|| self.desired(replacement))
                })
                .or_else(|| self.desired(active))
                .or_else(|| self.rows.iter().find_map(|row| row.target.clone()));
            self.navigation.request_reveal();
        }
        if self
            .hover
            .as_ref()
            .is_some_and(|target| !self.contains(target))
        {
            self.navigation.hover = None;
        }
        self.refresh_viewport();
    }
    fn rebuild(&mut self) {
        self.navigation.rows.clear();
        let multi = self.sessions.len() > 1;
        for session in &self.sessions {
            let owner = session.owner;
            if !self.navigation.rows.is_empty() {
                self.navigation.rows.push(TreeRow {
                    target: None,
                    owner,
                    parent: None,
                    kind: Kind::Native(SidebarRowKind::Spacer),
                    depth: 0,
                    last_sibling: true,
                    continuations: 0,
                    parent_row: None,
                });
            }
            let root = Target {
                owner,
                node: Node::Session,
            };
            if multi || session.group.status != SessionStatus::Online {
                self.navigation.rows.push(TreeRow {
                    target: Some(root.clone()),
                    owner,
                    parent: None,
                    kind: Kind::Session,
                    depth: 0,
                    last_sibling: true,
                    continuations: 0,
                    parent_row: None,
                });
                if self.collapsed.contains(&root) {
                    continue;
                }
            }
            if session.group.status != SessionStatus::Online {
                continue;
            }
            let mut hidden_below = None;
            for row in session.rows.iter() {
                let target = row.target.clone().map(|native| Target {
                    owner,
                    node: Node::Native(native),
                });
                let (parent, depth) = match row.kind {
                    SidebarRowKind::Workspace { .. } | SidebarRowKind::Spacer => {
                        (multi.then(|| root.clone()), u16::from(multi))
                    }
                    SidebarRowKind::Projects { index } => (
                        session
                            .registry
                            .workspaces
                            .get(index)
                            .map(|workspace| Target {
                                owner,
                                node: Node::Native(SidebarTarget::Workspace(workspace.id.clone())),
                            }),
                        1 + u16::from(multi),
                    ),
                    SidebarRowKind::Window { .. } => (
                        target.as_ref().and_then(|target| self.parent(target)),
                        1 + u16::from(multi),
                    ),
                    SidebarRowKind::Pane { index, .. } | SidebarRowKind::Detail { index } => {
                        let parent = target.as_ref().and_then(|target| self.parent(target));
                        let grouped = parent.as_ref().is_some_and(|target| {
                            matches!(&target.node, Node::Native(SidebarTarget::Window { .. }))
                        });
                        if matches!(row.kind, SidebarRowKind::Detail { .. }) {
                            let pane = session.registry.panes.get(index).and_then(|pane| {
                                pane.workspace_id.as_ref().map(|workspace| Target {
                                    owner,
                                    node: Node::Native(SidebarTarget::Pane {
                                        workspace: workspace.clone(),
                                        pane: pane.id.clone(),
                                    }),
                                })
                            });
                            (pane, 2 + u16::from(grouped) + u16::from(multi))
                        } else {
                            (parent, 1 + u16::from(grouped) + u16::from(multi))
                        }
                    }
                };
                if let Some(hidden) = hidden_below {
                    if depth > hidden {
                        continue;
                    }
                    hidden_below = None;
                }
                let collapsed = target
                    .as_ref()
                    .is_some_and(|target| self.collapsed.contains(target));
                let kind = match row.kind {
                    SidebarRowKind::Workspace { index, .. } => {
                        if collapsed {
                            hidden_below = Some(depth);
                        }
                        SidebarRowKind::Workspace { index, collapsed }
                    }
                    SidebarRowKind::Window {
                        workspace, index, ..
                    } => {
                        if collapsed {
                            hidden_below = Some(depth);
                        }
                        SidebarRowKind::Window {
                            workspace,
                            index,
                            collapsed,
                        }
                    }
                    other => other,
                };
                self.navigation.rows.push(TreeRow {
                    target,
                    owner,
                    parent,
                    kind: Kind::Native(kind),
                    depth,
                    last_sibling: true,
                    continuations: 0,
                    parent_row: None,
                });
            }
        }
        // Cache the visible hierarchy once; metadata rows do not end sibling groups.
        let mut seen = 0u8;
        for row in self.navigation.rows.iter_mut().rev() {
            if matches!(row.kind, Kind::Session) {
                seen = 0;
            } else if row.branch() {
                let bit = 1 << row.depth;
                row.last_sibling = seen & bit == 0;
                seen = (seen & (bit - 1)) | bit;
            }
        }
        let mut continuations = 0u8;
        let mut ancestors = [None; 4];
        for (index, row) in self.navigation.rows.iter_mut().enumerate() {
            let depth = usize::from(row.depth);
            row.parent_row = depth
                .checked_sub(1)
                .and_then(|depth| ancestors.get(depth).copied().flatten());
            if row.branch() || matches!(row.kind, Kind::Session) {
                ancestors[depth] = Some(index);
                ancestors[depth + 1..].fill(None);
            }
            if matches!(row.kind, Kind::Session) {
                continuations = 0;
            } else if row.branch() {
                let bit = 1 << row.depth;
                continuations &= bit - 1;
                row.continuations = continuations;
                if !row.last_sibling {
                    continuations |= bit;
                }
                continue;
            }
            row.continuations = continuations;
        }
    }
    pub fn fold(&mut self, collapsed: Option<bool>) {
        let Some(cursor) = self.cursor.clone() else {
            return;
        };
        let mut target = cursor.clone();
        match &cursor.node {
            Node::Native(SidebarTarget::Pane { .. }) => {
                if collapsed == Some(false) {
                    return;
                }
                let Some(parent) = self.parent(&cursor) else {
                    return;
                };
                target = parent;
            }
            Node::Native(SidebarTarget::Workspace(_)) if collapsed == Some(true) => {
                if let Some(parent) = self.parent(&cursor) {
                    self.select(parent);
                    self.refresh_viewport();
                    return;
                }
            }
            _ => {}
        }
        let fold = collapsed.unwrap_or(!self.collapsed.contains(&target));
        let changed = if fold {
            self.collapsed.insert(target.clone())
        } else {
            self.collapsed.remove(&target)
        };
        self.select(target.clone());
        if changed {
            self.rebuild();
        }
        self.repair(target.owner);
    }
    pub fn jump_session(&mut self, active: Owner) {
        let root = Target {
            owner: active,
            node: Node::Session,
        };
        let target = if self.contains(&root) {
            Some(root)
        } else {
            self.rows
                .iter()
                .find(|row| row.owner == active && row.target.is_some())
                .and_then(|row| row.target.clone())
        };
        if let Some(target) = target {
            self.select(target);
        }
        self.refresh_viewport();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Sidebar;
    use bkpr_protocol::{LayoutNode, PaneInfo, PaneStatus, Window, Workspace};
    use std::time::{Duration, Instant};

    fn owner(slot: usize) -> Owner {
        Owner {
            slot,
            generation: 1,
        }
    }
    fn native(owner: Owner, node: SidebarTarget) -> Target {
        Target {
            owner,
            node: Node::Native(node),
        }
    }
    fn root(owner: Owner) -> Target {
        Target {
            owner,
            node: Node::Session,
        }
    }
    fn workspace(owner: Owner) -> Target {
        native(owner, SidebarTarget::Workspace("workspace".into()))
    }
    fn window(owner: Owner) -> Target {
        native(
            owner,
            SidebarTarget::Window {
                workspace: "workspace".into(),
                window: "one".into(),
            },
        )
    }
    fn pane(owner: Owner) -> Target {
        native(
            owner,
            SidebarTarget::Pane {
                workspace: "workspace".into(),
                pane: "first".into(),
            },
        )
    }
    fn session(owner: Owner, status: SessionStatus, grouped: bool) -> Arc<Session> {
        let pane = |id: &str, window: &str| PaneInfo {
            id: id.into(),
            workspace_id: Some("workspace".into()),
            window_id: Some(window.into()),
            title: id.into(),
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
        let mut windows = vec![window("one", "first")];
        let mut panes = vec![pane("first", "one")];
        if grouped {
            windows.push(window("two", "second"));
            panes.push(pane("second", "two"));
        }
        let registry = Arc::new(RegistrySnapshot {
            revision: 1,
            workspaces: vec![Workspace {
                id: "workspace".into(),
                name: "workspace".into(),
                revision: 1,
                windows,
            }],
            panes,
        });
        let mut sidebar = Sidebar::default();
        sidebar.reconcile(&registry, Some("workspace"), Some("one"), Some("first"));
        Arc::new(Session {
            owner,
            group: SessionGroup {
                name: format!("owner {}", owner.slot),
                host: None,
                status,
                detail: String::new(),
            },
            registry,
            rows: sidebar.rows.clone().into(),
            workspace: Some("workspace".into()),
            window: Some("one".into()),
            focused: Some("first".into()),
            snapshots: Arc::new(HashMap::new()),
            statuses: Arc::new(HashMap::new()),
            history: Arc::new(HashMap::new()),
        })
    }

    #[test]
    fn every_online_owner_has_its_own_subtree_even_with_identical_native_ids() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, true),
                session(owner(1), SessionStatus::Online, false),
                session(owner(2), SessionStatus::Connecting, true),
                session(owner(3), SessionStatus::Offline, true),
            ],
            owner(0),
        );
        for slot in 0..4 {
            assert!(tree.contains(&root(owner(slot))));
        }
        assert!(tree.contains(&pane(owner(0))));
        assert!(tree.contains(&pane(owner(1))));
        assert!(!tree.contains(&pane(owner(2))));
        assert!(!tree.contains(&pane(owner(3))));
        let first = tree
            .rows
            .iter()
            .find(|row| row.target.as_ref() == Some(&pane(owner(0))))
            .unwrap();
        let second = tree
            .rows
            .iter()
            .find(|row| row.target.as_ref() == Some(&pane(owner(1))))
            .unwrap();
        assert_eq!(first.parent, Some(window(owner(0))));
        assert_eq!(first.depth, 3);
        assert_eq!(second.parent, Some(workspace(owner(1))));
        assert_eq!(second.depth, 2);
        assert_ne!(first.target, second.target);
    }

    #[test]
    fn single_owner_preserves_native_rows_and_folding_without_a_session_parent() {
        let session = session(owner(0), SessionStatus::Online, false);
        let mut tree = Tree::new();
        tree.reconcile(vec![session.clone()], owner(0));
        assert_eq!(tree.rows.len(), session.rows.len());
        for (row, native) in tree.rows.iter().zip(session.rows.iter()) {
            assert_eq!(row.kind, Kind::Native(native.kind));
            assert_eq!(
                row.target,
                native.target.clone().map(|node| Target {
                    owner: owner(0),
                    node: Node::Native(node),
                })
            );
        }
        assert_eq!(tree.rows[0].parent, None);
        assert_eq!(tree.rows[0].depth, 0);
        tree.enter();
        tree.fold(Some(true));
        assert_eq!(tree.cursor, Some(workspace(owner(0))));
        assert_eq!(tree.rows.len(), 1);
        tree.fold(Some(false));
        assert!(tree.contains(&pane(owner(0))));
        tree.select(workspace(owner(0)));
        tree.fold(Some(true));
        assert_eq!(tree.cursor, Some(workspace(owner(0))));
        assert_eq!(tree.rows.len(), 1);
    }

    #[test]
    fn sole_disconnected_owner_keeps_status_root_and_online_jump_selects_workspace() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![session(owner(0), SessionStatus::Connecting, false)],
            owner(0),
        );
        assert_eq!(tree.rows.len(), 1);
        assert_eq!(tree.rows[0].kind, Kind::Session);
        assert_eq!(tree.cursor, Some(root(owner(0))));
        tree.jump_session(owner(0));
        assert_eq!(tree.cursor, Some(root(owner(0))));
        tree.reconcile(
            vec![session(owner(0), SessionStatus::Online, false)],
            owner(0),
        );
        assert!(!tree.contains(&root(owner(0))));
        assert_eq!(tree.cursor, Some(pane(owner(0))));
        tree.jump_session(owner(0));
        assert_eq!(tree.cursor, Some(workspace(owner(0))));
        tree.reconcile(
            vec![session(owner(0), SessionStatus::Offline, false)],
            owner(0),
        );
        assert_eq!(tree.rows.len(), 1);
        assert_eq!(tree.cursor, Some(root(owner(0))));
    }

    #[test]
    fn native_folds_and_direct_workspace_parent_use_the_same_tree() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, true),
                session(owner(1), SessionStatus::Online, false),
            ],
            owner(0),
        );
        tree.enter();
        tree.fold(Some(true));
        assert_eq!(tree.cursor, Some(window(owner(0))));
        assert!(!tree.contains(&pane(owner(0))));
        assert!(tree.contains(&pane(owner(1))));
        tree.fold(Some(false));
        assert!(tree.contains(&pane(owner(0))));
        tree.select(workspace(owner(0)));
        tree.fold(Some(true));
        assert_eq!(tree.cursor, Some(root(owner(0))));
        assert!(tree.contains(&pane(owner(0))));
        tree.fold(Some(true));
        assert!(!tree.contains(&workspace(owner(0))));
        assert!(tree.contains(&pane(owner(1))));
        tree.fold(Some(false));
        assert!(tree.contains(&pane(owner(0))));
        tree.jump_session(owner(1));
        assert_eq!(tree.cursor, Some(root(owner(1))));
    }

    #[test]
    fn unchanged_and_reordered_owners_preserve_selection_and_scroll() {
        let first = session(owner(0), SessionStatus::Online, true);
        let second = session(owner(1), SessionStatus::Online, true);
        let mut tree = Tree::new();
        tree.reconcile(vec![first.clone(), second.clone()], owner(0));
        tree.enter();
        tree.select(pane(owner(1)));
        tree.hover(Some(&window(owner(0))));
        tree.viewport(2);
        tree.scroll_by(isize::MAX);
        let scroll = tree.scroll;
        tree.reconcile(vec![first.clone(), second.clone()], owner(0));
        assert_eq!(tree.scroll, scroll);
        assert_eq!(tree.cursor, Some(pane(owner(1))));
        assert_eq!(tree.hover, Some(window(owner(0))));
        tree.reconcile(vec![second, first], owner(0));
        assert_eq!(tree.rows[0].owner, owner(1));
        assert_eq!(tree.cursor, Some(pane(owner(1))));
        assert_eq!(tree.hover, Some(window(owner(0))));
        tree.navigate(isize::MIN);
        assert_eq!(tree.cursor, Some(root(owner(1))));
        assert_eq!(tree.scroll, 0);
        assert_eq!(tree.hover, None);
    }

    #[test]
    fn disconnect_removal_and_generation_replacement_cannot_alias_other_owners() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(owner(1), SessionStatus::Online, false),
            ],
            owner(0),
        );
        tree.enter();
        tree.select(pane(owner(1)));
        tree.hover(Some(&pane(owner(1))));
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(owner(1), SessionStatus::Offline, false),
            ],
            owner(0),
        );
        assert_eq!(tree.cursor, Some(root(owner(1))));
        assert_eq!(tree.hover, None);
        tree.fold(Some(true));
        tree.hover(Some(&root(owner(1))));
        let replacement = Owner {
            slot: 1,
            generation: 2,
        };
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(replacement, SessionStatus::Online, false),
            ],
            owner(0),
        );
        assert!(!tree.contains(&root(owner(1))));
        assert!(tree.contains(&pane(replacement)));
        assert_eq!(tree.cursor, Some(root(replacement)));
        assert_eq!(tree.hover, None);
        tree.reconcile(Vec::new(), owner(0));
        tree.navigate(1);
        assert_eq!(tree.cursor, None);
        assert_eq!(tree.scroll, 0);
    }

    #[test]
    fn reconnect_keeps_foreign_session_focus_and_sole_online_uses_fresh_native_focus() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(owner(1), SessionStatus::Online, true),
            ],
            owner(0),
        );
        tree.enter();
        tree.select(native(
            owner(1),
            SidebarTarget::Pane {
                workspace: "workspace".into(),
                pane: "second".into(),
            },
        ));
        let replacement = Owner {
            slot: 1,
            generation: 2,
        };
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(replacement, SessionStatus::Connecting, true),
            ],
            owner(0),
        );
        assert_eq!(tree.cursor, Some(root(replacement)));
        let online = Owner {
            slot: 1,
            generation: 3,
        };
        tree.reconcile(vec![session(online, SessionStatus::Online, true)], owner(0));
        assert!(!tree.contains(&root(online)));
        assert_eq!(tree.cursor, Some(pane(online)));
        assert_ne!(
            tree.cursor,
            Some(native(
                online,
                SidebarTarget::Pane {
                    workspace: "workspace".into(),
                    pane: "second".into(),
                }
            ))
        );
    }

    #[test]
    fn modal_focus_from_retired_generation_does_not_replace_repaired_selection() {
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(owner(1), SessionStatus::Online, true),
            ],
            owner(0),
        );
        tree.enter();
        tree.select(pane(owner(1)));
        let replacement = Owner {
            slot: 1,
            generation: 2,
        };
        tree.reconcile(
            vec![
                session(owner(0), SessionStatus::Online, false),
                session(replacement, SessionStatus::Connecting, true),
            ],
            owner(0),
        );
        assert_eq!(tree.cursor, Some(root(replacement)));
        tree.restore_focus(pane(owner(1)));
        assert_eq!(tree.cursor, Some(root(replacement)));
        tree.restore_focus(root(owner(0)));
        assert_eq!(tree.cursor, Some(root(owner(0))));
    }

    #[test]
    fn detail_rows_share_one_navigation_stop_and_clicks_are_owner_qualified() {
        let mut first = session(owner(0), SessionStatus::Online, false);
        let metadata = Arc::get_mut(&mut first).unwrap();
        let mut rows = metadata.rows.to_vec();
        rows.push(SidebarRow {
            target: Some(SidebarTarget::Pane {
                workspace: "workspace".into(),
                pane: "first".into(),
            }),
            kind: SidebarRowKind::Detail { index: 0 },
        });
        metadata.rows = rows.into();
        let mut tree = Tree::new();
        tree.reconcile(
            vec![first, session(owner(1), SessionStatus::Online, false)],
            owner(0),
        );
        tree.enter();
        tree.navigate(1);
        assert_eq!(tree.cursor, Some(root(owner(1))));
        let detail = tree
            .rows
            .iter()
            .find(|row| matches!(row.kind, Kind::Native(SidebarRowKind::Detail { .. })))
            .unwrap();
        assert_eq!(detail.parent, Some(pane(owner(0))));
        assert_eq!(detail.depth, 3);
        let now = Instant::now();
        assert!(!tree.click(pane(owner(0)), now));
        assert!(!tree.click(pane(owner(1)), now + Duration::from_millis(100)));
        assert!(tree.click(pane(owner(1)), now + Duration::from_millis(200)));
    }
}
