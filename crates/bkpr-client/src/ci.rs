//! Native projection of pre-native-rewrite ui.rs CI artwork and popup facts.
//! Provider metadata is supplied by the daemon; this module performs no I/O.
use crate::metadata::Paragraph;
use bkpr_protocol::{
    ChangeRequestState, CiFetchState, CiRefreshTarget, CiSnapshot, CiStatus, ProjectSnapshot,
};
use bkpr_tui_framework::Role;

pub(crate) const WORKFLOW: &str = "\u{f52e}";
pub(crate) const REFRESH: &str = "[ \u{f021}  ]";

pub(crate) fn state(ci: &CiSnapshot) -> CiStatus {
    if ci.fetch_state != CiFetchState::Ready
        || ci.stale
        || ci.error.is_some()
        || ci.truncated
        || ci.commit_missing
    {
        CiStatus::Unknown
    } else {
        ci.status
    }
}
pub(crate) fn glyph(status: CiStatus) -> &'static str {
    match status {
        CiStatus::NoChecks => "\u{f0377}",
        CiStatus::Pending => "\u{f0150}",
        CiStatus::Running => "\u{f0996}",
        CiStatus::Passed => "\u{f05e1}",
        CiStatus::Failed => "\u{f015a}",
        CiStatus::Cancelled => "\u{f073a}",
        CiStatus::Skipped => "\u{f0662}",
        CiStatus::Unknown => "\u{f0625}",
    }
}
pub(crate) fn role(status: CiStatus) -> Role {
    match status {
        CiStatus::Passed => Role::Success,
        CiStatus::Failed => Role::Error,
        CiStatus::Pending | CiStatus::Running => Role::Warning,
        _ => Role::Muted,
    }
}
pub(crate) fn request(state: ChangeRequestState) -> (&'static str, Role) {
    match state {
        ChangeRequestState::Draft => ("\u{f4dd}", Role::Muted),
        ChangeRequestState::Open => ("\u{f407}", Role::Success),
        ChangeRequestState::Merged => ("\u{f419}", Role::Accent),
        ChangeRequestState::Closed => ("\u{f4dc}", Role::Error),
    }
}
pub(crate) fn safe_link(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split('/').next()?;
    let host = if let Some((host, port)) = authority.split_once(':') {
        port.parse::<u16>().ok().filter(|port| *port != 0)?;
        host
    } else {
        authority
    };
    if host.trim_end_matches('.').split('.').any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-') || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
        || url.len() > 4096 || url.chars().any(|c| c == '\\' || c.is_control() || c.is_whitespace() || matches!(c, '\u{061c}' | '\u{200b}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}' | '\u{2066}'..='\u{2069}' | '\u{feff}')) {
        return None;
    }
    Some(url)
}
pub(crate) fn repository_heading(project: &ProjectSnapshot) -> Paragraph {
    match &project.ci {
        Some(ci) => {
            let mut identity = format!("\u{f09b} {}", ci.repository.path);
            if let Some(request) = &ci.change_request {
                identity.push_str(&format!(" #{}", request.number));
            }
            Paragraph::project(identity, Role::Title, project.color_slot).linked(
                ci.change_request
                    .as_ref()
                    .map_or(ci.repository.web_url.as_str(), |request| {
                        request.url.as_str()
                    }),
            )
        }
        None => Paragraph::project(
            project.repository.as_ref().map_or_else(
                || project.cwd.to_string_lossy().into_owned(),
                |repository| repository.name.clone(),
            ),
            Role::Title,
            project.color_slot,
        ),
    }
}
fn duration(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".into(),
        Some(0) => "0s".into(),
        Some(s @ 1..60) => format!("{s}s"),
        Some(s @ 60..3600) => format!("{}:{:02}", s / 60, s % 60),
        Some(s) => {
            let minutes = s % 3600 / 60;
            if minutes == 0 {
                format!("{}h", s / 3600)
            } else {
                format!("{}h {minutes}m", s / 3600)
            }
        }
    }
}
fn freshness(ci: &CiSnapshot) -> String {
    if ci.updated_at == 0 {
        return "not yet fetched".into();
    }
    let timestamp = i64::try_from(ci.updated_at)
        .ok()
        .and_then(|value| time::OffsetDateTime::from_unix_timestamp(value).ok())
        .and_then(|value| {
            value
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        });
    timestamp.map_or_else(
        || "fetch time unavailable".into(),
        |timestamp| format!("fetched {timestamp}"),
    )
}
fn error_detail(error: &str) -> String {
    let mut detail = String::with_capacity(error.len());
    for word in error.split_whitespace() {
        if !detail.is_empty() {
            detail.push(' ');
        }
        detail.push_str(
            if word.starts_with("https://") || word.starts_with("http://") {
                "[provider link]"
            } else {
                word
            },
        );
    }
    detail
}
fn source_known(ci: &CiSnapshot) -> bool {
    !ci.repository.host.is_empty()
        && !ci.repository.path.is_empty()
        && (!ci.head_sha.is_empty()
            || ci
                .checked_ref
                .as_deref()
                .is_some_and(|reference| !reference.is_empty()))
}
pub(crate) fn target(project: &ProjectSnapshot, pane: &str) -> Option<CiRefreshTarget> {
    let ci = project.ci.as_ref()?;
    if ci.fetch_state == CiFetchState::Loading || !source_known(ci) {
        return None;
    }
    Some(CiRefreshTarget {
        pane_id: pane.into(),
        cwd: project.cwd.clone(),
        repository: ci.repository.clone(),
        head_sha: ci.head_sha.clone(),
        local_head_sha: ci.local_head_sha.clone(),
        checked_ref: ci.checked_ref.clone(),
    })
}
pub(crate) fn matches_target(project: &ProjectSnapshot, target: &CiRefreshTarget) -> bool {
    project.cwd == target.cwd
        && project.ci.as_ref().is_some_and(|ci| {
            ci.fetch_state != CiFetchState::Loading
                && source_known(ci)
                && ci.repository == target.repository
                && ci.head_sha == target.head_sha
                && ci.local_head_sha == target.local_head_sha
                && ci.checked_ref == target.checked_ref
        })
}
pub(crate) fn request_content(ci: &CiSnapshot, rows: &mut Vec<Paragraph>) {
    if let Some(pr) = &ci.change_request {
        let (icon, ink) = request(pr.state);
        let mut row = Paragraph::new(format!("{icon} "), ink).linked(&pr.url);
        row.append(&pr.title, Role::Panel);
        rows.push(row);
        for issue in &pr.issues {
            let label = match &issue.title {
                Some(title) if !crate::metadata::clean(title).trim().is_empty() => {
                    format!("\u{f02e} {} · {title}", issue.identifier)
                }
                _ => format!("\u{f02e} {}", issue.identifier),
            };
            rows.push(Paragraph::new(label, Role::Panel).linked(&issue.url));
        }
    }
}
pub(crate) fn content(
    project: Option<&ProjectSnapshot>,
    pane: Option<&str>,
    checks: bool,
    rows: &mut Vec<Paragraph>,
) {
    let Some(project) = project else {
        return;
    };
    let Some(ci) = &project.ci else {
        if project.repository.is_some() {
            rows.push(Paragraph::new("Checks not available", Role::Muted));
        }
        return;
    };
    rows.push(Paragraph::new("", Role::Panel));
    rows.push(Paragraph::new("Checks", Role::Title));
    let status = state(ci);
    let fetch = match ci.fetch_state {
        CiFetchState::Loading => "Loading",
        CiFetchState::Unavailable => "Unavailable",
        CiFetchState::Ready => "Ready",
    };
    let mut summary = Paragraph::new(
        format!("{WORKFLOW} CI · {} {fetch}", glyph(status)),
        role(status),
    )
    .single_line();
    if ci.fetch_state == CiFetchState::Ready {
        summary.append(format!(" · {}", status.label()), role(status));
    }
    if ci.stale {
        summary.append(" · stale", Role::Warning);
    }
    if ci.truncated {
        summary.append(" · incomplete", Role::Muted);
    }
    if pane.is_some() && source_known(ci) {
        summary.right(
            REFRESH,
            if ci.fetch_state == CiFetchState::Loading {
                Role::Muted
            } else {
                Role::Panel
            },
        );
        summary.refresh = pane.and_then(|pane| target(project, pane));
    }
    rows.push(summary);
    if ci.updated_at != 0 {
        rows.push(Paragraph::new(freshness(ci), Role::Muted));
    }
    let checked = match (ci.checked_ref.as_deref(), ci.head_sha.is_empty()) {
        (Some(reference), false) => format!("Checks for {reference} @ {:.7}", ci.head_sha),
        (Some(reference), true) => format!("Remote target {reference} · commit unavailable"),
        (None, false) => format!("Checks for commit @ {:.7}", ci.head_sha),
        (None, true) => "Checked commit unavailable".into(),
    };
    rows.push(Paragraph::new(checked, Role::Muted));
    if !ci.head_sha.is_empty() && !ci.local_head_sha.is_empty() && ci.head_sha != ci.local_head_sha
    {
        rows.push(Paragraph::new(
            format!(
                "Local HEAD @ {:.7} · differs from checked commit",
                ci.local_head_sha
            ),
            Role::Muted,
        ));
    }
    if let Some(error) = &ci.error {
        rows.push(Paragraph::new(
            format!("Checks unavailable: {}", error_detail(error)),
            Role::Error,
        ));
    }
    if ci.stale {
        rows.push(Paragraph::new(
            "Cached results; current check state is not confirmed",
            Role::Warning,
        ));
    }
    if ci.truncated {
        rows.push(Paragraph::new(
            "Checks incomplete: provider pagination limit reached",
            Role::Warning,
        ));
    }
    if ci.commit_missing {
        rows.push(Paragraph::new(
            "Checked branch or commit is missing on the provider",
            Role::Warning,
        ));
    }
    if !checks {
        return;
    }
    if ci.checks.is_empty() {
        rows.push(Paragraph::new(
            if ci.fetch_state == CiFetchState::Loading {
                "Fetching checks…"
            } else if status == CiStatus::NoChecks {
                "No checks reported for this commit"
            } else {
                "No check results available"
            },
            Role::Muted,
        ));
    }
    for check in &ci.checks {
        let mut row = Paragraph::new(format!("{} ", glyph(check.status)), role(check.status));
        row.append(&check.name, Role::Panel);
        row.right(duration(check.duration_seconds), Role::Muted);
        if let Some(url) = &check.url {
            row = row.linked(url);
        }
        rows.push(row);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bkpr_protocol::*;

    pub(crate) fn project() -> ProjectSnapshot {
        let repository = ForgeRepository {
            provider: ForgeProvider::Github,
            host: "github.com".into(),
            path: "owner/repo".into(),
            web_url: "https://github.com/owner/repo".into(),
        };
        ProjectSnapshot {
            cwd: "/fixture/repo".into(),
            state: ProjectState::Ready,
            color_slot: Some(3),
            error: None,
            repository: Some(RepositoryInfo {
                name: "repo".into(),
                root: "/fixture/repo".into(),
                common_dir: "/fixture/repo/.git".into(),
                git_dir: "/fixture/repo/.git".into(),
                branch: Some("feature/日本語".into()),
                head_sha: Some("localabcdef".into()),
                staged: ChangeSummary::default(),
                unstaged: ChangeSummary::default(),
                untracked_files: 0,
                base_ref: Some("origin/main".into()),
                base_sha: Some("baseabcdef".into()),
                committed: Some(ChangeSummary {
                    files: 2,
                    insertions: 1234,
                    deletions: 567,
                    binary_files: 1,
                }),
            }),
            ci: Some(CiSnapshot {
                status: CiStatus::Passed,
                fetch_state: CiFetchState::Ready,
                updated_at: 1,
                stale: false,
                error: None,
                repository: repository.clone(),
                head_sha: "remoteabcdef".into(),
                branch: Some("feature/日本語".into()),
                local_head_sha: "localabcdef".into(),
                checked_ref: Some("origin/feature/日本語".into()),
                truncated: false,
                commit_missing: false,
                checks: vec![CiCheck {
                    name: "unit tests 日本語".into(),
                    status: CiStatus::Passed,
                    url: Some("https://github.com/owner/repo/actions/runs/7".into()),
                    duration_seconds: Some(123),
                }],
                change_request: Some(ChangeRequest {
                    number: 42,
                    title: "Restore project metadata".into(),
                    url: "https://github.com/owner/repo/pull/42".into(),
                    state: ChangeRequestState::Open,
                    description: "Not a historical popup paragraph".into(),
                    issues: vec![LinkedIssue {
                        provider: IssueTracker::Linear,
                        identifier: "ENG-42".into(),
                        title: Some("Repair rendering".into()),
                        url: "https://linear.app/team/issue/ENG-42/repair-rendering".into(),
                    }],
                    base: Some(ChangeRequestBase {
                        branch: "stack/parent".into(),
                        commit: "baseabcdef".into(),
                        repository,
                    }),
                }),
            }),
        }
    }
    #[test]
    fn historical_artwork_duration_and_untrusted_links_are_preserved() {
        for (seconds, expected) in [
            (None, "—"),
            (Some(0), "0s"),
            (Some(9), "9s"),
            (Some(60), "1:00"),
            (Some(123), "2:03"),
            (Some(3600), "1h"),
            (Some(3660), "1h 1m"),
        ] {
            assert_eq!(duration(seconds), expected);
        }
        assert_eq!(glyph(CiStatus::Passed), "\u{f05e1}");
        assert_eq!(glyph(CiStatus::Failed), "\u{f015a}");
        for url in [
            "http://github.com/repo",
            "https://user@github.com/repo",
            "https://github.com/repo\u{1b}]8;;",
            "https://github.com/repo\nnext",
            "https://github.com/repo\u{202e}",
            "https://github.com\\evil/repo",
        ] {
            assert!(safe_link(url).is_none(), "{url:?}");
        }
        assert!(safe_link("https://github.com/owner/repo/actions/runs/7?check=1").is_some());
    }
    #[test]
    fn popup_retains_remote_local_pr_issue_base_and_check_context_at_all_widths() {
        let project = project();
        let mut rows = vec![repository_heading(&project)];
        request_content(project.ci.as_ref().unwrap(), &mut rows);
        crate::metadata::project_content(Some(&project), false, &mut rows);
        content(Some(&project), Some("exact-pane"), true, &mut rows);
        let text = rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "\u{f09b} owner/repo #42",
            "\u{f407}",
            "Restore project metadata",
            "\u{f02e} ENG-42 · Repair rendering",
            "Checks for origin/feature/日本語 @ remotea",
            "Local HEAD @ localab",
            "unit tests 日本語",
        ] {
            assert!(text.contains(expected), "{expected}");
        }
        assert!(!text.contains("Not a historical popup paragraph"));
        let position = |label: &str| {
            rows.iter()
                .position(|row| row.text.contains(label))
                .unwrap()
        };
        assert!(position("owner/repo #42") < position("Restore project metadata"));
        assert!(position("Restore project metadata") < position("ENG-42"));
        assert!(position("ENG-42") < position("\u{f062c}"));
        let refresh = rows.iter().find_map(|row| row.refresh.as_ref()).unwrap();
        assert_eq!(refresh.pane_id, "exact-pane");
        assert_eq!(refresh.cwd, project.cwd);
        assert_eq!(refresh.head_sha, "remoteabcdef");
        assert_eq!(refresh.local_head_sha, "localabcdef");
        for columns in 1..=80 {
            let mut lines = Vec::new();
            crate::metadata::wrap(&rows, columns, &mut lines);
            for line in &lines {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line.text.as_str())
                        <= usize::from(columns)
                );
                if let Some((cells, url)) = &line.link {
                    assert!(*cells <= columns);
                    assert!(safe_link(url).is_some());
                }
                if let Some((rect, target)) = &line.refresh {
                    assert!(rect.x + rect.width <= columns);
                    assert_eq!(rect.width, 6);
                    assert_eq!(target, refresh);
                }
            }
            if columns == 80 {
                assert!(lines.iter().any(|line| line.text.contains("2:03")));
                assert!(
                    lines.iter().any(|line| line.text.contains("stack/parent")
                        && line.text.contains("+1234 -567"))
                );
            }
        }
    }
    #[test]
    fn freshness_failures_never_claim_cached_green_is_current_or_retarget_refresh() {
        for case in 0..6 {
            let mut project = project();
            let ci = project.ci.as_mut().unwrap();
            match case {
                0 => ci.fetch_state = CiFetchState::Loading,
                1 => ci.error = Some("permission denied https://secret.invalid/token".into()),
                2 => ci.stale = true,
                3 => ci.truncated = true,
                4 => ci.commit_missing = true,
                _ => {
                    ci.head_sha.clear();
                    ci.checked_ref = None;
                    ci.fetch_state = CiFetchState::Unavailable;
                }
            }
            assert_eq!(state(ci), CiStatus::Unknown);
            let mut rows = Vec::new();
            content(Some(&project), Some("exact-pane"), true, &mut rows);
            assert!(rows.iter().any(|row| row.text.contains("unit tests")));
            assert!(
                rows.iter()
                    .all(|row| !row.text.contains("https://secret.invalid/token"))
            );
            if case == 0 || case == 5 {
                assert!(rows.iter().all(|row| row.refresh.is_none()));
            } else {
                assert_eq!(
                    rows.iter()
                        .find_map(|row| row.refresh.as_ref())
                        .unwrap()
                        .head_sha,
                    "remoteabcdef"
                );
            }
        }
    }
    #[test]
    fn cold_unavailable_named_branch_keeps_exact_manual_recovery_without_a_checked_commit() {
        let mut project = project();
        let ci = project.ci.as_mut().unwrap();
        ci.fetch_state = CiFetchState::Unavailable;
        ci.head_sha.clear();
        ci.checks.clear();
        ci.error = Some("Authentication unavailable until keychain unlock".into());
        let mut rows = Vec::new();
        content(Some(&project), Some("exact-pane"), true, &mut rows);
        let target = rows.iter().find_map(|row| row.refresh.as_ref()).unwrap();
        assert!(target.head_sha.is_empty());
        assert_eq!(target.checked_ref.as_deref(), Some("origin/feature/日本語"));
        assert_eq!(target.local_head_sha, "localabcdef");
        assert!(matches_target(&project, target));
        assert!(
            rows.iter()
                .any(|row| row.text.contains("commit unavailable"))
        );
        project.ci.as_mut().unwrap().checked_ref = None;
        assert!(
            super::target(&project, "exact-pane").is_none(),
            "detached source needs a checked commit"
        );
        assert!(!matches_target(&project, target));
    }
    #[test]
    fn fetch_timestamp_is_absolute_utc_and_does_not_depend_on_the_client_clock() {
        let mut ci = project().ci.unwrap();
        ci.updated_at = 1;
        assert_eq!(freshness(&ci), "fetched 1970-01-01T00:00:01Z");
        ci.updated_at = u64::MAX;
        assert_eq!(freshness(&ci), "fetch time unavailable");
        ci.updated_at = 0;
        assert_eq!(freshness(&ci), "not yet fetched");
    }
    #[test]
    fn data_only_previews_keep_checks_without_advertising_unrouted_refresh_controls() {
        let project = project();
        for (pane, actionable) in [(None, false), (Some("exact-pane"), true)] {
            let mut rows = Vec::new();
            content(Some(&project), pane, true, &mut rows);
            let mut lines = Vec::new();
            crate::metadata::wrap(&rows, 80, &mut lines);
            assert!(lines.iter().any(|line| line.text.contains("unit tests")));
            assert_eq!(
                lines.iter().any(|line| line.text.contains(REFRESH)),
                actionable
            );
            assert_eq!(lines.iter().any(|line| line.refresh.is_some()), actionable);
        }
    }
    #[test]
    fn complete_fetch_timestamp_wraps_without_truncation_and_error_detail_is_not_duplicated() {
        let mut project = project();
        let ci = project.ci.as_mut().unwrap();
        ci.updated_at = 1;
        ci.error = Some("provider-unavailable-marker".into());
        let mut rows = Vec::new();
        content(Some(&project), Some("exact-pane"), true, &mut rows);
        let timestamp = "1970-01-01T00:00:01Z";
        assert_eq!(
            rows.iter()
                .filter(|row| row.text.contains(timestamp))
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.text.contains("provider-unavailable-marker"))
                .count(),
            1
        );
        let row = rows
            .iter()
            .find(|row| row.text.contains(timestamp))
            .unwrap();
        for columns in [1, 2, 8, 20, 28, 40, 62] {
            let mut lines = Vec::new();
            crate::metadata::wrap(std::slice::from_ref(row), columns, &mut lines);
            let rendered = lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<String>();
            assert!(rendered.contains(timestamp), "{columns}: {rendered}");
            assert!(
                !rendered.contains('…'),
                "fetch timestamp is complete at every viewport width"
            );
        }
    }
}
