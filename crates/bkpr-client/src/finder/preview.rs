//! Bkpr-specific pane preview, based on main 9ad93bd's MIT-licensed
//! ui.rs::tooltip pane grouping. Only current protocol facts are projected.
use bkpr_protocol::{
    ChangeSummary, PaneInfo, PaneSnapshot, PaneStatus, ProjectState, RegistrySnapshot,
};
use bkpr_tui_framework::Role;

pub(super) struct Paragraph {
    pub text: String,
    pub role: Role,
}
pub(super) struct PreviewLine {
    pub text: String,
    pub role: Role,
}

pub(super) fn clean(text: &str) -> String {
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
    rows.push(Paragraph {
        text: clean(text.as_ref()),
        role,
    });
}
fn heading(rows: &mut Vec<Paragraph>, title: &str) {
    push(rows, "", Role::Panel);
    push(rows, title, Role::Title);
}
fn changes(summary: &ChangeSummary) -> String {
    let mut text = format!(
        "{} files · +{} −{}",
        summary.files, summary.insertions, summary.deletions
    );
    if summary.binary_files > 0 {
        text.push_str(&format!(" · {} binary", summary.binary_files));
    }
    text
}
fn activity_role(state: &str) -> Role {
    match state {
        "working" => Role::Info,
        "done" => Role::Success,
        "blocked" => Role::Warning,
        "error" => Role::Error,
        _ => Role::Muted,
    }
}

pub(super) fn content(
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
    let lifecycle = match pane.status {
        PaneStatus::Running => "Running".to_owned(),
        PaneStatus::Stopped => "Stopped".to_owned(),
        PaneStatus::Exited => pane
            .exit_code
            .map_or_else(|| "Exited".into(), |code| format!("Exited ({code})")),
    };
    if let Some(harness) = harness {
        push(
            &mut rows,
            format!("{} · {lifecycle}", harness.kind.display_name()),
            Role::Accent,
        );
    } else {
        push(&mut rows, lifecycle, Role::Muted);
    }
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
    if let Some(error) = harness
        .and_then(|h| h.managed.as_ref())
        .and_then(|m| m.error.as_ref())
    {
        heading(&mut rows, "Capture");
        push(&mut rows, error, Role::Error);
    }
    heading(&mut rows, "Repository");
    match &pane.project {
        None => push(&mut rows, "Local project metadata unavailable", Role::Muted),
        Some(project) => {
            let stale = project.state != ProjectState::Ready;
            match project.state {
                ProjectState::Loading => {
                    push(&mut rows, "Refreshing local Git metadata…", Role::Muted)
                }
                ProjectState::NotRepository => push(
                    &mut rows,
                    "Local directory · not a Git repository",
                    Role::Muted,
                ),
                ProjectState::Error => {
                    push(&mut rows, "Local Git metadata unavailable", Role::Warning)
                }
                ProjectState::Ready => {}
            }
            if let Some(error) = &project.error {
                push(&mut rows, error, Role::Error);
            }
            if let Some(repository) = &project.repository {
                if stale {
                    push(
                        &mut rows,
                        "Last-known repository facts · stale",
                        Role::Warning,
                    );
                }
                push(
                    &mut rows,
                    &repository.name,
                    if stale { Role::Warning } else { Role::Accent },
                );
                push(
                    &mut rows,
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
                push(
                    &mut rows,
                    branch,
                    if stale {
                        Role::Warning
                    } else {
                        Role::Secondary
                    },
                );
                heading(
                    &mut rows,
                    if stale {
                        "Worktree changes · stale"
                    } else {
                        "Worktree changes"
                    },
                );
                if repository.staged == ChangeSummary::default()
                    && repository.unstaged == ChangeSummary::default()
                    && repository.untracked_files == 0
                {
                    push(
                        &mut rows,
                        "Clean working tree",
                        if stale { Role::Warning } else { Role::Success },
                    );
                } else {
                    push(
                        &mut rows,
                        format!("Staged · {}", changes(&repository.staged)),
                        Role::Success,
                    );
                    push(
                        &mut rows,
                        format!("Unstaged · {}", changes(&repository.unstaged)),
                        Role::Warning,
                    );
                    push(
                        &mut rows,
                        format!("Untracked · {} files", repository.untracked_files),
                        Role::Muted,
                    );
                }
                heading(
                    &mut rows,
                    if stale {
                        "Branch changes · stale"
                    } else {
                        "Branch changes"
                    },
                );
                match (&repository.base_ref, &repository.committed) {
                    (Some(base), Some(summary)) => {
                        push(
                            &mut rows,
                            format!("HEAD vs merge base of {base}"),
                            Role::Muted,
                        );
                        push(&mut rows, changes(summary), Role::Info);
                    }
                    _ => push(
                        &mut rows,
                        "Comparison unavailable · no suitable local base",
                        Role::Muted,
                    ),
                }
            } else if project.state == ProjectState::Ready {
                push(&mut rows, "Repository details unavailable", Role::Muted);
            }
        }
    }
    heading(&mut rows, "Checks & tickets");
    push(
        &mut rows,
        "Forge, CI and ticket metadata are not available",
        Role::Muted,
    );
    rows
}

pub(super) fn wrap(paragraphs: &[Paragraph], width: u16, lines: &mut Vec<PreviewLine>) {
    if width == 0 {
        return;
    }
    let options = textwrap::Options::new(usize::from(width))
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
        .word_splitter(textwrap::WordSplitter::NoHyphenation);
    for paragraph in paragraphs {
        if paragraph.text.is_empty() {
            lines.push(PreviewLine {
                text: String::new(),
                role: paragraph.role,
            });
        } else {
            lines.extend(
                textwrap::wrap(&paragraph.text, &options)
                    .into_iter()
                    .map(|line| PreviewLine {
                        text: line.into_owned(),
                        role: paragraph.role,
                    }),
            );
        }
    }
}
