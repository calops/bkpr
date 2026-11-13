//! Passive sidebar metadata projected into the existing single scene compositor.
//! Inspired by Bkpr main's MIT-licensed ui.rs tooltip model; this implementation
//! uses only pane-first protocol facts, without the old Zellij overlay transport.
use crate::controller::SidebarTarget;
use crate::metadata::{self, Paragraph, ProjectSpan};
use crate::project_colors::{ProjectPalette, ProjectSlots};
use bkpr_protocol::{PaneSnapshot, RegistrySnapshot, Size};
use bkpr_tui_framework::overlay::Placement;
use bkpr_tui_framework::{
    ForegroundSpan, Rect, Role, RoleSpan, Theme, UiSurface,
    window::{self, Border},
};
use unicode_width::UnicodeWidthStr;

pub const SURFACE_ID: &str = "ui:tooltip";

pub struct Content {
    rows: Vec<Paragraph>,
}

fn content_rect(size: Size) -> Rect {
    let mut rect = window::content(size);
    // Match finder/guide padding: corner cells are not wholly opaque in the
    // rounded physical contour and cannot hold the first or last text glyph.
    if rect.width > 2 {
        rect.x += 1;
        rect.width -= 2;
    }
    rect
}

fn belongs(registry: &RegistrySnapshot, target: &SidebarTarget) -> bool {
    let Some(workspace) = registry
        .workspaces
        .iter()
        .find(|w| w.id == target.workspace())
    else {
        return false;
    };
    match target {
        SidebarTarget::Workspace(_) => true,
        SidebarTarget::Window { window, .. } => workspace.windows.iter().any(|w| &w.id == window),
        SidebarTarget::Pane { pane, .. } => registry.panes.iter().any(|p| {
            p.id == *pane
                && p.workspace_id.as_deref() == Some(workspace.id.as_str())
                && workspace
                    .windows
                    .iter()
                    .any(|window| Some(&window.id) == p.window_id.as_ref())
        }),
    }
}

fn project_identity(project: &bkpr_protocol::ProjectSnapshot) -> &std::path::Path {
    project
        .repository
        .as_ref()
        .map_or(project.cwd.as_path(), |repository| {
            repository.common_dir.as_path()
        })
}

fn same_project_context(
    left: &bkpr_protocol::ProjectSnapshot,
    right: &bkpr_protocol::ProjectSnapshot,
) -> bool {
    left.state == right.state
        && left.error == right.error
        && left.ci == right.ci
        && match (&left.repository, &right.repository) {
            (Some(left), Some(right)) => left == right,
            (None, None) => left.cwd == right.cwd,
            _ => false,
        }
}
/// Snapshot is optional and must already be held by the frontend. No attachment,
/// filesystem discovery, forge requests, or hidden-pane reads are performed.
pub fn build_content<'a>(
    registry: &RegistrySnapshot,
    target: &SidebarTarget,
    projects: &ProjectSlots,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
) -> Option<Content> {
    if !belongs(registry, target) {
        return None;
    }
    let workspace = registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == target.workspace())?;
    if let SidebarTarget::Pane { pane, .. } = target {
        let pane = registry
            .panes
            .iter()
            .find(|candidate| candidate.id == *pane)?;
        return Some(Content {
            rows: metadata::content(
                pane,
                registry,
                snapshot(&pane.id),
                true,
                pane.project
                    .as_ref()
                    .and_then(|project| projects.slot(project)),
            ),
        });
    }
    let window = target
        .window()
        .and_then(|id| workspace.windows.iter().find(|window| window.id == id));
    let panes: Vec<_> = registry
        .panes
        .iter()
        .filter(|pane| {
            pane.workspace_id.as_deref() == Some(workspace.id.as_str())
                && window.is_none_or(|window| pane.window_id.as_ref() == Some(&window.id))
        })
        .collect();
    let mut rows = vec![
        Paragraph::new(
            window.map_or(workspace.name.as_str(), |window| window.name.as_str()),
            Role::Title,
        ),
        Paragraph::new(
            if window.is_some() {
                format!("{} · {} terminals", workspace.name, panes.len())
            } else {
                format!(
                    "{} windows · {} terminals",
                    workspace.windows.len(),
                    panes.len()
                )
            },
            Role::Muted,
        ),
        Paragraph::new("", Role::Panel),
        Paragraph::new("Terminals", Role::Title),
    ];
    for pane in &panes {
        rows.push(metadata::pane_summary(pane, snapshot(&pane.id), true));
    }
    if panes.is_empty() {
        rows.push(Paragraph::new(
            if window.is_some() {
                "No terminals in this window"
            } else {
                "No terminals in this workspace"
            },
            Role::Muted,
        ));
    }

    // Canonical Git common-dir groups linked worktrees into one project. Keep
    // distinct supplied contexts (branch, changes, freshness) within that group;
    // a stale pane must never make a ready sibling's facts appear current.
    let mut project_groups: Vec<Vec<&bkpr_protocol::ProjectSnapshot>> = Vec::new();
    for project in panes.iter().filter_map(|pane| pane.project.as_ref()) {
        let identity = project_identity(project);
        if let Some(group) = project_groups
            .iter_mut()
            .find(|group| project_identity(group[0]) == identity)
        {
            if !group
                .iter()
                .any(|prior| same_project_context(prior, project))
            {
                group.push(project);
            }
        } else {
            project_groups.push(vec![project]);
        }
    }
    if project_groups.is_empty() {
        rows.push(Paragraph::new("", Role::Panel));
        rows.push(Paragraph::new("Context", Role::Title));
        rows.push(Paragraph::new("No project metadata available", Role::Muted));
    }
    for group in project_groups {
        rows.push(Paragraph::new("", Role::Panel));
        rows.push(crate::ci::repository_heading(
            group[0],
            projects.slot(group[0]),
        ));
        let mut requests = std::collections::BTreeSet::new();
        for project in &group {
            if let Some(ci) = &project.ci
                && let Some(request) = &ci.change_request
                && requests.insert((&ci.repository, request.number))
            {
                if requests.len() > 1 {
                    rows.push(crate::ci::repository_heading(
                        project,
                        projects.slot(project),
                    ));
                }
                crate::ci::request_content(ci, &mut rows);
            }
        }
        let mut sources: Vec<&bkpr_protocol::ProjectSnapshot> = Vec::new();
        for (index, project) in group.iter().enumerate() {
            if index > 0 {
                rows.push(Paragraph::new("", Role::Panel));
            }
            metadata::project_content(Some(project), false, projects.slot(project), &mut rows);
            let Some(ci) = &project.ci else {
                continue;
            };
            if let Some(prior) = sources.iter_mut().find(|prior| {
                let old = prior.ci.as_ref().unwrap();
                old.repository == ci.repository
                    && old.head_sha == ci.head_sha
                    && old.local_head_sha == ci.local_head_sha
                    && old.checked_ref == ci.checked_ref
                    && old.change_request.as_ref().map(|request| request.number)
                        == ci.change_request.as_ref().map(|request| request.number)
            }) {
                let old = prior.ci.as_ref().unwrap();
                if ci.updated_at > old.updated_at
                    || (ci.updated_at == old.updated_at
                        && crate::ci::state(ci) == bkpr_protocol::CiStatus::Unknown)
                {
                    *prior = project;
                }
            } else {
                sources.push(project);
            }
        }
        if sources.is_empty() {
            crate::ci::content(Some(group[0]), None, false, &mut rows);
        }
        for project in sources {
            let pane = panes.iter().find(|pane| {
                pane.project
                    .as_ref()
                    .is_some_and(|candidate| std::ptr::eq(candidate, project))
            });
            crate::ci::content(
                Some(project),
                pane.map(|pane| pane.id.as_str()),
                false,
                &mut rows,
            );
        }
    }
    Some(Content { rows })
}

struct Source<'a> {
    owner: crate::tree::Owner,
    registry: &'a RegistrySnapshot,
    projects: &'a ProjectSlots,
}

#[derive(Clone, PartialEq, Eq)]
struct ContentKey {
    owner: crate::tree::Owner,
    target: Option<SidebarTarget>,
    group: Option<crate::controller::SessionGroup>,
    registry_revision: u64,
    snapshots: Vec<(String, Option<SnapshotKey>)>,
}
#[derive(Clone, PartialEq, Eq)]
struct SnapshotKey {
    pane_id: String,
    program_identity: Option<bkpr_protocol::ProcessIdentity>,
    pid: Option<u32>,
    lifecycle: String,
    error: Option<String>,
}
impl SnapshotKey {
    fn new(snapshot: &PaneSnapshot) -> Self {
        Self {
            pane_id: snapshot.pane_id.clone(),
            program_identity: snapshot.program_identity.clone(),
            pid: snapshot.pid,
            lifecycle: snapshot.lifecycle.clone(),
            error: snapshot.error.clone(),
        }
    }
    fn matches(&self, snapshot: &PaneSnapshot) -> bool {
        self.pane_id == snapshot.pane_id
            && self.program_identity == snapshot.program_identity
            && self.pid == snapshot.pid
            && self.lifecycle == snapshot.lifecycle
            && self.error == snapshot.error
    }
}
#[derive(Clone, PartialEq, Eq)]
struct PaintKey {
    screen: Size,
    sidebar_width: u16,
    anchor_row: u16,
    scroll: usize,
}

/// Retained passive surface. Ownership of hover/keyboard selection and focus
/// remains with App; paint never selects a pane or installs a cursor.
pub struct Popover {
    pub surface: UiSurface,
    content_key: Option<ContentKey>,
    content: Option<Content>,
    paint_key: Option<PaintKey>,
    rect: Option<Rect>,
    scroll: usize,
    max_scroll: usize,
    roles: Vec<RoleSpan>,
    projects: Vec<ProjectSpan>,
    foregrounds: Vec<ForegroundSpan>,
    refresh_hits: Vec<(Rect, bkpr_protocol::CiRefreshTarget)>,
    links: Vec<(u16, u16, u16, String)>,
    refresh_pending: Option<(crate::tree::Owner, bkpr_protocol::CiRefreshTarget)>,
    graphical: bool,
}
impl Popover {
    pub fn new(screen: Size) -> Self {
        Self {
            surface: UiSurface::new(Size {
                cols: 1,
                rows: 1,
                ..screen
            }),
            content_key: None,
            content: None,
            paint_key: None,
            rect: None,
            scroll: 0,
            max_scroll: 0,
            roles: Vec::new(),
            projects: Vec::new(),
            foregrounds: Vec::new(),
            graphical: false,
            refresh_hits: Vec::new(),
            links: Vec::new(),
            refresh_pending: None,
        }
    }
    pub(crate) fn set_graphical(&mut self, graphical: bool) -> bool {
        if self.graphical == graphical {
            return false;
        }
        self.graphical = graphical;
        self.paint_key = None;
        true
    }

    pub fn hide(&mut self) {
        self.content_key = None;
        self.content = None;
        self.paint_key = None;
        self.rect = None;
        self.scroll = 0;
        self.max_scroll = 0;
        self.refresh_hits.clear();
        self.clear_links();
    }
    fn clear_links(&mut self) {
        for (x, y, cells, _) in &self.links {
            if let Some(row) = self.surface.frame.rows.get_mut(usize::from(*y)) {
                for cell in row
                    .cells
                    .iter_mut()
                    .skip(usize::from(*x))
                    .take(usize::from(*cells))
                {
                    cell.hyperlink = None;
                }
            }
        }
        self.links.clear();
    }
    /// Positive values scroll down. Parent routes wheel input only over this rect.
    pub fn scroll_by(&mut self, rows: i32) {
        self.scroll = if rows < 0 {
            self.scroll.saturating_sub(rows.unsigned_abs() as usize)
        } else {
            self.scroll
                .saturating_add(rows as usize)
                .min(self.max_scroll)
        };
    }
    /// Resolve passive metadata exclusively through the target's immutable owner.
    pub(crate) fn paint_tree(
        &mut self,
        tree: &crate::tree::Tree,
        target: Option<&crate::tree::Target>,
        anchor_row: u16,
        sidebar_width: u16,
        screen: Size,
    ) -> Option<Rect> {
        let Some(target) = target else {
            self.hide();
            return None;
        };
        let Some(session) = tree.session(target.owner) else {
            self.hide();
            return None;
        };
        match &target.node {
            crate::tree::Node::Native(native) => self.paint_owned(
                Source {
                    owner: target.owner,
                    registry: &session.registry,
                    projects: tree.project_slots(target.owner),
                },
                Some(native),
                anchor_row,
                sidebar_width,
                screen,
                |id| session.snapshots.get(id),
            ),
            crate::tree::Node::Session => {
                let unchanged = self.content_key.as_ref().is_some_and(|key| {
                    key.owner == target.owner
                        && key.target.is_none()
                        && key.group.as_ref() == Some(&session.group)
                });
                if !unchanged {
                    if !self
                        .content_key
                        .as_ref()
                        .is_some_and(|key| key.owner == target.owner && key.target.is_none())
                    {
                        self.scroll = 0;
                    }
                    self.content = Some(Content {
                        rows: vec![
                            Paragraph::new(&session.group.name, Role::Title),
                            Paragraph::new(
                                session.group.status.badge(),
                                session.group.status.role(),
                            ),
                            Paragraph::new(
                                session.group.host.as_deref().unwrap_or("Local session"),
                                Role::Muted,
                            ),
                            Paragraph::new(&session.group.detail, Role::Base),
                        ],
                    });
                    self.content_key = Some(ContentKey {
                        owner: target.owner,
                        target: None,
                        group: Some(session.group.clone()),
                        registry_revision: session.registry.revision,
                        snapshots: Vec::new(),
                    });
                    self.paint_key = None;
                }
                self.layout(anchor_row, sidebar_width, screen)
            }
        }
    }

    fn paint_owned<'a>(
        &mut self,
        source: Source<'_>,
        target: Option<&SidebarTarget>,
        anchor_row: u16,
        sidebar_width: u16,
        screen: Size,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    ) -> Option<Rect> {
        let Source {
            owner,
            registry,
            projects,
        } = source;
        let Some(target) = target.filter(|target| belongs(registry, target)) else {
            self.hide();
            return None;
        };
        if let Some((pending_owner, pending)) = &self.refresh_pending
            && (*pending_owner != owner
                || registry
                    .panes
                    .iter()
                    .find(|pane| pane.id == pending.pane_id)
                    .and_then(|pane| pane.project.as_ref())
                    .is_none_or(|project| !crate::ci::matches_target(project, pending)))
        {
            self.refresh_pending = None;
        }
        let unchanged = self.content_key.as_ref().is_some_and(|key| {
            key.owner == owner
                && key.target.as_ref() == Some(target)
                && key.registry_revision == registry.revision
                && key
                    .snapshots
                    .iter()
                    .all(|(pane, key)| match (key, snapshot(pane)) {
                        (None, None) => true,
                        (Some(key), Some(snapshot)) => key.matches(snapshot),
                        _ => false,
                    })
        });
        if !unchanged {
            if !self
                .content_key
                .as_ref()
                .is_some_and(|key| key.owner == owner && key.target.as_ref() == Some(target))
            {
                self.scroll = 0;
            }
            self.content = build_content(registry, target, projects, &snapshot);
            let workspace = registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == target.workspace())?;
            let snapshots = registry
                .panes
                .iter()
                .filter(|pane| match target {
                    SidebarTarget::Pane { pane: id, .. } => pane.id == *id,
                    SidebarTarget::Workspace(_) => {
                        pane.workspace_id.as_deref() == Some(workspace.id.as_str())
                    }
                    SidebarTarget::Window { window, .. } => {
                        pane.workspace_id.as_deref() == Some(workspace.id.as_str())
                            && pane.window_id.as_ref() == Some(window)
                    }
                })
                .map(|pane| (pane.id.clone(), snapshot(&pane.id).map(SnapshotKey::new)))
                .collect();
            self.content_key = Some(ContentKey {
                owner,
                target: Some(target.clone()),
                group: None,
                registry_revision: registry.revision,
                snapshots,
            });
            self.paint_key = None;
        }
        self.layout(anchor_row, sidebar_width, screen)
    }

    fn layout(&mut self, anchor_row: u16, sidebar_width: u16, screen: Size) -> Option<Rect> {
        let key = PaintKey {
            screen,
            sidebar_width,
            anchor_row,
            scroll: self.scroll,
        };
        if self.paint_key.as_ref() == Some(&key) {
            return self.rect;
        }
        self.rect = None;
        self.paint_key = None;
        let content = self.content.as_ref()?;
        let raw = &content.rows;
        let preferred_width = raw
            .iter()
            .map(|paragraph| UnicodeWidthStr::width(paragraph.text.as_str()))
            .max()
            .unwrap_or(1)
            .saturating_add(4)
            .clamp(24, 64) as u16;
        // Align the first content row, not the card's framing row, with the item.
        // Tiny-screen above/below placement still excludes the actual source row.
        let popup_row = if screen.cols.saturating_sub(sidebar_width) >= 4 {
            anchor_row.saturating_sub(1)
        } else {
            anchor_row
        };
        let probe = Placement::beside(screen, sidebar_width, popup_row, preferred_width, 24)?.outer;
        let local = Size {
            cols: probe.width,
            rows: probe.height,
            ..screen
        };
        let inner = content_rect(local);
        let mut wrapped = Vec::new();
        metadata::wrap(raw, inner.width, &mut wrapped);
        let wrapped_width = inner.width;
        let desired_height = wrapped
            .len()
            .saturating_add(if local.cols >= 3 && local.rows >= 3 {
                2
            } else {
                0
            })
            .clamp(1, 24) as u16;
        let rect = Placement::beside(
            screen,
            sidebar_width,
            popup_row,
            preferred_width,
            desired_height,
        )?
        .outer;
        let local = Size {
            cols: rect.width,
            rows: rect.height,
            ..screen
        };
        let inner = content_rect(local);
        if inner.width != wrapped_width {
            wrapped.clear();
            metadata::wrap(raw, inner.width, &mut wrapped);
        }
        let overflowing = wrapped.len() > usize::from(inner.height);
        // Keep a useful content row even in a one-row terminal band.
        let footer = overflowing && inner.height > 1;
        let body_height = usize::from(inner.height).saturating_sub(usize::from(footer));
        self.max_scroll = wrapped.len().saturating_sub(body_height);
        self.scroll = self.scroll.min(self.max_scroll);
        let mut lines = if self.graphical {
            Vec::new()
        } else {
            window::border_rows(local, " Details ", Border::Rounded)
        };
        self.roles = window::theme_spans(local).to_vec();
        self.projects.clear();
        self.refresh_hits.clear();
        self.clear_links();
        if local.cols >= 3 && local.rows >= 3 {
            self.roles.push(RoleSpan {
                rect: Rect {
                    x: 1,
                    y: 0,
                    width: 9.min(local.cols - 2),
                    height: 1,
                },
                role: Role::Title,
            });
        }
        for (index, line) in wrapped
            .iter()
            .skip(self.scroll)
            .take(body_height)
            .enumerate()
        {
            self.roles.push(RoleSpan {
                rect: Rect {
                    x: inner.x,
                    y: inner.y + index as u16,
                    width: inner.width,
                    height: 1,
                },
                role: line.role,
            });
            self.roles.extend(line.spans.iter().map(|span| RoleSpan {
                rect: Rect {
                    x: inner.x + span.rect.x,
                    y: inner.y + index as u16,
                    ..span.rect
                },
                role: span.role,
            }));
            self.projects
                .extend(line.projects.iter().map(|span| ProjectSpan {
                    rect: Rect {
                        x: inner.x + span.rect.x,
                        y: inner.y + index as u16,
                        ..span.rect
                    },
                    slot: span.slot,
                }));
            if let Some((cells, url)) = &line.link {
                self.links
                    .push((inner.x, inner.y + index as u16, *cells, url.clone()));
            }
            if let Some((hit, target)) = &line.refresh {
                self.refresh_hits.push((
                    Rect {
                        x: rect.x + inner.x + hit.x,
                        y: rect.y + inner.y + index as u16,
                        ..*hit
                    },
                    target.clone(),
                ));
            }
            lines.push((
                Rect {
                    x: inner.x,
                    y: inner.y + index as u16,
                    width: inner.width,
                    height: 1,
                },
                line.text.clone(),
                false,
            ));
        }
        if footer {
            let end = (self.scroll + body_height).min(wrapped.len());
            let label = format!(
                "{}/{} {}{}",
                end,
                wrapped.len(),
                if self.scroll > 0 { "↑" } else { "" },
                if end < wrapped.len() { "↓" } else { "" }
            );
            let label = if inner.width < 5 {
                window::fit("↕", inner.width)
            } else {
                window::fit(&label, inner.width)
            };
            lines.push((
                Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width,
                    height: 1,
                },
                label,
                false,
            ));
        }
        self.surface.paint(local, lines, None);
        for (x, y, cells, url) in &self.links {
            for cell in self.surface.frame.rows[usize::from(*y)]
                .cells
                .iter_mut()
                .skip(usize::from(*x))
                .take(usize::from(*cells))
            {
                cell.hyperlink = Some(url.clone());
            }
        }
        self.rect = Some(rect);
        self.paint_key = Some(PaintKey {
            scroll: self.scroll,
            ..key
        });
        Some(rect)
    }
    /// Only a current, visibly painted owner/source may issue refresh. This is
    /// independent of active terminal focus and rejects stale session generations.
    fn refresh_candidate(
        &self,
        tree: &crate::tree::Tree,
        point: Option<(u16, u16)>,
    ) -> Option<(crate::tree::Owner, &bkpr_protocol::CiRefreshTarget)> {
        let key = self.content_key.as_ref()?;
        let session = tree.session(key.owner)?;
        if session.group.status != crate::controller::SessionStatus::Online {
            return None;
        }
        if key.registry_revision != session.registry.revision
            || crate::controller::tree_popup_target(tree, false).is_none_or(|target| {
                target.owner != key.owner
                    || match &target.node {
                        crate::tree::Node::Native(native) => key.target.as_ref() != Some(native),
                        _ => true,
                    }
            })
        {
            return None;
        }
        let mut targets = self
            .content
            .as_ref()?
            .rows
            .iter()
            .filter_map(|row| row.refresh.as_ref())
            .filter(|target| {
                point.is_none_or(|(x, y)| {
                    self.refresh_hits
                        .iter()
                        .any(|(rect, visible)| visible == *target && rect.contains(x, y))
                })
            });
        let target = targets.next()?;
        if targets.next().is_some() {
            return None;
        }
        let current = session
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == target.pane_id)?
            .project
            .as_ref()?;
        if !crate::ci::matches_target(current, target) {
            return None;
        }
        if self
            .refresh_pending
            .as_ref()
            .is_some_and(|(owner, pending)| *owner == key.owner && pending == target)
        {
            return None;
        }
        Some((key.owner, target))
    }
    pub(crate) fn refresh_available(&self, tree: &crate::tree::Tree) -> bool {
        self.refresh_candidate(tree, None).is_some()
    }
    pub(crate) fn refresh_at(
        &mut self,
        tree: &crate::tree::Tree,
        point: Option<(u16, u16)>,
    ) -> Option<(crate::tree::Owner, bkpr_protocol::CiRefreshTarget)> {
        let (owner, target) = self.refresh_candidate(tree, point)?;
        let result = (owner, target.clone());
        self.refresh_pending = Some(result.clone());
        Some(result)
    }
    /// A command receipt is not provider completion. Release only its exact
    /// pending action; actual Loading metadata continues to disable the button.
    pub(crate) fn refresh_receipt(
        &mut self,
        owner: crate::tree::Owner,
        target: &bkpr_protocol::CiRefreshTarget,
        known: bool,
    ) {
        if known
            && self
                .refresh_pending
                .as_ref()
                .is_some_and(|(pending_owner, pending)| {
                    *pending_owner == owner && pending == target
                })
        {
            self.refresh_pending = None;
        }
    }
    /// Screen-space opaque text body; the logical frame retains its complete
    /// outer rounded border for the plain fallback.
    pub(crate) fn body_rect(&self) -> Option<Rect> {
        let outer = self.rect?;
        let inner = content_rect(self.surface.frame.size);
        Some(Rect {
            x: outer.x + inner.x,
            y: outer.y + inner.y,
            ..inner
        })
    }

    pub(crate) fn apply_theme(&mut self, theme: &Theme, palette: &ProjectPalette) {
        self.foregrounds.clear();
        let muted = theme.ink(Role::Muted, false);
        let roles_len = self.roles.len();
        for project in &self.projects {
            let (role, foreground) = palette.ink(project.slot, muted);
            self.roles.push(RoleSpan {
                rect: project.rect,
                role,
            });
            self.foregrounds.push(ForegroundSpan {
                rect: project.rect,
                foreground,
            });
        }
        self.surface.apply_theme_with_colors(
            theme,
            Role::Panel,
            &self.roles,
            &self.foregrounds,
            &[],
        );
        self.roles.truncate(roles_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PaneInfo, PaneStatus};

    fn pane(id: &str, window: &str) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some("work".into()),
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
            harness_status_changed_at_ms: None,
        }
    }

    fn registry() -> RegistrySnapshot {
        RegistrySnapshot {
            revision: 1,
            workspaces: vec![bkpr_protocol::Workspace {
                id: "work".into(),
                name: "Work".into(),
                revision: 1,
                windows: vec![bkpr_protocol::Window {
                    id: "one".into(),
                    name: "One".into(),
                    revision: 1,
                    layout: None,
                }],
            }],
            panes: vec![pane("terminal", "one")],
        }
    }

    #[test]
    fn pane_and_group_popovers_resolve_the_exact_host_project_mapping() {
        use crate::controller::{Controller, SessionGroup, SessionStatus};
        use crate::project_colors::ProjectHost;
        use crate::tree::{Node, Owner, Session, Target, Tree};
        use std::sync::Arc;
        let mut registry = registry();
        registry.panes[0].project = Some(crate::ci::tests::project());
        let state = Controller::new(&registry);
        let registry = Arc::new(registry);
        let local = Owner {
            slot: 0,
            generation: 1,
        };
        let remote = Owner {
            slot: 1,
            generation: 7,
        };
        let session = |owner, project_host| {
            Arc::new(Session {
                owner,
                project_host,
                group: SessionGroup {
                    name: "Session".into(),
                    host: None,
                    status: SessionStatus::Online,
                    detail: String::new(),
                },
                registry: Arc::clone(&registry),
                rows: state.sidebar.rows.clone().into(),
                workspace: state.workspace.clone(),
                window: state.window.clone(),
                focused: state.focused.clone(),
                snapshots: Default::default(),
                statuses: Default::default(),
                history: Default::default(),
            })
        };
        let mut tree = Tree::new();
        tree.reconcile(
            vec![
                session(local, ProjectHost::Local),
                session(
                    remote,
                    ProjectHost::Remote {
                        destination: "builder".into(),
                        ssh_config: None,
                    },
                ),
            ],
            local,
        );
        let screen = Size {
            cols: 120,
            rows: 40,
            cell_width: 8,
            cell_height: 16,
        };
        let mut popup = Popover::new(screen);
        for native in [
            SidebarTarget::Pane {
                workspace: "work".into(),
                pane: "terminal".into(),
            },
            SidebarTarget::Workspace("work".into()),
            SidebarTarget::Window {
                workspace: "work".into(),
                window: "one".into(),
            },
        ] {
            let target = Target {
                owner: remote,
                node: Node::Native(native),
            };
            popup
                .paint_tree(&tree, Some(&target), 3, 28, screen)
                .unwrap();
            assert!(!popup.projects.is_empty());
            assert!(popup.projects.iter().all(|span| span.slot == Some(1)));
        }
    }
    #[test]
    fn unified_popup_uses_owner_generation_and_session_authority() {
        use crate::controller::{Controller, SessionGroup, SessionStatus};
        use crate::tree::{Node, Owner, Session, Target, Tree};
        use std::sync::Arc;
        let screen = Size {
            cols: 120,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        };
        let mut popup = Popover::new(screen);
        let mut tree = Tree::new();
        for generation in [1, 2] {
            let owner = Owner {
                slot: 0,
                generation,
            };
            let title = format!("owner-{generation}-pane");
            let mut registry = registry();
            registry.panes[0].title = title.clone();
            let state = Controller::new(&registry);
            let group = SessionGroup {
                name: format!("owner-{generation}-session"),
                host: (generation == 2).then(|| "remote.example".into()),
                status: SessionStatus::Online,
                detail: format!("owner-{generation}-detail"),
            };
            let session = Arc::new(Session {
                owner,
                project_host: Default::default(),
                group,
                registry: Arc::new(registry),
                rows: state.sidebar.rows.clone().into(),
                workspace: state.workspace,
                window: state.window,
                focused: state.focused,
                snapshots: Arc::new(std::collections::HashMap::from([(
                    "terminal".into(),
                    snapshot(),
                )])),
                statuses: Default::default(),
                history: Default::default(),
            });
            tree.reconcile(vec![session], owner);
            let target = Target {
                owner,
                node: Node::Native(SidebarTarget::Pane {
                    workspace: "work".into(),
                    pane: "terminal".into(),
                }),
            };
            assert!(
                popup
                    .paint_tree(&tree, Some(&target), 3, 38, screen)
                    .is_some()
            );
            let rows = &popup.content.as_ref().unwrap().rows;
            assert!(rows.iter().any(|row| row.text.contains(&title)));
        }
        let session_target = Target {
            owner: Owner {
                slot: 0,
                generation: 2,
            },
            node: Node::Session,
        };
        assert!(
            popup
                .paint_tree(&tree, Some(&session_target), 2, 38, screen)
                .is_some()
        );
        let rows = &popup.content.as_ref().unwrap().rows;
        assert!(rows.iter().any(|row| row.text == "owner-2-detail"));
        assert!(!rows.iter().any(|row| row.text.contains("owner-2-pane")));
        let stale = Target {
            owner: Owner {
                slot: 0,
                generation: 1,
            },
            node: Node::Session,
        };
        assert!(
            popup
                .paint_tree(&tree, Some(&stale), 2, 38, screen)
                .is_none()
        );
    }

    fn snapshot() -> PaneSnapshot {
        PaneSnapshot {
            pane_id: "terminal".into(),
            pid: Some(41),
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
            program_identity: Some(bkpr_protocol::ProcessIdentity {
                pid: 41,
                birth: "birth".into(),
            }),
            events: Vec::new(),
            event_sequence: 0,
            size_owner: None,
            lease_generation: 0,
            views: Vec::new(),
            clipboard: None,
            clipboard_pending: false,
            capabilities: bkpr_protocol::PaneCapabilities {
                sixel: false,
                normalized_frames: false,
                native_mouse_reporting: false,
                clipboard_timeout_ms: 0,
                view_timeout_ms: 0,
                poll_wait_max_ms: 0,
                row_deltas: false,
                scene_encodings: Vec::new(),
            },
            selection: None,
            search: None,
        }
    }

    #[test]
    fn workspace_qualification_rejects_foreign_or_stale_panes() {
        let registry = RegistrySnapshot {
            revision: 1,
            panes: vec![],
            workspaces: vec![bkpr_protocol::Workspace {
                id: "one".into(),
                name: "One".into(),
                revision: 1,
                windows: vec![bkpr_protocol::Window {
                    id: "window".into(),
                    name: "Window".into(),
                    revision: 1,
                    layout: Some(bkpr_protocol::LayoutNode::Leaf {
                        pane_id: "gone".into(),
                    }),
                }],
            }],
        };
        assert!(
            build_content(
                &registry,
                &SidebarTarget::Workspace("one".into()),
                &ProjectSlots::default(),
                |_| None,
            )
            .is_some()
        );
        assert!(
            build_content(
                &registry,
                &SidebarTarget::Pane {
                    workspace: "one".into(),
                    pane: "gone".into()
                },
                &ProjectSlots::default(),
                |_| None
            )
            .is_none()
        );
        assert!(
            build_content(
                &registry,
                &SidebarTarget::Workspace("gone".into()),
                &ProjectSlots::default(),
                |_| None,
            )
            .is_none()
        );
    }

    #[test]
    fn window_metadata_excludes_other_windows_in_the_same_workspace() {
        let registry = RegistrySnapshot {
            revision: 1,
            workspaces: vec![bkpr_protocol::Workspace {
                id: "work".into(),
                name: "Work".into(),
                revision: 1,
                windows: ["one", "two"]
                    .into_iter()
                    .map(|id| bkpr_protocol::Window {
                        id: id.into(),
                        name: id.into(),
                        revision: 1,
                        layout: None,
                    })
                    .collect(),
            }],
            panes: vec![pane("first", "one"), pane("second", "two")],
        };
        let observed = std::cell::RefCell::new(Vec::new());
        let target = SidebarTarget::Window {
            workspace: "work".into(),
            window: "two".into(),
        };
        let content = build_content(&registry, &target, &ProjectSlots::default(), |pane| {
            observed.borrow_mut().push(pane.to_owned());
            None
        })
        .unwrap();
        assert_eq!(content.rows[0].text, "two");
        assert!(content.rows.iter().any(|row| row.text == "second"));
        assert!(!content.rows.iter().any(|row| row.text.contains("first")));
        assert_eq!(*observed.borrow(), ["second"]);
        assert!(
            build_content(
                &registry,
                &SidebarTarget::Window {
                    workspace: "work".into(),
                    window: "gone".into(),
                },
                &ProjectSlots::default(),
                |_| None
            )
            .is_none()
        );
    }

    #[test]
    fn pane_metadata_shares_finder_groups_and_rejects_foreign_snapshot_authority() {
        let mut registry = registry();
        registry.panes[0].harness = Some(bkpr_protocol::HarnessInfo {
            kind: bkpr_protocol::HarnessKind::Codex,
            pid: 41,
            birth: "birth".into(),
            cwd: "/fixture".into(),
            managed: None,
        });
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: "terminal".into(),
        };
        let mut snapshot = snapshot();
        snapshot.error = Some("Current engine error".into());
        let popup = build_content(&registry, &target, &ProjectSlots::default(), |_| {
            Some(&snapshot)
        })
        .unwrap();
        let finder = metadata::content(&registry.panes[0], &registry, Some(&snapshot), false, None);
        assert_eq!(
            popup
                .rows
                .iter()
                .map(|row| (&row.text, row.role))
                .collect::<Vec<_>>(),
            finder
                .iter()
                .map(|row| (&row.text, row.role))
                .collect::<Vec<_>>(),
        );
        assert!(popup.rows.iter().any(|row| row.text.contains("Codex")));
        assert!(
            popup
                .rows
                .iter()
                .any(|row| row.role == Role::Error && row.text.contains("engine error"))
        );
        snapshot.pane_id = "foreign".into();
        let popup = build_content(&registry, &target, &ProjectSlots::default(), |_| {
            Some(&snapshot)
        })
        .unwrap();
        assert!(
            !popup
                .rows
                .iter()
                .any(|row| row.text.contains("Codex") || row.text.contains("engine error"))
        );
        snapshot.pane_id = "terminal".into();
        snapshot.program_identity.as_mut().unwrap().birth = "previous".into();
        let popup = build_content(&registry, &target, &ProjectSlots::default(), |_| {
            Some(&snapshot)
        })
        .unwrap();
        assert!(!popup.rows.iter().any(|row| row.text.contains("Codex")));
    }

    #[test]
    fn held_error_changes_invalidate_paint_and_tiny_geometry_stays_inside_outer() {
        let registry = registry();
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: "terminal".into(),
        };
        let screen = Size {
            cols: 100,
            rows: 40,
            cell_width: 8,
            cell_height: 16,
        };
        let mut snapshot = snapshot();
        let mut popup = Popover::new(screen);
        popup
            .paint_owned(
                Source {
                    owner: crate::tree::Owner {
                        slot: 0,
                        generation: 1,
                    },
                    registry: &registry,
                    projects: &ProjectSlots::default(),
                },
                Some(&target),
                2,
                24,
                screen,
                |_| Some(&snapshot),
            )
            .unwrap();
        snapshot.error = Some("Connection failed".into());
        popup
            .paint_owned(
                Source {
                    owner: crate::tree::Owner {
                        slot: 0,
                        generation: 1,
                    },
                    registry: &registry,
                    projects: &ProjectSlots::default(),
                },
                Some(&target),
                2,
                24,
                screen,
                |_| Some(&snapshot),
            )
            .unwrap();
        let text = popup
            .surface
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
            .join("\n");
        assert!(text.contains("Connection failed"));
        for (cols, rows, anchor) in [(1, 3, 1), (2, 4, 0), (5, 2, 0), (24, 8, 2)] {
            let screen = Size {
                cols,
                rows,
                ..screen
            };
            let outer = popup
                .paint_owned(
                    Source {
                        owner: crate::tree::Owner {
                            slot: 0,
                            generation: 1,
                        },
                        registry: &registry,
                        projects: &ProjectSlots::default(),
                    },
                    Some(&target),
                    anchor,
                    cols,
                    screen,
                    |_| Some(&snapshot),
                )
                .unwrap();
            let body = popup.body_rect().unwrap();
            assert_eq!(popup.surface.frame.size.cols, outer.width);
            assert_eq!(popup.surface.frame.size.rows, outer.height);
            assert!(body.x >= outer.x && body.y >= outer.y);
            assert!(body.x + body.width <= outer.x + outer.width);
            assert!(body.y + body.height <= outer.y + outer.height);
            assert!(outer.x + outer.width <= screen.cols && outer.y + outer.height <= screen.rows);
        }
        popup.hide();
        assert!(popup.body_rect().is_none());
    }

    #[test]
    fn canonical_project_groups_deduplicate_subdirectories_but_keep_worktree_freshness() {
        use bkpr_protocol::{ChangeSummary, ProjectSnapshot, ProjectState, RepositoryInfo};
        let project = |cwd: &str, root: &str, state| ProjectSnapshot {
            ci: None,
            cwd: cwd.into(),
            state,
            error: None,
            repository: Some(RepositoryInfo {
                name: "same-project".into(),
                root: root.into(),
                common_dir: "/repo/.git".into(),
                git_dir: format!("{root}/.git").into(),
                branch: Some(root.into()),
                head_sha: Some("abcdef".into()),
                staged: ChangeSummary::default(),
                unstaged: ChangeSummary::default(),
                untracked_files: 0,
                base_ref: None,
                base_sha: None,
                committed: None,
            }),
        };
        let mut registry = registry();
        registry.panes[0].project = Some(project("/tree/a", "/tree/a", ProjectState::Ready));
        let mut duplicate = pane("duplicate", "one");
        duplicate.project = Some(project("/tree/a/subdir", "/tree/a", ProjectState::Ready));
        let mut other = pane("other", "one");
        other.project = Some(project("/tree/b", "/tree/b", ProjectState::Loading));
        registry.panes.extend([duplicate, other]);
        let content = build_content(
            &registry,
            &SidebarTarget::Workspace("work".into()),
            &ProjectSlots::default(),
            |_| None,
        )
        .unwrap();
        assert_eq!(
            content
                .rows
                .iter()
                .filter(|row| row.text == "same-project")
                .count(),
            1
        );
        assert_eq!(
            content
                .rows
                .iter()
                .filter(|row| row.text.starts_with("Worktree · "))
                .count(),
            2
        );
        registry
            .panes
            .last_mut()
            .unwrap()
            .project
            .as_mut()
            .unwrap()
            .state = ProjectState::Ready;
        let refreshed = build_content(
            &registry,
            &SidebarTarget::Workspace("work".into()),
            &ProjectSlots::default(),
            |_| None,
        )
        .unwrap();
        assert_eq!(content.rows.len(), refreshed.rows.len());
        assert_eq!(
            content
                .rows
                .iter()
                .zip(&refreshed.rows)
                .filter(|(loading, ready)| loading.text != ready.text)
                .count(),
            1,
            "one worktree's freshness changes without replacing other worktree facts"
        );
    }
    #[test]
    fn beside_popup_aligns_its_first_content_row_and_keeps_tiny_anchor_visible() {
        let registry = registry();
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: "terminal".into(),
        };
        let screen = Size {
            cols: 100,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        };
        let mut popup = Popover::new(screen);
        for anchor in [2, 7, 12] {
            let outer = popup
                .paint_owned(
                    Source {
                        owner: crate::tree::Owner {
                            slot: 0,
                            generation: 1,
                        },
                        registry: &registry,
                        projects: &ProjectSlots::default(),
                    },
                    Some(&target),
                    anchor,
                    24,
                    screen,
                    |_| None,
                )
                .unwrap();
            assert_eq!(popup.body_rect().unwrap().y, anchor);
            assert!(outer.y + outer.height <= screen.rows);
        }
        let tiny = Size {
            cols: 3,
            rows: 6,
            ..screen
        };
        let outer = popup
            .paint_owned(
                Source {
                    owner: crate::tree::Owner {
                        slot: 0,
                        generation: 1,
                    },
                    registry: &registry,
                    projects: &ProjectSlots::default(),
                },
                Some(&target),
                3,
                3,
                tiny,
                |_| None,
            )
            .unwrap();
        assert!(outer.y > 3 || outer.y + outer.height <= 3);
    }
    #[test]
    fn graphical_padding_is_blank_and_rounded_fallback_returns_without_content_change() {
        let registry = registry();
        let target = SidebarTarget::Pane {
            workspace: "work".into(),
            pane: "terminal".into(),
        };
        let screen = Size {
            cols: 100,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        };
        let mut popup = Popover::new(screen);
        let mut original_content = None;
        for graphical in [false, true, false] {
            popup.set_graphical(graphical);
            popup
                .paint_owned(
                    Source {
                        owner: crate::tree::Owner {
                            slot: 0,
                            generation: 1,
                        },
                        registry: &registry,
                        projects: &ProjectSlots::default(),
                    },
                    Some(&target),
                    4,
                    24,
                    screen,
                    |_| None,
                )
                .unwrap();
            let frame = &popup.surface.frame;
            let top = &frame.rows[0].cells;
            if graphical {
                assert!(top.iter().all(|cell| cell.text == " "));
                assert!(frame.rows.iter().all(|row| row.cells[0].text == " "));
            } else {
                assert_eq!(top[0].text, "╭");
                assert_eq!(top.last().unwrap().text, "╮");
                assert_eq!(frame.rows.last().unwrap().cells[0].text, "╰");
            }
            let content = frame.rows[1..frame.rows.len() - 1]
                .iter()
                .map(|row| {
                    row.cells[1..row.cells.len() - 1]
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            if let Some(original) = &original_content {
                assert_eq!(&content, original);
            } else {
                original_content = Some(content);
            }
        }
    }
    #[test]
    fn ci_refresh_and_links_follow_exact_painted_owner_source_and_revision() {
        use crate::controller::{Controller, SessionGroup, SessionStatus};
        use crate::tree::{Node, Owner, Session, Target, Tree};
        use std::sync::Arc;
        let screen = Size {
            cols: 120,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        };
        let owner = Owner {
            slot: 3,
            generation: 7,
        };
        let target = Target {
            owner,
            node: Node::Native(SidebarTarget::Pane {
                workspace: "work".into(),
                pane: "terminal".into(),
            }),
        };
        let mut registry = registry();
        registry.panes[0].project = Some(crate::ci::tests::project());
        let session = |registry: RegistrySnapshot, owner| {
            let state = Controller::new(&registry);
            Arc::new(Session {
                owner,
                project_host: Default::default(),
                group: SessionGroup {
                    name: "Exact owner".into(),
                    host: None,
                    status: SessionStatus::Online,
                    detail: String::new(),
                },
                registry: Arc::new(registry),
                rows: state.sidebar.rows.clone().into(),
                workspace: state.workspace,
                window: state.window,
                focused: state.focused,
                snapshots: Default::default(),
                statuses: Default::default(),
                history: Default::default(),
            })
        };
        let mut tree = Tree::new();
        tree.reconcile(vec![session(registry.clone(), owner)], owner);
        tree.hover = Some(target.clone());
        let mut popup = Popover::new(screen);
        popup
            .paint_tree(&tree, Some(&target), 3, 28, screen)
            .unwrap();
        popup.scroll_by(i32::MAX);
        popup
            .paint_tree(&tree, Some(&target), 3, 28, screen)
            .unwrap();
        let hit = popup.refresh_hits[0].0;
        let accepted = popup.refresh_at(&tree, Some((hit.x, hit.y))).unwrap();
        assert_eq!(accepted.0, owner);
        assert_eq!(accepted.1.pane_id, "terminal");
        assert_eq!(accepted.1.head_sha, "remoteabcdef");
        assert!(popup.refresh_at(&tree, Some((hit.x, hit.y))).is_none());
        // A new URL with unchanged visible check labels must invalidate annotations.
        registry.revision += 1;
        registry.panes[0]
            .project
            .as_mut()
            .unwrap()
            .ci
            .as_mut()
            .unwrap()
            .checks[0]
            .url = Some("https://github.com/owner/repo/actions/runs/99".into());
        tree.reconcile(vec![session(registry.clone(), owner)], owner);
        tree.hover = Some(target.clone());
        assert!(popup.refresh_at(&tree, Some((hit.x, hit.y))).is_none());
        popup
            .paint_tree(&tree, Some(&target), 3, 28, screen)
            .unwrap();
        assert!(
            popup
                .surface
                .frame
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.hyperlink.as_deref()
                    == Some("https://github.com/owner/repo/actions/runs/99"))
        );
        assert!(
            !popup
                .surface
                .frame
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.hyperlink.as_deref()
                    == Some("https://github.com/owner/repo/actions/runs/7"))
        );
        registry.revision += 1;
        registry.panes[0]
            .project
            .as_mut()
            .unwrap()
            .ci
            .as_mut()
            .unwrap()
            .checks[0]
            .url = Some("javascript:invalid".into());
        tree.reconcile(vec![session(registry.clone(), owner)], owner);
        tree.hover = Some(target.clone());
        popup
            .paint_tree(&tree, Some(&target), 3, 28, screen)
            .unwrap();
        assert!(
            !popup
                .surface
                .frame
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.hyperlink.as_deref()
                    == Some("https://github.com/owner/repo/actions/runs/99"))
        );
        // Same labels and styles, different exact source: old refresh cannot win.
        registry.revision += 1;
        registry.panes[0].project.as_mut().unwrap().cwd = "/fixture/repo/subdirectory".into();
        tree.reconcile(vec![session(registry.clone(), owner)], owner);
        tree.hover = Some(target.clone());
        assert!(popup.refresh_at(&tree, Some((hit.x, hit.y))).is_none());
        popup
            .paint_tree(&tree, Some(&target), 3, 28, screen)
            .unwrap();
        let refreshed = popup.refresh_at(&tree, None).unwrap().1;
        assert_eq!(
            refreshed.cwd,
            std::path::Path::new("/fixture/repo/subdirectory")
        );
        assert_eq!(refreshed.head_sha, "remoteabcdef");
        let replacement = Owner {
            generation: 8,
            ..owner
        };
        tree.reconcile(vec![session(registry, replacement)], replacement);
        assert!(popup.refresh_at(&tree, Some((hit.x, hit.y))).is_none());
        tree.hover = Some(target);
        assert!(popup.refresh_at(&tree, Some((hit.x, hit.y))).is_none());
    }

    #[test]
    fn workspace_ci_deduplicates_provider_sources_and_requests_but_keeps_local_contexts() {
        let mut registry = registry();
        registry.panes[0].project = Some(crate::ci::tests::project());
        let mut duplicate = registry.panes[0].clone();
        duplicate.id = "duplicate".into();
        duplicate
            .project
            .as_mut()
            .unwrap()
            .repository
            .as_mut()
            .unwrap()
            .root = "/fixture/linked".into();
        registry.panes.push(duplicate);
        let content = build_content(
            &registry,
            &SidebarTarget::Workspace("work".into()),
            &ProjectSlots::default(),
            |_| None,
        )
        .unwrap();
        let text = content
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("Restore project metadata").count(), 1);
        assert_eq!(text.matches("CI ·").count(), 1);
        assert_eq!(text.matches("Worktree ·").count(), 2);
        assert_eq!(
            content
                .rows
                .iter()
                .filter(|row| row.refresh.is_some())
                .count(),
            1
        );
        assert!(!text.contains("unit tests"));
        let ci = registry.panes[1]
            .project
            .as_mut()
            .unwrap()
            .ci
            .as_mut()
            .unwrap();
        ci.head_sha = "otherremote".into();
        let request = ci.change_request.as_mut().unwrap();
        request.number = 43;
        request.url = "https://github.com/owner/repo/pull/43".into();
        let content = build_content(
            &registry,
            &SidebarTarget::Workspace("work".into()),
            &ProjectSlots::default(),
            |_| None,
        )
        .unwrap();
        let text = content
            .rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("#42").count(), 1);
        assert_eq!(text.matches("#43").count(), 1);
        assert_eq!(text.matches("CI ·").count(), 2);
        assert_eq!(
            content
                .rows
                .iter()
                .filter(|row| row.refresh.is_some())
                .count(),
            2
        );
    }
    #[test]
    fn fast_command_receipts_and_known_rejections_release_only_their_exact_pending_action() {
        let screen = Size {
            cols: 100,
            rows: 40,
            cell_width: 9,
            cell_height: 18,
        };
        let mut popup = Popover::new(screen);
        let owner = crate::tree::Owner {
            slot: 0,
            generation: 4,
        };
        let target = crate::ci::target(&crate::ci::tests::project(), "exact-pane").unwrap();
        for _known_outcome in ["queued", "known admission failure"] {
            popup.refresh_pending = Some((owner, target.clone()));
            popup.refresh_receipt(owner, &target, true);
            assert!(popup.refresh_pending.is_none());
        }
        popup.refresh_pending = Some((owner, target.clone()));
        popup.refresh_receipt(owner, &target, false);
        assert!(
            popup.refresh_pending.is_some(),
            "ambiguous dispatch cannot authorize a replay"
        );
        let retired = crate::tree::Owner {
            generation: 3,
            ..owner
        };
        popup.refresh_receipt(retired, &target, true);
        assert!(popup.refresh_pending.is_some());
        let mut wrong_source = target.clone();
        wrong_source.local_head_sha = "another-local-head".into();
        popup.refresh_receipt(owner, &wrong_source, true);
        assert!(popup.refresh_pending.is_some());
        popup.refresh_receipt(owner, &target, true);
        assert!(popup.refresh_pending.is_none());
    }
}
