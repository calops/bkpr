//! Passive sidebar metadata projected into the existing single scene compositor.
//! Inspired by Bkpr main's MIT-licensed ui.rs tooltip model; this implementation
//! uses only pane-first protocol facts, without the old Zellij overlay transport.
use crate::controller::SidebarTarget;
use bkpr_protocol::{
    LayoutNode, PaneInfo, PaneSnapshot, PaneStatus, ProjectSnapshot, RegistrySnapshot, Size,
};
use bkpr_tui_framework::{
    Rect, UiSurface,
    window::{self, Border},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SURFACE_ID: &str = "ui:tooltip";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub rows: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub title: String,
    pub subtitle: String,
    pub sections: Vec<Section>,
}

fn projected(layout: Option<&LayoutNode>, pane: &str) -> bool {
    match layout {
        Some(LayoutNode::Leaf { pane_id }) => pane_id == pane,
        Some(LayoutNode::Split { first, second, .. }) => {
            projected(Some(first), pane) || projected(Some(second), pane)
        }
        None => false,
    }
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
        SidebarTarget::Pane { pane, .. } => registry.panes.iter().any(|p| {
            p.id == *pane
                && (p.workspace_id.as_deref() == Some(workspace.id.as_str())
                    || projected(workspace.layout.as_ref(), pane))
        }),
    }
}
fn status(pane: &PaneInfo) -> String {
    match pane.status {
        PaneStatus::Running => "Running".into(),
        PaneStatus::Stopped => "Stopped".into(),
        PaneStatus::Exited => pane
            .exit_code
            .map(|code| format!("Exited ({code})"))
            .unwrap_or_else(|| "Exited".into()),
    }
}
fn project_section(project: &ProjectSnapshot) -> Section {
    let rows = crate::controller::project_details(Some(project));
    Section {
        title: project
            .repository
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "Project".into()),
        rows,
    }
}

/// Snapshot is optional and must already be held by the frontend. No attachment,
/// filesystem discovery, forge requests, or hidden-pane reads are performed.
pub fn build_content<'a>(
    registry: &RegistrySnapshot,
    target: &SidebarTarget,
    snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
) -> Option<Content> {
    if !belongs(registry, target) {
        return None;
    }
    let workspace = registry
        .workspaces
        .iter()
        .find(|w| w.id == target.workspace())?;
    match target {
        SidebarTarget::Workspace(_) => {
            let panes: Vec<_> = registry
                .panes
                .iter()
                .filter(|p| {
                    p.workspace_id.as_deref() == Some(workspace.id.as_str())
                        || projected(workspace.layout.as_ref(), &p.id)
                })
                .collect();
            let mut sections = vec![Section {
                title: "Terminals".into(),
                rows: panes
                    .iter()
                    .map(|p| {
                        let known = snapshot(&p.id);
                        let harness = crate::controller::recognized_harness(p, known)
                            .map(|h| format!(" · {}", h.kind.display_name()))
                            .unwrap_or_default();
                        let activity = crate::controller::harness_status(p, known)
                            .map(|s| format!(" · {}", s.state))
                            .unwrap_or_default();
                        format!("{} · {}{harness}{activity}", p.title, status(p))
                    })
                    .collect(),
            }];
            if panes.is_empty() {
                sections[0]
                    .rows
                    .push("No terminals in this workspace".into());
            }
            let mut projects: Vec<&ProjectSnapshot> = Vec::new();
            for pane in &panes {
                if let Some(project) = &pane.project
                    && !projects
                        .iter()
                        .any(|old| match (&old.repository, &project.repository) {
                            (Some(a), Some(b)) => a.root == b.root,
                            _ => old.cwd == project.cwd,
                        })
                {
                    projects.push(project);
                }
            }
            for project in projects {
                sections.push(project_section(project));
            }
            if sections.len() == 1 {
                sections.push(Section {
                    title: "Project".into(),
                    rows: vec!["No local project metadata available".into()],
                });
            }
            Some(Content {
                title: workspace.name.clone(),
                subtitle: format!("Workspace · {} terminals", panes.len()),
                sections,
            })
        }
        SidebarTarget::Pane { pane, .. } => {
            let pane = registry.panes.iter().find(|p| p.id == *pane)?;
            let known = snapshot(&pane.id);
            let snapshot = known.filter(|s| s.pane_id == pane.id);
            let mut rows = vec![
                format!("Status: {}", status(pane)),
                format!("Pane: {}", pane.id),
                format!(
                    "CWD: {}",
                    pane.foreground_cwd.as_ref().unwrap_or(&pane.cwd).display()
                ),
            ];
            if let Some(snapshot) = snapshot {
                rows.push(format!("Lifecycle: {}", snapshot.lifecycle));
                if let Some(pid) = snapshot.pid {
                    rows.push(format!("PID: {pid}"));
                }
                if let Some(error) = &snapshot.error {
                    rows.push(format!("Error: {error}"));
                }
            }
            let mut sections = vec![Section {
                title: "Terminal".into(),
                rows,
            }];
            if let Some(harness) = crate::controller::recognized_harness(pane, known) {
                let mut rows = vec![
                    format!("Harness: {}", harness.kind.display_name()),
                    format!("PID: {}", harness.pid),
                    format!("CWD: {}", harness.cwd.display()),
                ];
                if let Some(managed) = &harness.managed {
                    rows.push(format!("Agent: {}", managed.launch_id));
                    if let Some(conversation) = &managed.conversation {
                        rows.push(format!("Conversation: {}", conversation.id));
                    }
                    if let Some(error) = &managed.error {
                        rows.push(format!("Capture: {error}"));
                    }
                }
                sections.push(Section {
                    title: "Harness".into(),
                    rows,
                });
            }
            if let Some(activity) = crate::controller::harness_status(pane, known) {
                let mut rows = vec![format!("State: {}", activity.state)];
                if !activity.title.is_empty() {
                    rows.push(activity.title.clone());
                }
                if let Some(message) = &activity.message {
                    rows.push(message.clone());
                }
                if let Some(progress) = activity.progress {
                    rows.push(format!("Progress: {progress}%"));
                }
                sections.push(Section {
                    title: "Activity".into(),
                    rows,
                });
            }
            if let Some(project) = &pane.project {
                sections.push(project_section(project));
            }
            Some(Content {
                title: pane.title.clone(),
                subtitle: format!("Terminal · {}", workspace.name),
                sections,
            })
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ContentKey {
    target: SidebarTarget,
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
        }
    }
    pub fn hide(&mut self) {
        self.content_key = None;
        self.content = None;
        self.paint_key = None;
        self.rect = None;
        self.scroll = 0;
        self.max_scroll = 0;
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
    /// Return screen placement; frame coordinates are local to that rect. Add
    /// Surface { id: SURFACE_ID, rect, frame: &self.surface.frame, focused: false,
    /// selection: &[], search: &[] } after pane/chrome surfaces, before modals.
    pub fn paint<'a>(
        &mut self,
        registry: &RegistrySnapshot,
        target: Option<&SidebarTarget>,
        anchor_row: u16,
        sidebar_width: u16,
        screen: Size,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    ) -> Option<Rect> {
        let Some(target) = target.filter(|target| belongs(registry, target)) else {
            self.hide();
            return None;
        };
        let unchanged = self.content_key.as_ref().is_some_and(|key| {
            key.target == *target
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
            if self.content_key.as_ref().map(|key| &key.target) != Some(target) {
                self.scroll = 0;
            }
            self.content = build_content(registry, target, &snapshot);
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
                            || projected(workspace.layout.as_ref(), &pane.id)
                    }
                })
                .map(|pane| (pane.id.clone(), snapshot(&pane.id).map(SnapshotKey::new)))
                .collect();
            self.content_key = Some(ContentKey {
                target: target.clone(),
                registry_revision: registry.revision,
                snapshots,
            });
            self.paint_key = None;
        }
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
        let raw = content_rows(content);
        let preferred_width = raw
            .iter()
            .map(|(line, _)| UnicodeWidthStr::width(line.as_str()))
            .max()
            .unwrap_or(1)
            .saturating_add(2)
            .clamp(24, 64) as u16;
        let probe = window::beside(screen, sidebar_width, anchor_row, preferred_width, 24)?;
        let local = Size {
            cols: probe.width,
            rows: probe.height,
            ..screen
        };
        let inner = window::content(local);
        let wrapped = wrap_rows(&raw, inner.width);
        let desired_height = wrapped
            .len()
            .saturating_add(if local.cols >= 3 && local.rows >= 3 {
                2
            } else {
                0
            })
            .clamp(1, 24) as u16;
        let rect = window::beside(
            screen,
            sidebar_width,
            anchor_row,
            preferred_width,
            desired_height,
        )?;
        let local = Size {
            cols: rect.width,
            rows: rect.height,
            ..screen
        };
        let inner = window::content(local);
        let wrapped = wrap_rows(&raw, inner.width);
        let overflowing = wrapped.len() > usize::from(inner.height);
        // Keep a useful content row even in a one-row terminal band.
        let footer = overflowing && inner.height > 1;
        let body_height = usize::from(inner.height).saturating_sub(usize::from(footer));
        self.max_scroll = wrapped.len().saturating_sub(body_height);
        self.scroll = self.scroll.min(self.max_scroll);
        let mut lines = window::border_rows(local, " Details ", Border::Rounded);
        for (index, (label, heading)) in wrapped
            .iter()
            .skip(self.scroll)
            .take(body_height)
            .enumerate()
        {
            lines.push((
                Rect {
                    x: inner.x,
                    y: inner.y + index as u16,
                    width: inner.width,
                    height: 1,
                },
                label.clone(),
                *heading,
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
        self.rect = Some(rect);
        self.paint_key = Some(PaintKey {
            scroll: self.scroll,
            ..key
        });
        Some(rect)
    }
}

fn content_rows(content: &Content) -> Vec<(String, bool)> {
    let mut rows = vec![
        (content.title.clone(), true),
        (content.subtitle.clone(), false),
    ];
    for section in &content.sections {
        rows.push((String::new(), false));
        rows.push((section.title.clone(), true));
        rows.extend(section.rows.iter().cloned().map(|row| (row, false)));
    }
    rows
}
fn wrap_rows(rows: &[(String, bool)], width: u16) -> Vec<(String, bool)> {
    if width == 0 {
        return Vec::new();
    }
    let mut output = Vec::new();
    for (value, heading) in rows {
        let mut line = String::new();
        let mut columns = 0;
        for ch in window::clean(value).chars() {
            let advance = UnicodeWidthChar::width(ch).unwrap_or(0);
            if advance > usize::from(width) {
                // A two-cell glyph cannot fit in a one-cell viewport. Mark its
                // omission rather than silently losing metadata or spilling out.
                if !line.is_empty() {
                    output.push((std::mem::take(&mut line), *heading));
                }
                output.push(("…".into(), *heading));
                columns = 0;
                continue;
            }
            if columns + advance > usize::from(width) {
                output.push((std::mem::take(&mut line), *heading));
                columns = 0;
            }
            if advance > 0 || !line.is_empty() {
                line.push(ch);
            }
            columns += advance;
        }
        if !line.is_empty() || value.is_empty() {
            output.push((line, *heading));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_preserves_combining_marks_and_marks_unfittable_glyphs() {
        assert_eq!(
            wrap_rows(&[("界e\u{301}x".into(), false)], 3),
            vec![("界e\u{301}".into(), false), ("x".into(), false)]
        );
        assert_eq!(
            wrap_rows(&[("界a".into(), true)], 1),
            vec![("…".into(), true), ("a".into(), true)]
        );
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
                layout: Some(LayoutNode::Leaf {
                    pane_id: "gone".into(),
                }),
            }],
        };
        assert!(
            build_content(&registry, &SidebarTarget::Workspace("one".into()), |_| None).is_some()
        );
        assert!(
            build_content(
                &registry,
                &SidebarTarget::Pane {
                    workspace: "one".into(),
                    pane: "gone".into()
                },
                |_| None
            )
            .is_none()
        );
        assert!(
            build_content(&registry, &SidebarTarget::Workspace("gone".into()), |_| {
                None
            })
            .is_none()
        );
    }
}
