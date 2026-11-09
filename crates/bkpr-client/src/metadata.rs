//! Shared Bkpr pane metadata, based on main 9ad93bd809f8fb0dbd0c28bcbc267a682a21430b
//! ui.rs::PopupContent/PopupRow/tooltip (Copyright (c) 2026 Rémi Labeyrie, MIT).
//! Only supplied protocol facts are projected; no terminal or provider reads.
use bkpr_protocol::{
    ChangeSummary, PaneInfo, PaneSnapshot, PaneStatus, ProjectSnapshot, ProjectState,
    RegistrySnapshot,
};
use bkpr_tui_framework::{Rect, Role, RoleSpan};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
enum Ink {
    Role(Role),
    Project(Option<u32>),
}

#[derive(Clone, Copy)]
pub(crate) struct ProjectSpan {
    pub rect: Rect,
    pub slot: Option<u32>,
}

pub(crate) struct Paragraph {
    pub text: String,
    pub role: Role,
    spans: Vec<(Range<usize>, Ink)>,
    right: Vec<(String, Role)>,
    truncate: bool,
}
pub(crate) struct PreviewLine {
    pub text: String,
    pub role: Role,
    pub spans: Vec<RoleSpan>,
    pub projects: Vec<ProjectSpan>,
}

impl Paragraph {
    pub(crate) fn new(text: impl AsRef<str>, role: Role) -> Self {
        Self {
            text: clean(text.as_ref()),
            role,
            spans: Vec::new(),
            right: Vec::new(),
            truncate: false,
        }
    }
    pub(crate) fn project(text: impl AsRef<str>, role: Role, slot: Option<u32>) -> Self {
        let mut paragraph = Self::new(text, role);
        paragraph
            .spans
            .push((0..paragraph.text.len(), Ink::Project(slot)));
        paragraph
    }
    fn append(&mut self, text: impl AsRef<str>, role: Role) {
        let start = self.text.len();
        self.text.push_str(&clean(text.as_ref()));
        self.spans.push((start..self.text.len(), Ink::Role(role)));
    }
    fn right(&mut self, text: impl AsRef<str>, role: Role) {
        self.right.push((clean(text.as_ref()), role));
        self.truncate = true;
    }
}

pub(crate) fn clean(text: &str) -> String {
    if text.chars().any(|c| c.is_whitespace() && c != ' ') {
        let normalized: String = text
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        bkpr_tui_framework::window::clean(&normalized)
    } else {
        bkpr_tui_framework::window::clean(text)
    }
}
fn push(rows: &mut Vec<Paragraph>, text: impl AsRef<str>, role: Role) {
    rows.push(Paragraph::new(text, role));
}
fn heading(rows: &mut Vec<Paragraph>, title: &str) {
    push(rows, "", Role::Panel);
    push(rows, title, Role::Title);
}
pub(crate) fn activity_role(state: &str) -> Role {
    match state {
        "working" => Role::Info,
        "done" => Role::Success,
        "blocked" => Role::Warning,
        "error" => Role::Error,
        _ => Role::Muted,
    }
}

pub(crate) fn content(
    pane: &PaneInfo,
    registry: &RegistrySnapshot,
    known: Option<&PaneSnapshot>,
) -> Vec<Paragraph> {
    let harness = crate::controller::recognized_harness(pane, known);
    let activity = crate::controller::harness_status(pane, known);
    let mut rows = Vec::new();
    push(
        &mut rows,
        if pane.title.is_empty() {
            "Untitled terminal"
        } else {
            &pane.title
        },
        Role::Title,
    );
    rows.push(pane_summary(pane, known, false));
    if let Some(workspace) = registry
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == pane.workspace_id.as_ref())
    {
        let window = workspace
            .windows
            .iter()
            .find(|w| Some(&w.id) == pane.window_id.as_ref());
        push(
            &mut rows,
            window.map_or_else(
                || workspace.name.clone(),
                |window| format!("{} · {}", workspace.name, window.name),
            ),
            Role::Muted,
        );
    }
    push(
        &mut rows,
        pane.foreground_cwd
            .as_ref()
            .unwrap_or(&pane.cwd)
            .to_string_lossy(),
        Role::Muted,
    );
    if let Some(activity) = activity {
        heading(&mut rows, "Activity");
        push(&mut rows, &activity.state, activity_role(&activity.state));
        if !activity.title.is_empty() {
            push(&mut rows, &activity.title, Role::Panel);
        }
        if let Some(message) = &activity.message {
            push(&mut rows, message, Role::Panel);
        }
        if let Some(progress) = activity.progress {
            push(&mut rows, format!("Progress · {progress}%"), Role::Info);
        }
    }
    if let Some(error) = known
        .filter(|snapshot| snapshot.pane_id == pane.id)
        .and_then(|snapshot| snapshot.error.as_ref())
    {
        heading(&mut rows, "Terminal");
        push(&mut rows, error, Role::Error);
    }
    if let Some(error) = harness
        .and_then(|h| h.managed.as_ref())
        .and_then(|m| m.error.as_ref())
    {
        heading(&mut rows, "Capture");
        push(&mut rows, error, Role::Error);
    }
    heading(&mut rows, "Repository");
    project_content(pane.project.as_ref(), true, &mut rows);
    heading(&mut rows, "Checks & tickets");
    push(
        &mut rows,
        "Forge, CI and ticket metadata are not available",
        Role::Muted,
    );
    rows
}

pub(crate) fn project_content(
    project: Option<&ProjectSnapshot>,
    include_name: bool,
    rows: &mut Vec<Paragraph>,
) {
    let Some(project) = project else {
        push(rows, "Local project metadata unavailable", Role::Muted);
        return;
    };
    // Keep one freshness row even for ready data. A refresh changes this row,
    // not the identity, semantic colors or positions of last-known facts.
    let (status, role) = match (project.state, project.repository.is_some()) {
        (ProjectState::Ready, _) => ("Git: current", Role::Muted),
        (ProjectState::Loading, true) => ("Git: stale · refreshing", Role::Warning),
        (ProjectState::Loading, false) => ("Git: loading", Role::Muted),
        (ProjectState::Error, true) => ("Git: stale · refresh failed", Role::Error),
        (ProjectState::Error, false) => ("Git: unavailable", Role::Error),
        (ProjectState::NotRepository, true) => ("Git: stale · not a repository", Role::Warning),
        (ProjectState::NotRepository, false) => {
            ("Local directory · not a Git repository", Role::Muted)
        }
    };
    let mut status = Paragraph::new(status, role);
    status.truncate = true;
    rows.push(status);
    if let Some(error) = &project.error {
        push(rows, error, Role::Error);
    }
    let Some(repository) = &project.repository else {
        if project.state == ProjectState::Ready {
            push(rows, "Repository details unavailable", Role::Muted);
        }
        return;
    };
    if include_name {
        rows.push(Paragraph::project(
            &repository.name,
            Role::Accent,
            project.color_slot,
        ));
    }
    push(
        rows,
        format!("Worktree · {}", repository.root.display()),
        Role::Muted,
    );
    let branch = match (&repository.branch, &repository.head_sha) {
        (Some(branch), _) => format!("Branch · {branch}"),
        (None, Some(sha)) => format!(
            "Detached HEAD · {}",
            sha.chars().take(10).collect::<String>()
        ),
        (None, None) => "Branch unavailable · no HEAD commit".to_owned(),
    };
    let mut branch = Paragraph::new(branch, Role::Panel);
    branch.truncate = true;
    rows.push(branch);
    heading(rows, "Worktree changes");
    if repository.staged == ChangeSummary::default()
        && repository.unstaged == ChangeSummary::default()
        && repository.untracked_files == 0
    {
        push(rows, "Clean working tree", Role::Success);
    } else {
        change_row(rows, "Staged", &repository.staged);
        change_row(rows, "Unstaged", &repository.unstaged);
        push(
            rows,
            format!("Untracked · {} files", repository.untracked_files),
            Role::Muted,
        );
    }
    heading(rows, "Branch changes");
    match (&repository.base_ref, &repository.committed) {
        (Some(base), Some(summary)) => {
            push(rows, format!("HEAD vs merge base of {base}"), Role::Muted);
            change_row(rows, "Committed", summary);
        }
        _ => push(
            rows,
            "Comparison unavailable · no suitable local base",
            Role::Muted,
        ),
    }
}

pub(crate) fn pane_summary(
    pane: &PaneInfo,
    known: Option<&PaneSnapshot>,
    include_title: bool,
) -> Paragraph {
    let lifecycle = match pane.status {
        PaneStatus::Running => "Running".to_owned(),
        PaneStatus::Stopped => "Stopped".to_owned(),
        PaneStatus::Exited => pane
            .exit_code
            .map_or_else(|| "Exited".into(), |code| format!("Exited ({code})")),
    };
    let mut row = Paragraph::new(
        if include_title {
            if pane.title.is_empty() {
                "Untitled terminal"
            } else {
                &pane.title
            }
        } else {
            &lifecycle
        },
        if include_title {
            Role::Panel
        } else {
            Role::Muted
        },
    );
    if let Some(harness) = crate::controller::recognized_harness(pane, known) {
        row.append(" · ", Role::Muted);
        row.append(
            format!(
                "{} {}",
                crate::controller::harness_icon(harness.kind),
                harness.kind.display_name()
            ),
            crate::controller::harness_role(harness.kind),
        );
    }
    if let Some(activity) = crate::controller::harness_status(pane, known) {
        row.append(" · ", Role::Muted);
        row.append(&activity.state, activity_role(&activity.state));
        if let Some(progress) = activity.progress {
            row.right(format!("{progress}%"), activity_role(&activity.state));
        }
    } else if include_title {
        row.right(lifecycle, Role::Muted);
    }
    row
}

fn change_row(rows: &mut Vec<Paragraph>, label: &str, summary: &ChangeSummary) {
    let mut row = Paragraph::new(format!("{label} · {} files · ", summary.files), Role::Muted);
    row.append(format!("+{}", summary.insertions), Role::Success);
    row.append(" ", Role::Muted);
    row.append(format!("−{}", summary.deletions), Role::Error);
    if summary.binary_files > 0 {
        row.append(format!(" · {} binary", summary.binary_files), Role::Muted);
    }
    rows.push(row);
}

fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Byte boundary of the longest complete grapheme prefix within a cell budget.
fn prefix(text: &str, budget: usize) -> usize {
    let mut used = 0;
    for (byte, glyph) in text.grapheme_indices(true) {
        let advance = width(glyph);
        if used + advance > budget {
            return byte;
        }
        used += advance;
    }
    text.len()
}

fn append_span(line: &mut PreviewLine, text: &str, role: Role) {
    let start = width(&line.text);
    line.text.push_str(text);
    let cells = width(text);
    if cells > 0 {
        line.spans.push(RoleSpan {
            rect: Rect {
                x: start as u16,
                y: 0,
                width: cells as u16,
                height: 1,
            },
            role,
        });
    }
}

fn body_line(paragraph: &Paragraph, range: Range<usize>) -> PreviewLine {
    let text = &paragraph.text[range.clone()];
    let mut line = PreviewLine {
        text: text.to_owned(),
        role: paragraph.role,
        spans: Vec::new(),
        projects: Vec::new(),
    };
    for (span, ink) in &paragraph.spans {
        let start = span.start.max(range.start);
        let end = span.end.min(range.end);
        if start < end {
            let rect = Rect {
                x: width(&paragraph.text[range.start..start]) as u16,
                y: 0,
                width: width(&paragraph.text[start..end]) as u16,
                height: 1,
            };
            match ink {
                Ink::Role(role) => line.spans.push(RoleSpan { rect, role: *role }),
                Ink::Project(slot) => line.projects.push(ProjectSpan { rect, slot: *slot }),
            }
        }
    }
    line
}

fn truncate(paragraph: &Paragraph, columns: usize) -> PreviewLine {
    let right = paragraph
        .right
        .iter()
        .map(|(text, _)| width(text))
        .sum::<usize>()
        .min(columns / 2);
    let budget = columns.saturating_sub(right + usize::from(right > 0));
    let clipped = width(&paragraph.text) > budget;
    let end = prefix(&paragraph.text, budget.saturating_sub(usize::from(clipped)));
    let mut line = body_line(paragraph, 0..end);
    if clipped && budget > 0 {
        append_span(&mut line, "…", Role::Muted);
    }
    if right > 0 {
        line.text.extend(std::iter::repeat_n(
            ' ',
            columns.saturating_sub(right + width(&line.text)),
        ));
        let mut remaining = right;
        for (text, role) in &paragraph.right {
            let end = prefix(text, remaining);
            append_span(&mut line, &text[..end], *role);
            remaining = remaining.saturating_sub(width(&text[..end]));
        }
    }
    line
}

/// Textwrap owns word breaking; this adapter only projects Bkpr semantic ranges
/// and protects full graphemes when an unbreakable token exceeds a tiny viewport.
pub(crate) fn wrap(paragraphs: &[Paragraph], columns: u16, lines: &mut Vec<PreviewLine>) {
    if columns == 0 {
        return;
    }
    let columns = usize::from(columns);
    let options = textwrap::Options::new(columns)
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
        .word_splitter(textwrap::WordSplitter::NoHyphenation)
        .break_words(false);
    for paragraph in paragraphs {
        if paragraph.truncate {
            lines.push(truncate(paragraph, columns));
            continue;
        }
        let mut offset = 0;
        for wrapped in textwrap::wrap(&paragraph.text, &options) {
            let start = offset
                + paragraph.text[offset..]
                    .find(wrapped.as_ref())
                    .expect("textwrap does not insert metadata characters");
            let end = start + wrapped.len();
            let mut byte = start;
            while byte < end {
                let length = prefix(&paragraph.text[byte..end], columns);
                if length == 0 {
                    // A two-cell grapheme cannot occupy a one-cell body.
                    let glyph = paragraph.text[byte..end].graphemes(true).next().unwrap();
                    lines.push(PreviewLine {
                        text: "…".into(),
                        role: paragraph.role,
                        spans: Vec::new(),
                        projects: Vec::new(),
                    });
                    byte += glyph.len();
                } else {
                    lines.push(body_line(paragraph, byte..byte + length));
                    byte += length;
                }
            }
            if start == end {
                lines.push(body_line(paragraph, start..end));
            }
            offset = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::RepositoryInfo;

    fn project(state: ProjectState) -> ProjectSnapshot {
        ProjectSnapshot {
            color_slot: None,
            cwd: "/project".into(),
            state,
            error: None,
            repository: Some(RepositoryInfo {
                name: "project".into(),
                root: "/project".into(),
                common_dir: "/project/.git".into(),
                git_dir: "/project/.git".into(),
                branch: Some("feature/日本語/very-long-branch".into()),
                head_sha: Some("abcdef012345".into()),
                staged: ChangeSummary {
                    files: 1,
                    insertions: 8,
                    deletions: 3,
                    binary_files: 0,
                },
                unstaged: ChangeSummary::default(),
                untracked_files: 0,
                base_ref: None,
                base_sha: None,
                committed: None,
            }),
        }
    }

    #[test]
    fn wrapping_projects_semantic_inks_across_words_and_graphemes() {
        let mut row = Paragraph::new("Changes · ", Role::Muted);
        row.append("界e\u{301}x +8", Role::Success);
        row.append(" −3", Role::Error);
        let rows = [row];
        for columns in [1, 2, 3, 8, 30] {
            let mut lines = Vec::new();
            wrap(&rows, columns, &mut lines);
            assert!(!lines.is_empty());
            for line in &lines {
                assert!(width(&line.text) <= usize::from(columns));
                assert!(!line.text.starts_with('\u{301}'));
                for span in &line.spans {
                    assert!(span.rect.x + span.rect.width <= columns);
                }
            }
            assert!(
                lines
                    .iter()
                    .any(|line| line.spans.iter().any(|span| span.role == Role::Success))
            );
            assert!(
                lines
                    .iter()
                    .any(|line| line.spans.iter().any(|span| span.role == Role::Error))
            );
        }
        let mut lines = Vec::new();
        wrap(&[Paragraph::new("界e\u{301}x", Role::Panel)], 3, &mut lines);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["界e\u{301}", "x"]
        );
        lines.clear();
        wrap(&[Paragraph::new("界a", Role::Panel)], 1, &mut lines);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["…", "a"]
        );
    }

    #[test]
    fn project_ink_clips_at_graphemes_without_recoloring_status_or_progress() {
        let mut row = Paragraph::project("界e\u{301}x", Role::Panel, Some(7));
        row.append(" stale", Role::Warning);
        row.right("58%", Role::Info);
        for (columns, expected_width) in [(8, 3), (12, 4), (30, 4)] {
            let mut lines = Vec::new();
            wrap(std::slice::from_ref(&row), columns, &mut lines);
            let line = &lines[0];
            assert_eq!(
                line.projects
                    .iter()
                    .map(|span| (span.rect, span.slot))
                    .collect::<Vec<_>>(),
                [(
                    Rect {
                        x: 0,
                        y: 0,
                        width: expected_width,
                        height: 1
                    },
                    Some(7)
                )]
            );
            for project in &line.projects {
                for status in line
                    .spans
                    .iter()
                    .filter(|span| matches!(span.role, Role::Warning | Role::Info))
                {
                    assert!(
                        project.rect.x + project.rect.width <= status.rect.x
                            || status.rect.x + status.rect.width <= project.rect.x
                    );
                }
            }
        }
    }

    #[test]
    fn refresh_keeps_cached_fact_text_geometry_and_semantic_inks() {
        let mut ready = project(ProjectState::Ready);
        ready.color_slot = Some(3);
        let mut refreshing = ready.clone();
        refreshing.state = ProjectState::Loading;
        for columns in [1, 3, 8, 24, 40, 80] {
            let render = |project: &ProjectSnapshot| {
                let mut rows = Vec::new();
                project_content(Some(project), true, &mut rows);
                let mut lines = Vec::new();
                wrap(&rows, columns, &mut lines);
                lines
            };
            let current = render(&ready);
            let stale = render(&refreshing);
            assert_eq!(current.len(), stale.len());
            assert_eq!(current[0].role, Role::Muted);
            assert_eq!(stale[0].role, Role::Warning);
            for (current, stale) in current[1..].iter().zip(&stale[1..]) {
                assert_eq!(current.text, stale.text);
                assert_eq!(current.role, stale.role);
                assert_eq!(current.spans, stale.spans);
                assert_eq!(
                    current
                        .projects
                        .iter()
                        .map(|span| (span.rect, span.slot))
                        .collect::<Vec<_>>(),
                    stale
                        .projects
                        .iter()
                        .map(|span| (span.rect, span.slot))
                        .collect::<Vec<_>>(),
                );
            }
        }
    }

    #[test]
    fn unavailable_repository_states_never_claim_cached_success_is_current() {
        for state in [
            ProjectState::Loading,
            ProjectState::Error,
            ProjectState::NotRepository,
        ] {
            let mut project = project(state);
            project.error = Some("Git refresh failed\u{1b}\nretry later".into());
            let mut rows = Vec::new();
            project_content(Some(&project), true, &mut rows);
            assert!(rows.iter().any(|row| row.text.contains("stale")));
            assert!(
                rows.iter()
                    .any(|row| row.role == Role::Error && row.text.contains("refresh failed"))
            );
            let mut lines = Vec::new();
            wrap(&rows, 40, &mut lines);
            assert!(lines.iter().any(|line| line.text.contains("stale")));
            assert!(
                lines
                    .iter()
                    .all(|line| !line.text.contains('\u{1b}') && !line.text.contains('\n'))
            );
        }
    }

    #[test]
    fn missing_branch_comparison_is_not_zero_changes() {
        let mut project = project(ProjectState::Ready);
        let repository = project.repository.as_mut().unwrap();
        repository.branch = None;
        repository.head_sha = None;
        let mut rows = Vec::new();
        project_content(Some(&project), true, &mut rows);
        assert!(rows.iter().any(|row| row.text.contains("no HEAD commit")));
        assert!(
            rows.iter()
                .any(|row| row.text.contains("no suitable local base"))
        );
        assert!(!rows.iter().any(|row| row.text.starts_with("Committed")));
    }
}
