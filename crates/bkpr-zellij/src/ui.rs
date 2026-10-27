use bkpr::agent_contract::harness_name;
use bkpr::ci_contract::{
    ChangeRequestState, CiFetchState, CiRefreshTarget, CiSnapshot, CiStatus, ForgeProvider,
    IssueTracker, LinkedIssue,
};
use bkpr::{Attention, ProjectIdentity, Snapshot, Status};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

pub const RESET: &str = "\u{1b}[0m";
pub use crate::palette::Theme;
pub const TEXT: &str = "\u{1b}[22;39m";
const WORKFLOW: &str = "\u{f52e}"; // nf-oct-workflow

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    Workspace(String),
    Pane(String),
}

impl Item {
    pub fn workspace_id<'a>(&'a self, state: &'a Snapshot) -> Option<&'a str> {
        match self {
            Self::Workspace(id) => Some(id),
            Self::Pane(id) => state
                .panes
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.workspace_id.as_str()),
        }
    }
}

#[derive(Clone)]
pub struct TreeRow {
    pub item: Option<Item>,
    pub workspace_id: Option<String>,
    pub kind: RowKind,
}

#[derive(Clone)]
pub enum RowKind {
    Spacer,
    Workspace { index: usize, collapsed: bool },
    Projects { index: usize, has_children: bool },
    Pane { index: usize, last: bool },
    Detail { index: usize, last: bool },
}

pub fn tree(state: &Snapshot, collapsed: &std::collections::BTreeSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (index, workspace) in state.workspaces.iter().enumerate() {
        if index > 0 {
            rows.push(TreeRow {
                item: None,
                workspace_id: None,
                kind: RowKind::Spacer,
            });
        }
        let folded = collapsed.contains(&workspace.id);
        let children: Vec<_> = state
            .panes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.workspace_id == workspace.id)
            .collect();
        let item = Some(Item::Workspace(workspace.id.clone()));
        let group = Some(workspace.id.clone());
        rows.push(TreeRow {
            item: item.clone(),
            workspace_id: group.clone(),
            kind: RowKind::Workspace {
                index,
                collapsed: folded,
            },
        });
        if folded {
            continue;
        }
        rows.push(TreeRow {
            item,
            workspace_id: group.clone(),
            kind: RowKind::Projects {
                index,
                has_children: !children.is_empty(),
            },
        });
        for (child_index, (pane_index, pane)) in children.iter().enumerate() {
            let item = Some(Item::Pane(pane.id.clone()));
            let last = child_index + 1 == children.len();
            rows.push(TreeRow {
                item: item.clone(),
                workspace_id: group.clone(),
                kind: RowKind::Pane {
                    index: *pane_index,
                    last,
                },
            });
            rows.push(TreeRow {
                item,
                workspace_id: group.clone(),
                kind: RowKind::Detail {
                    index: *pane_index,
                    last,
                },
            });
        }
    }
    rows
}

pub fn items(rows: &[TreeRow]) -> Vec<Item> {
    let mut items = Vec::new();
    for row in rows {
        if let Some(item) = &row.item
            && items.last() != Some(item)
        {
            items.push(item.clone());
        }
    }
    items
}

pub fn item_band(rows: &[TreeRow], item: &Item) -> Option<std::ops::RangeInclusive<usize>> {
    let first = rows
        .iter()
        .position(|row| row.item.as_ref() == Some(item))?;
    let last = rows
        .iter()
        .rposition(|row| row.item.as_ref() == Some(item))?;
    Some(first..=last)
}

/// Hit testing follows the rows occupied by the actual item.
pub fn item_at(rows: &[TreeRow], index: usize) -> Option<Item> {
    rows.get(index)?.item.clone()
}

fn unsafe_label_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200b}'
                | '\u{200e}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// Metadata is untrusted text: strip terminal controls, bidi overrides and hidden separators.
pub fn clean(text: &str) -> std::borrow::Cow<'_, str> {
    for (index, character) in text.chars().enumerate() {
        if index == 1024 || unsafe_label_character(character) {
            return std::borrow::Cow::Owned(
                text.chars()
                    .filter(|c| !unsafe_label_character(*c))
                    .take(1024)
                    .collect(),
            );
        }
    }
    std::borrow::Cow::Borrowed(text)
}

fn branch_label(pane: &bkpr::Pane) -> Option<std::borrow::Cow<'_, str>> {
    pane.branch
        .as_deref()
        .map(clean)
        .filter(|branch| !branch.trim().is_empty())
}

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let text = clean(text);
    // ASCII has no contextual or double-width cells; avoid rescanning every prefix.
    if text.is_ascii() {
        return if text.len() <= columns {
            text.into_owned()
        } else {
            format!("{}…", &text[..columns - 1])
        };
    }
    if width(&text) <= columns {
        return text.into_owned();
    }
    let budget = columns - 1;
    let mut end = 0;
    for (offset, character) in text.char_indices() {
        let next = offset + character.len_utf8();
        if width(&text[..next]) > budget {
            break;
        }
        end = next;
    }
    format!("{}…", &text[..end])
}

pub fn fit(text: &str, columns: usize) -> String {
    let mut text = truncate(text, columns);
    let padding = columns.saturating_sub(width(&text));
    text.extend(std::iter::repeat_n(' ', padding));
    text
}

pub fn status_color(status: Status, theme: &Theme) -> &str {
    match status {
        Status::Idle => &theme.accents[2],
        Status::Working => &theme.accents[3],
        Status::Blocked => &theme.accents[1],
        Status::Done => &theme.accents[6],
        Status::Unknown => &theme.muted,
    }
}

/// The native owner assigns slots once; painting only borrows a cached terminal style.
pub fn project_color<'a>(project: Option<&ProjectIdentity>, theme: &'a Theme) -> &'a str {
    theme.project_color(
        project
            .filter(|project| project.repository)
            .and_then(|project| project.color_slot),
    )
}

fn ci_state(ci: &CiSnapshot) -> CiStatus {
    if ci.fetch_state != CiFetchState::Ready || ci.stale || ci.error.is_some() || ci.truncated {
        CiStatus::Unknown
    } else {
        ci.status
    }
}

fn ci_glyph(status: CiStatus) -> &'static str {
    match status {
        CiStatus::NoChecks => "\u{f0377}",  // nf-md-minus_circle_outline
        CiStatus::Pending => "\u{f0150}",   // nf-md-clock_outline
        CiStatus::Running => "\u{f0996}",   // nf-md-progress_clock
        CiStatus::Passed => "\u{f05e1}",    // nf-md-check_circle_outline
        CiStatus::Failed => "\u{f015a}",    // nf-md-close_circle_outline
        CiStatus::Cancelled => "\u{f073a}", // nf-md-cancel
        CiStatus::Skipped => "\u{f0662}",   // nf-md-skip_next_circle_outline
        CiStatus::Unknown => "\u{f0625}",   // nf-md-help_circle_outline
    }
}

fn ci_color(status: CiStatus, theme: &Theme) -> &str {
    match status {
        CiStatus::Failed => status_color(Status::Blocked, theme),
        CiStatus::Passed => status_color(Status::Idle, theme),
        CiStatus::Pending | CiStatus::Running => status_color(Status::Working, theme),
        _ => &theme.muted,
    }
}

fn ci_badge(columns: usize) -> String {
    let badge = format!("{WORKFLOW} ");
    if width(&badge) <= columns {
        badge
    } else {
        String::new()
    }
}

/// Reserve the complete pair, or omit it; never truncate a line-count number.
fn branch_counts(
    changes: Option<&bkpr::BranchChanges>,
    columns: usize,
    theme: &Theme,
) -> Option<(String, usize)> {
    let total = changes?.total;
    let digits = |number: u64| {
        number
            .checked_ilog10()
            .map_or(1, |power| power as usize + 1)
    };
    let needed = digits(total.added) + digits(total.deleted) + 3;
    (needed <= columns).then(|| {
        (
            format!(
                "{added}+{}{muted} {deleted}-{}{muted}",
                total.added,
                total.deleted,
                added = theme.added,
                deleted = theme.deleted,
                muted = theme.muted,
            ),
            needed,
        )
    })
}

/// Same source shared by several panes is one CI group, never several independent votes.
fn workspace_ci<'a>(
    state: &'a Snapshot,
    workspace: &str,
) -> Vec<(&'a ProjectIdentity, &'a CiSnapshot)> {
    let mut groups = std::collections::BTreeMap::new();
    for pane in state
        .panes
        .iter()
        .filter(|pane| pane.workspace_id == workspace)
    {
        let (Some(project), Some(ci)) = (&pane.project, &pane.ci) else {
            continue;
        };
        let key = (
            &project.id,
            &ci.repository,
            &ci.head_sha,
            &ci.checked_ref,
            &ci.local_head_sha,
            ci.change_request.as_ref().map(|request| request.number),
        );
        let entry = groups.entry(key).or_insert((project, ci));
        if ci.updated_at > entry.1.updated_at
            || (ci.updated_at == entry.1.updated_at && ci_state(ci) == CiStatus::Unknown)
        {
            *entry = (project, ci);
        }
    }
    groups.into_values().collect()
}

fn project_labels(projects: &[ProjectIdentity], columns: usize, theme: &Theme) -> String {
    let mut result = String::new();
    let mut used = 0;
    for (index, project) in projects.iter().enumerate() {
        if used >= columns {
            break;
        }
        if index > 0 {
            let separator = truncate(", ", columns - used);
            used += width(&separator);
            result.push_str(&theme.muted);
            result.push_str(&separator);
        }
        let name = truncate(&project.name, columns.saturating_sub(used));
        used += width(&name);
        result.push_str(project_color(Some(project), theme));
        result.push_str(&name);
        if name.as_str() != clean(&project.name).as_ref() {
            break;
        }
    }
    result.push_str(&" ".repeat(columns.saturating_sub(used)));
    result
}

/// Popup text stays separate from terminal styling across the plugin pipe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupContent {
    pub key: String,
    pub title: PopupRow,
    pub subtitle: PopupRow,
    #[serde(default)]
    pub intro: Vec<PopupRow>,
    pub sections: Vec<PopupSection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PopupColor {
    Text,
    Muted,
    Added,
    Deleted,
    Native(Status),
    Ci(CiStatus),
    Project(ProjectIdentity),
    Harness(String),
    ChangeRequest(ChangeRequestState),
}

impl PopupColor {
    pub fn ansi<'a>(&self, theme: &'a Theme) -> &'a str {
        match self {
            Self::Text => TEXT,
            Self::Muted => &theme.muted,
            Self::Added => &theme.added,
            Self::Deleted => &theme.deleted,
            Self::Native(status) => status_color(*status, theme),
            Self::Ci(status) => ci_color(*status, theme),
            Self::Project(project) => project_color(Some(project), theme),
            Self::Harness(harness) => harness_color(harness, theme),
            Self::ChangeRequest(state) => request_presentation(*state)
                .1
                .map_or(theme.muted.as_str(), |index| theme.accents[index].as_str()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupSpan {
    pub text: String,
    pub color: PopupColor,
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub working_clock: bool,
}

impl PopupSpan {
    fn new(text: impl Into<String>, color: PopupColor) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
            working_clock: false,
        }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupRow {
    pub spans: Vec<PopupSpan>,
    /// Body suffix kept visible when a long branch name is truncated.
    #[serde(default)]
    pub suffix: Vec<PopupSpan>,
    /// Unlinked metadata aligned to the right edge, independently of the body.
    #[serde(default)]
    pub right: Vec<PopupSpan>,
    /// Refresh is an exact repository/HEAD action, never inferred from a hyperlink.
    #[serde(default)]
    pub refresh: Option<CiRefreshTarget>,
    /// Check names occupy one row; explanatory context and warnings may wrap.
    pub truncate: bool,
    /// Validated HTTPS metadata scoped only to body glyphs, never padding or chrome.
    pub link: Option<String>,
}

impl PopupRow {
    fn text(text: impl Into<String>, color: PopupColor) -> Self {
        Self {
            spans: vec![PopupSpan::new(text, color)],
            ..Self::default()
        }
    }
    fn heading(text: impl Into<String>) -> Self {
        Self {
            spans: vec![PopupSpan::new(text, PopupColor::Text).bold()],
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupSection {
    pub heading: Option<PopupRow>,
    pub rows: Vec<PopupRow>,
}

impl PopupSection {
    fn named(name: &str, rows: Vec<PopupRow>) -> Self {
        Self {
            heading: Some(PopupRow::heading(name)),
            rows,
        }
    }
}

impl PopupContent {
    pub fn rows(&self) -> Vec<PopupRow> {
        let mut rows = vec![self.title.clone()];
        if !self.subtitle.spans.is_empty()
            || !self.subtitle.suffix.is_empty()
            || !self.subtitle.right.is_empty()
        {
            rows.push(self.subtitle.clone());
        }
        rows.extend(self.intro.iter().cloned());
        for section in &self.sections {
            rows.push(PopupRow::default());
            if let Some(heading) = &section.heading {
                rows.push(heading.clone());
            }
            rows.extend(section.rows.iter().cloned());
        }
        rows
    }

    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }
}

fn native_summary(status: Status, attention: Option<Attention>) -> PopupRow {
    let mut text = format!(" {}", status.label());
    if status == Status::Blocked {
        text.push_str(" · ");
        text.push_str(attention.map(Attention::label).unwrap_or("Needs attention"));
    }
    let mut icon = PopupSpan::new(status_icon(status, attention), PopupColor::Native(status));
    icon.working_clock = status == Status::Working;
    PopupRow {
        spans: vec![icon, PopupSpan::new(text, PopupColor::Native(status))],
        ..PopupRow::default()
    }
}

fn freshness(ci: &CiSnapshot) -> String {
    if ci.updated_at == 0 {
        return "not yet fetched".to_owned();
    }
    let Some(age) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|now| now.as_secs().saturating_sub(ci.updated_at))
    else {
        return "age unavailable".to_owned();
    };
    match age {
        0..60 => format!("{age}s ago"),
        60..3600 => format!("{}m ago", age / 60),
        3600..86400 => format!("{}h ago", age / 3600),
        _ => format!("{}d ago", age / 86400),
    }
}

pub fn safe_link(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split('/').next()?;
    let host = if let Some((host, port)) = authority.split_once(':') {
        port.parse::<u16>().ok().filter(|port| *port != 0)?;
        host
    } else {
        authority
    };
    if host.trim_end_matches('.').split('.').any(|label| {
        label.is_empty()
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }) || url.len() > 4096
        || url
            .chars()
            .any(|c| c == '\\' || c.is_control() || c.is_whitespace() || unsafe_label_character(c))
    {
        return None;
    }
    Some(url)
}

fn provider_icon(provider: ForgeProvider) -> &'static str {
    match provider {
        ForgeProvider::Github => "\u{f09b}", // nf-fa-github
    }
}

fn request_presentation(state: ChangeRequestState) -> (&'static str, Option<usize>) {
    match state {
        ChangeRequestState::Draft => ("\u{f4dd}", None), // nf-oct-git_pull_request_draft
        ChangeRequestState::Open => ("\u{f407}", Some(2)),
        ChangeRequestState::Merged => ("\u{f419}", Some(5)),
        ChangeRequestState::Closed => ("\u{f4dc}", Some(1)),
    }
}

fn forge_repository(ci: &CiSnapshot, project: Option<&ProjectIdentity>) -> PopupRow {
    let identity = match &ci.change_request {
        Some(request) => format!(
            "{} {} #{}",
            provider_icon(ci.repository.provider),
            ci.repository.path,
            request.number
        ),
        None => format!(
            "{} {}",
            provider_icon(ci.repository.provider),
            ci.repository.path
        ),
    };
    let mut repository = PopupRow::text(
        identity,
        project
            .cloned()
            .map(PopupColor::Project)
            .unwrap_or(PopupColor::Text),
    );
    repository.link = safe_link(
        ci.change_request
            .as_ref()
            .map(|request| request.url.as_str())
            .unwrap_or(&ci.repository.web_url),
    )
    .map(str::to_owned);
    repository
}

fn request_row(request: &bkpr::ci_contract::ChangeRequest) -> PopupRow {
    PopupRow {
        spans: vec![
            PopupSpan::new(
                format!("{} ", request_presentation(request.state).0),
                PopupColor::ChangeRequest(request.state),
            ),
            PopupSpan::new(request.title.clone(), PopupColor::Text),
        ],
        link: safe_link(&request.url).map(str::to_owned),
        ..PopupRow::default()
    }
}

fn issue_row(issue: &LinkedIssue) -> PopupRow {
    let icon = match issue.provider {
        IssueTracker::Linear => "\u{f02e}", // nf-fa-bookmark
    };
    let mut label = format!("{icon} {}", issue.identifier);
    if let Some(title) = issue
        .title
        .as_deref()
        .filter(|title| !clean(title).trim().is_empty())
    {
        label.push_str(" · ");
        label.push_str(title);
    }
    PopupRow {
        spans: vec![PopupSpan::new(label, PopupColor::Text)],
        link: safe_link(&issue.url).map(str::to_owned),
        ..PopupRow::default()
    }
}

fn forge_context(
    ci: &CiSnapshot,
    project: Option<&ProjectIdentity>,
    branches: Vec<PopupRow>,
) -> Vec<PopupRow> {
    let mut rows = vec![forge_repository(ci, project)];
    if let Some(request) = &ci.change_request {
        rows.push(request_row(request));
        rows.extend(request.issues.iter().map(issue_row));
    }
    rows.extend(branches);
    rows
}

fn duration(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".to_owned(),
        Some(0) => "0s".to_owned(),
        Some(seconds @ 1..60) => format!("{seconds}s"),
        Some(seconds @ 60..3600) => format!("{}:{:02}", seconds / 60, seconds % 60),
        Some(seconds) => {
            let hours = seconds / 3600;
            let minutes = seconds % 3600 / 60;
            if minutes == 0 {
                format!("{hours}h")
            } else {
                format!("{hours}h {minutes}m")
            }
        }
    }
}

pub fn refresh_button_text() -> &'static str {
    "[ \u{f021}  ]"
}

pub fn refresh_button_width() -> usize {
    6
}

fn ci_summary(ci: &CiSnapshot) -> PopupRow {
    let status = ci_state(ci);
    let label = match ci.fetch_state {
        CiFetchState::Loading => "Loading",
        CiFetchState::Unavailable => "Unavailable",
        CiFetchState::Ready => "Ready",
    };
    let mut summary = PopupRow {
        spans: vec![
            PopupSpan::new(
                format!("{WORKFLOW} CI · {} {label}", ci_glyph(status)),
                PopupColor::Ci(status),
            )
            .bold(),
        ],
        ..PopupRow::default()
    };
    if !ci.repository.host.is_empty() && !ci.repository.path.is_empty() && !ci.head_sha.is_empty() {
        summary.right.push(PopupSpan::new(
            refresh_button_text(),
            if ci.fetch_state == CiFetchState::Loading {
                PopupColor::Muted
            } else {
                PopupColor::Text
            },
        ));
        if ci.fetch_state != CiFetchState::Loading {
            summary.refresh = Some(CiRefreshTarget {
                repository: ci.repository.clone(),
                head_sha: ci.head_sha.clone(),
            });
        }
    }
    if ci.fetch_state == CiFetchState::Ready {
        summary.spans.push(PopupSpan::new(
            format!(" · {}", status.label()),
            PopupColor::Ci(status),
        ));
    }
    if ci.updated_at != 0 {
        summary.spans.push(PopupSpan::new(
            format!(" · {}", freshness(ci)),
            PopupColor::Muted,
        ));
    }
    if ci.stale {
        summary.spans.push(PopupSpan::new(
            " · stale",
            PopupColor::Native(Status::Working),
        ));
    }
    summary.truncate = true;
    if ci.truncated {
        summary
            .spans
            .push(PopupSpan::new(" · incomplete", PopupColor::Muted));
    }
    if let Some(error) = &ci.error {
        let detail = error
            .split_whitespace()
            .map(|word| {
                if word.starts_with("https://") || word.starts_with("http://") {
                    "[provider link]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        summary.spans.push(PopupSpan::new(
            format!(" · {detail}"),
            PopupColor::Native(Status::Blocked),
        ));
    }
    summary
}

fn ci_target_rows(ci: &CiSnapshot) -> Vec<PopupRow> {
    let target = match (ci.checked_ref.as_deref(), ci.head_sha.is_empty()) {
        (Some(reference), false) => format!("Checks for {reference} @ {:.7}", ci.head_sha),
        (Some(reference), true) => format!("Remote target {reference} · commit unavailable"),
        (None, false) => format!("Checks for commit @ {:.7}", ci.head_sha),
        (None, true) => "Checked commit unavailable".to_owned(),
    };
    let mut rows = vec![PopupRow::text(target, PopupColor::Muted)];
    if !ci.head_sha.is_empty() && !ci.local_head_sha.is_empty() && ci.head_sha != ci.local_head_sha
    {
        rows.push(PopupRow::text(
            format!(
                "Local HEAD @ {:.7} · differs from checked commit",
                ci.local_head_sha
            ),
            PopupColor::Muted,
        ));
    }
    rows
}

fn ci_rows(ci: &CiSnapshot) -> Vec<PopupRow> {
    let status = ci_state(ci);
    let mut rows = vec![ci_summary(ci)];
    rows.extend(ci_target_rows(ci));
    if let Some(error) = &ci.error {
        let detail = error
            .split_whitespace()
            .map(|word| {
                if word.starts_with("https://") || word.starts_with("http://") {
                    "[provider link]"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        rows.push(PopupRow::text(
            format!("Checks unavailable: {detail}"),
            PopupColor::Native(Status::Blocked),
        ));
    }
    if ci.stale {
        rows.push(PopupRow::text(
            "Cached results; current check state is not confirmed",
            PopupColor::Native(Status::Working),
        ));
    }
    if ci.truncated {
        rows.push(PopupRow::text(
            "Checks incomplete: provider pagination limit reached",
            PopupColor::Native(Status::Working),
        ));
    }
    if ci.checks.is_empty() {
        rows.push(PopupRow::text(
            if ci.fetch_state == CiFetchState::Loading {
                "Fetching checks…"
            } else if ci.status == CiStatus::NoChecks && status == CiStatus::NoChecks {
                "No checks reported for this commit"
            } else {
                "No check results available"
            },
            PopupColor::Muted,
        ));
    }
    for check in &ci.checks {
        let link = check.url.as_deref().and_then(safe_link).map(str::to_owned);
        let spans = vec![
            PopupSpan::new(
                format!("{} ", ci_glyph(check.status)),
                PopupColor::Ci(check.status),
            ),
            PopupSpan::new(check.name.clone(), PopupColor::Text),
        ];
        rows.push(PopupRow {
            spans,
            right: vec![PopupSpan::new(
                duration(check.duration_seconds),
                PopupColor::Muted,
            )],
            truncate: true,
            link,
            ..PopupRow::default()
        });
    }
    rows
}

pub fn working_clock_icon(frame: usize) -> &'static str {
    const FRAMES: [&str; 12] = [
        "\u{f1456} ", // nf-md-clock_time_twelve_outline
        "\u{f144b} ", // nf-md-clock_time_one_outline
        "\u{f144c} ",
        "\u{f144d} ",
        "\u{f144e} ",
        "\u{f144f} ",
        "\u{f1450} ",
        "\u{f1451} ",
        "\u{f1452} ",
        "\u{f1453} ",
        "\u{f1454} ",
        "\u{f1455} ", // nf-md-clock_time_eleven_outline
    ];
    FRAMES[frame % FRAMES.len()]
}

fn status_icon(status: Status, attention: Option<Attention>) -> &'static str {
    // Reserve a trailing cell for Nerd Font glyphs that paint double-width.
    match status {
        Status::Idle => "\u{f03e6} ", // nf-md-pause_circle_outline
        Status::Working => working_clock_icon(0),
        Status::Blocked => match attention {
            Some(Attention::Permission) => "\u{f0bc5} ", // nf-md-shield_key_outline
            Some(Attention::Question) => "\u{f0186} ",   // nf-md-comment_question_outline
            Some(Attention::Auth) => "\u{f0341} ",       // nf-md-lock_outline
            Some(Attention::Error) => "\u{f05d6} ",      // nf-md-alert_circle_outline
            None => "\u{f002a} ",                        // nf-md-alert_outline
        },
        Status::Done => "\u{f05e1} ",    // nf-md-check_circle_outline
        Status::Unknown => "\u{f0625} ", // nf-md-help_circle_outline
    }
}

fn pane_harness(pane: &bkpr::Pane) -> Option<&str> {
    pane.harness
        .as_deref()
        .filter(|harness| harness_name(harness).is_some())
}

fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "claude" | "claude-code" => "\u{ec82}",   // nf-cod-claude
        "omp" => "\u{f03ff}",                     // nf-md-pi
        "codex" => "\u{ec81}",                    // nf-cod-openai
        "opencode" => "\u{f0174}",                // nf-md-code_tags
        "agy" | "antigravity" => "\u{f0463}",     // nf-md-rocket
        "cursor" | "cursor-agent" => "\u{f01bf}", // nf-md-cursor_default_outline
        "copilot" | "copilot-cli" | "github-copilot" => "\u{ec1e}", // nf-cod-copilot
        "aider" => "\u{f03eb}",                   // nf-md-pencil
        "cline" => "\u{f06a9}",                   // nf-md-robot
        "goose" => "\u{f01e5}",                   // nf-md-duck
        _ => "\u{f167a}",                         // nf-md-robot_outline
    }
}

fn harness_color<'a>(harness: &str, theme: &'a Theme) -> &'a str {
    match harness {
        "claude" | "claude-code" => &theme.accents[3],
        "omp" => &theme.accents[5],
        "codex" => &theme.accents[2],
        "opencode" => &theme.accents[4],
        "agy" | "antigravity" => &theme.accents[6],
        "cursor" | "cursor-agent" => TEXT,
        "copilot" | "copilot-cli" | "github-copilot" => &theme.accents[12],
        "aider" => &theme.accents[2],
        "cline" => &theme.accents[14],
        "goose" => &theme.accents[3],
        _ => TEXT,
    }
}

#[derive(Clone, Copy, Default)]
pub struct RowStyle {
    pub selected: bool,
    pub hovered: bool,
    pub selected_group: bool,
    pub clock_frame: usize,
}

pub fn render_row(
    row: &TreeRow,
    state: &Snapshot,
    theme: &Theme,
    columns: usize,
    style: RowStyle,
) -> String {
    // Keep a left gutter for the hover outline; the rightmost cell belongs to the item.
    let left = if columns > 0 { " " } else { "" };
    let content = render_content_row(row, state, theme, columns.saturating_sub(1), style);
    if content.is_empty() {
        return content;
    }
    let background = if style.selected {
        theme.selected.as_str()
    } else if style.hovered {
        theme.hovered.as_str()
    } else {
        ""
    };
    format!("{RESET}{}{background}{left}{content}{RESET}", theme.base)
}

fn render_content_row(
    row: &TreeRow,
    state: &Snapshot,
    theme: &Theme,
    columns: usize,
    style: RowStyle,
) -> String {
    let RowStyle {
        selected,
        hovered,
        selected_group: selected_pane_group,
        clock_frame,
    } = style;
    let muted = theme.muted.as_str();
    let background = if selected {
        theme.selected.as_str()
    } else if hovered {
        theme.hovered.as_str()
    } else {
        ""
    };
    let guides = if selected || selected_pane_group {
        theme.bright.as_str()
    } else {
        theme.muted.as_str()
    };
    let body = match &row.kind {
        RowKind::Spacer => return String::new(),
        RowKind::Workspace { index, collapsed } => {
            let workspace = &state.workspaces[*index];
            let dot = truncate("● ", columns);
            let icon = if *collapsed
                && state
                    .panes
                    .iter()
                    .any(|pane| pane.workspace_id == workspace.id && pane_harness(pane).is_some())
            {
                truncate(
                    if workspace.status == Status::Working {
                        working_clock_icon(clock_frame)
                    } else {
                        status_icon(workspace.status, workspace.attention)
                    },
                    columns.saturating_sub(width(&dot)),
                )
            } else {
                String::new()
            };
            let available = columns.saturating_sub(width(&dot) + width(&icon));
            let ci_status = if *collapsed {
                let groups = workspace_ci(state, &workspace.id);
                (!groups.is_empty())
                    .then(|| CiStatus::aggregate(groups.iter().map(|(_, ci)| ci_state(ci))))
            } else {
                None
            };
            let badge = ci_status
                .map(|_| ci_badge(available.saturating_sub(2)))
                .unwrap_or_default();
            let badge_columns = width(&badge) + usize::from(!badge.is_empty());
            let name = truncate(&workspace.name, available.saturating_sub(badge_columns));
            let fill = available.saturating_sub(width(&name) + badge_columns);
            let tail = if *collapsed {
                "·".repeat(fill)
            } else {
                " ".repeat(fill)
            };
            format!(
                "{TEXT}{dot}{TEXT}\u{1b}[1m{name}\u{1b}[22m{muted}{tail}{}{}{badge}{}{icon}",
                if badge.is_empty() { "" } else { " " },
                ci_status
                    .map(|status| ci_color(status, theme))
                    .unwrap_or(muted),
                if icon.is_empty() {
                    muted
                } else {
                    status_color(workspace.status, theme)
                },
            )
        }
        RowKind::Projects {
            index,
            has_children,
        } => {
            let projects = &state.workspaces[*index].projects;
            let lead = truncate(if *has_children { "│ " } else { "  " }, columns);
            format!(
                "{guides}{lead}{muted}{}",
                project_labels(projects, columns.saturating_sub(width(&lead)), theme)
            )
        }
        RowKind::Pane { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { "╰─╴" } else { "├─╴" }, columns);
            let indicator = truncate("● ", columns.saturating_sub(width(&lead)));
            let harness = pane_harness(pane);
            let status = if harness.is_some() {
                truncate(
                    if pane.status == Status::Working {
                        working_clock_icon(clock_frame)
                    } else {
                        status_icon(pane.status, pane.attention)
                    },
                    columns.saturating_sub(width(&lead) + width(&indicator)),
                )
            } else {
                String::new()
            };
            let available =
                columns.saturating_sub(width(&lead) + width(&indicator) + width(&status));
            // Keep the harness beside its status, with a trailing cell for wide glyphs.
            let icon = harness.map(harness_icon).unwrap_or("");
            let logo_color = harness
                .map(|harness| harness_color(harness, theme))
                .unwrap_or(TEXT);
            let icon_columns = width(icon) + usize::from(!icon.is_empty());
            let (icon, icon_columns) = if available >= icon_columns {
                (icon, icon_columns)
            } else {
                ("", 0)
            };
            let name = fit(&pane.name, available.saturating_sub(icon_columns));
            format!(
                "{guides}{lead}{}{indicator}{logo_color}{icon}{}{TEXT}{name}{}{status}",
                project_color(pane.project.as_ref(), theme),
                if icon.is_empty() { "" } else { " " },
                if status.is_empty() {
                    muted
                } else {
                    status_color(pane.status, theme)
                },
            )
        }
        RowKind::Detail { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { "     " } else { "│    " }, columns);
            let available = columns.saturating_sub(width(&lead));
            let Some(branch) = branch_label(pane) else {
                return format!(
                    "{RESET}{}{background}{guides}{lead}{muted}{}{RESET}",
                    theme.base,
                    fit(&pane.description, available)
                );
            };
            let request = pane.ci.as_ref().and_then(|ci| ci.change_request.as_ref());
            let (glyph, glyph_color) = request
                .map(|request| {
                    let (glyph, color) = request_presentation(request.state);
                    (
                        glyph,
                        color.map_or(muted, |index| theme.accents[index].as_str()),
                    )
                })
                .unwrap_or(("\u{f062c}", muted));
            let symbol_columns = width(glyph) + 1;
            let (symbol, symbol_space, symbol_columns) = if available >= symbol_columns {
                (glyph, " ", symbol_columns)
            } else {
                ("", "", 0)
            };
            let ci_status = pane.ci.as_ref().map(ci_state);
            let badge = ci_status
                .map(|_| ci_badge(available.saturating_sub(symbol_columns + 2)))
                .unwrap_or_default();
            let badge_columns = width(&badge) + usize::from(!badge.is_empty());
            let content_columns = available.saturating_sub(symbol_columns + badge_columns);
            let counts = branch_counts(
                pane.branch_changes.as_ref(),
                content_columns.saturating_sub(3),
                theme,
            );
            let counts_columns = counts.as_ref().map_or(0, |(_, columns)| columns + 1);
            let branch = truncate(&branch, content_columns.saturating_sub(counts_columns));
            let fill = content_columns.saturating_sub(width(&branch) + counts_columns);
            format!(
                "{guides}{lead}{glyph_color}{symbol}{symbol_space}{muted}{branch}{}{}{}{}{}{badge}",
                if counts.is_some() { " " } else { "" },
                counts.map(|(text, _)| text).unwrap_or_default(),
                " ".repeat(fill),
                if badge.is_empty() { "" } else { " " },
                ci_status
                    .map(|status| ci_color(status, theme))
                    .unwrap_or(muted),
            )
        }
    };
    format!("{RESET}{}{background}{body}{RESET}", theme.base)
}

fn repository_heading(project: &ProjectIdentity, ci: Option<&CiSnapshot>) -> PopupRow {
    ci.map(|ci| forge_repository(ci, Some(project)))
        .unwrap_or_else(|| {
            PopupRow::text(project.name.clone(), PopupColor::Project(project.clone()))
        })
}

fn branch_rows(
    branch: Option<&str>,
    changes: Option<&bkpr::BranchChanges>,
    ci: Option<&CiSnapshot>,
) -> Vec<PopupRow> {
    let mut row = PopupRow::text(
        format!(
            "\u{f062c} {}",
            branch
                .filter(|branch| !clean(branch).trim().is_empty())
                .unwrap_or("Branch unavailable")
        ),
        PopupColor::Text,
    );
    row.truncate = true;
    let base = ci
        .and_then(|ci| ci.change_request.as_ref())
        .and_then(|request| request.base.as_ref())
        .map(|base| base.branch.as_str())
        .or_else(|| changes.map(|changes| changes.base_ref.as_str()));
    if let Some(base) = base {
        let base = base
            .strip_prefix("refs/heads/")
            .or_else(|| base.strip_prefix("refs/remotes/"))
            .unwrap_or(base);
        row.right = vec![PopupSpan::new(
            format!("\u{f419} {base}"),
            PopupColor::Muted,
        )];
    }
    let Some(changes) = changes else {
        return vec![
            row,
            PopupRow::text("Branch changes unavailable", PopupColor::Muted),
        ];
    };
    row.suffix = vec![
        PopupSpan::new(" ", PopupColor::Muted),
        PopupSpan::new(format!("+{}", changes.total.added), PopupColor::Added),
        PopupSpan::new(" ", PopupColor::Muted),
        PopupSpan::new(format!("-{}", changes.total.deleted), PopupColor::Deleted),
    ];
    let mut rows = vec![row];
    if changes.total.binary_files != 0 {
        rows.push(PopupRow::text(
            format!("Binary files: {} (no lines)", changes.total.binary_files),
            PopupColor::Muted,
        ));
    }
    rows
}

pub fn tooltip(item: &Item, state: &Snapshot) -> PopupContent {
    match item {
        Item::Workspace(id) => state
            .workspaces
            .iter()
            .find(|w| &w.id == id)
            .map(|workspace| {
                let panes: Vec<_> = state
                    .panes
                    .iter()
                    .filter(|p| &p.workspace_id == id)
                    .collect();
                let subtitle = if panes.iter().any(|pane| pane_harness(pane).is_some()) {
                    let mut subtitle = native_summary(workspace.status, workspace.attention);
                    subtitle.spans.push(PopupSpan::new(
                        format!(" · {} terminals", panes.len()),
                        PopupColor::Muted,
                    ));
                    subtitle
                } else {
                    PopupRow::text(format!("{} terminals", panes.len()), PopupColor::Muted)
                };
                let groups = workspace_ci(state, id);
                let mut sections = Vec::new();
                for project in &workspace.projects {
                    let sources: Vec<_> = groups
                        .iter()
                        .filter(|(identity, _)| identity.id == project.id)
                        .collect();
                    let mut rows = Vec::new();
                    let mut requests = std::collections::BTreeSet::new();
                    for (index, (_, ci)) in sources.iter().enumerate() {
                        if let Some(request) = &ci.change_request
                            && requests.insert(request.number)
                        {
                            if index != 0 {
                                rows.push(forge_repository(ci, Some(project)));
                            }
                            rows.push(request_row(request));
                            rows.extend(request.issues.iter().map(issue_row));
                        }
                    }
                    let mut contexts: Vec<&bkpr::Pane> = Vec::new();
                    for pane in &panes {
                        if pane
                            .project
                            .as_ref()
                            .is_none_or(|identity| identity.id != project.id)
                            || contexts.iter().any(|prior| {
                                prior.branch == pane.branch
                                    && prior.branch_changes == pane.branch_changes
                            })
                        {
                            continue;
                        }
                        contexts.push(pane);
                        if project.repository {
                            rows.extend(branch_rows(
                                pane.branch.as_deref().or_else(|| {
                                    pane.ci.as_ref().and_then(|ci| ci.branch.as_deref())
                                }),
                                pane.branch_changes.as_ref(),
                                pane.ci.as_ref(),
                            ));
                        }
                    }
                    if project.repository && contexts.is_empty() {
                        rows.extend(branch_rows(None, None, None));
                    }
                    rows.push(PopupRow::default());
                    rows.push(PopupRow::heading("Checks"));
                    if sources.is_empty() {
                        rows.push(PopupRow::text(
                            if project.repository {
                                "Checks not available"
                            } else {
                                "Local directory · not a Git repository"
                            },
                            PopupColor::Muted,
                        ));
                    } else {
                        for (_, ci) in &sources {
                            let mut summary = ci_summary(ci);
                            if sources.len() > 1
                                && let Some(branch) = ci.branch.as_deref()
                            {
                                summary.spans.insert(
                                    0,
                                    PopupSpan::new(format!("{branch} · "), PopupColor::Muted),
                                );
                            }
                            rows.push(summary);
                            rows.extend(ci_target_rows(ci));
                        }
                    }
                    sections.push(PopupSection {
                        heading: Some(repository_heading(
                            project,
                            sources.first().map(|(_, ci)| *ci),
                        )),
                        rows,
                    });
                }
                if sections.is_empty() {
                    sections.push(PopupSection::named(
                        "Context",
                        vec![PopupRow::text(
                            "No project metadata available",
                            PopupColor::Muted,
                        )],
                    ));
                }
                PopupContent {
                    key: format!("workspace:{id}"),
                    title: PopupRow {
                        spans: vec![
                            PopupSpan::new(workspace.name.clone(), PopupColor::Text).bold(),
                        ],
                        ..PopupRow::default()
                    },
                    subtitle,
                    intro: Vec::new(),
                    sections,
                }
            })
            .unwrap_or_default(),
        Item::Pane(id) => state
            .panes
            .iter()
            .find(|p| &p.id == id)
            .map(|pane| {
                let mut title = PopupRow::default();
                let harness = pane_harness(pane);
                if let Some(harness) = harness {
                    title.spans.push(
                        PopupSpan::new(
                            format!("{} ", harness_icon(harness)),
                            PopupColor::Harness(harness.to_owned()),
                        )
                        .bold(),
                    );
                }
                title
                    .spans
                    .push(PopupSpan::new(pane.name.clone(), PopupColor::Text).bold());
                let subtitle = if let Some(name) = harness.and_then(harness_name) {
                    let mut subtitle = native_summary(pane.status, pane.attention);
                    subtitle
                        .spans
                        .push(PopupSpan::new(format!(" · {name}"), PopupColor::Muted));
                    subtitle
                } else {
                    PopupRow::default()
                };
                let mut intro = Vec::new();
                if let Some(cwd) = pane.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
                    intro.push(PopupRow::text(format!("\u{f07b} {cwd}"), PopupColor::Muted));
                }
                if let Some(agent_id) = pane.agent_id.as_deref().filter(|_| harness.is_some()) {
                    intro.push(PopupRow {
                        spans: vec![
                            PopupSpan::new("\u{f0efe} Agent ID: ", PopupColor::Muted), // nf-md-identifier
                            PopupSpan::new(agent_id, PopupColor::Text),
                        ],
                        ..PopupRow::default()
                    });
                }
                if !pane.description.trim().is_empty() {
                    intro.push(PopupRow {
                        spans: vec![
                            PopupSpan::new("Activity: ", PopupColor::Muted),
                            PopupSpan::new(pane.description.clone(), PopupColor::Text),
                        ],
                        ..PopupRow::default()
                    });
                }
                let branches = if pane.branch_changes.is_some()
                    || pane.branch.is_some()
                    || pane.ci.is_some()
                    || pane
                        .project
                        .as_ref()
                        .is_some_and(|project| project.repository)
                {
                    branch_rows(
                        pane.branch
                            .as_deref()
                            .or_else(|| pane.ci.as_ref().and_then(|ci| ci.branch.as_deref())),
                        pane.branch_changes.as_ref(),
                        pane.ci.as_ref(),
                    )
                } else {
                    Vec::new()
                };
                let repository = if let Some(ci) = &pane.ci {
                    forge_context(ci, pane.project.as_ref(), branches)
                } else {
                    let mut rows = pane
                        .project
                        .as_ref()
                        .map(|project| vec![repository_heading(project, None)])
                        .unwrap_or_default();
                    rows.extend(branches);
                    rows
                };
                let mut sections = Vec::new();
                if !repository.is_empty() {
                    sections.push(PopupSection::named("Repository", repository));
                }
                if let Some(ci) = &pane.ci {
                    sections.push(PopupSection::named("Checks", ci_rows(ci)));
                } else if pane
                    .project
                    .as_ref()
                    .is_some_and(|project| project.repository)
                {
                    sections.push(PopupSection::named(
                        "Checks",
                        vec![PopupRow::text("Checks not available", PopupColor::Muted)],
                    ));
                } else {
                    sections.push(PopupSection::named(
                        "Checks",
                        vec![PopupRow::text(
                            if pane
                                .project
                                .as_ref()
                                .is_some_and(|project| !project.repository)
                            {
                                "Local directory · not a Git repository"
                            } else {
                                "Checks not available"
                            },
                            PopupColor::Muted,
                        )],
                    ));
                }
                PopupContent {
                    key: format!("pane:{id}"),
                    title,
                    subtitle,
                    intro,
                    sections,
                }
            })
            .unwrap_or_default(),
    }
}

#[derive(Default)]
pub struct Input {
    pub value: String,
    pub cursor: usize,
}

impl Input {
    pub fn key(&mut self, key: &zellij_tile::prelude::KeyWithModifier) -> bool {
        use zellij_tile::prelude::{BareKey, KeyModifier};
        if key.key_modifiers.contains(&KeyModifier::Alt)
            || key.key_modifiers.contains(&KeyModifier::Super)
        {
            return false;
        }
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        match key.bare_key {
            BareKey::Char('u') if ctrl => {
                self.value.clear();
                self.cursor = 0;
            }
            BareKey::Char(character) if !ctrl => self.insert(&character.to_string()),
            BareKey::Backspace if !ctrl => self.backspace(),
            BareKey::Delete if !ctrl => self.delete(),
            BareKey::Left if !ctrl => self.left(),
            BareKey::Right if !ctrl => self.right(),
            BareKey::Home if !ctrl => self.cursor = 0,
            BareKey::End if !ctrl => self.cursor = self.value.len(),
            _ => return false,
        }
        true
    }

    pub fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }

    pub fn insert(&mut self, text: &str) {
        let text = clean(text);
        // Keep requests and the interactive editor bounded independently of the backend.
        if self.value.len() + text.len() <= 4096 {
            self.value.insert_str(self.cursor, &text);
            self.cursor += text.len();
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.value[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.value.replace_range(self.cursor..end, "");
    }

    pub fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn visible(&self, columns: usize) -> (String, usize) {
        if columns == 0 {
            return (String::new(), 0);
        }
        let mut start = 0;
        while width(&self.value[start..self.cursor]) >= columns {
            start += self.value[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
        }
        let cursor_column = width(&self.value[start..self.cursor]);
        (fit(&self.value[start..], columns), cursor_column)
    }
}

pub enum Modal {
    Close { item: Item, name: String },
}

impl Modal {
    pub fn lines(&self) -> Vec<String> {
        match self {
            Self::Close { name, .. } => vec![
                "Close permanently?".to_owned(),
                name.clone(),
                String::new(),
                "This terminates native terminals.".to_owned(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PopupRow {
        pub fn plain(&self) -> String {
            self.spans
                .iter()
                .chain(&self.suffix)
                .map(|span| clean(&span.text))
                .collect()
        }
    }

    #[test]
    fn labels_disarm_hidden_direction_changes_without_destroying_unicode() {
        assert_eq!(
            clean("\u{061c}\u{200e}a\u{202e}b\u{202c}\u{2067}c\u{2069}\u{200b}\u{2060}\u{feff}"),
            "abc"
        );
        let legitimate = "日本語 · العربية · π · 👩\u{200d}💻 · ک\u{200c}تاب";
        assert_eq!(clean(legitimate), legitimate);
        assert_eq!(clean("name\u{1b}\n\r\u{7}"), "name");
    }

    #[test]
    fn clipped_labels_respect_cell_budgets_and_sanitize_before_slicing() {
        for (text, columns, expected) in [
            ("abcdef", 0, ""),
            ("abcdef", 1, "…"),
            ("abcdef", 4, "abc…"),
            ("abcd", 4, "abcd"),
            ("ab\ncd", 4, "abcd"),
            ("日本語", 4, "日…"),
            ("e\u{301}xyz", 3, "e\u{301}x…"),
            ("👩\u{200d}💻xy", 3, "👩\u{200d}💻…"),
        ] {
            let clipped = truncate(text, columns);
            assert_eq!(clipped, expected);
            assert!(width(&clipped) <= columns);
        }
    }

    fn identity(id: &str) -> ProjectIdentity {
        ProjectIdentity {
            id: id.to_owned(),
            name: "same-name".to_owned(),
            repository: true,
            color_slot: Some(0),
        }
    }

    fn ci(status: CiStatus) -> CiSnapshot {
        CiSnapshot {
            status,
            checks: vec![bkpr::ci_contract::CiCheck {
                name: "unit tests".to_owned(),
                status,
                url: Some("https://github.com/owner/repo/actions/runs/12".to_owned()),
                duration_seconds: Some(123),
            }],
            change_request: Some(bkpr::ci_contract::ChangeRequest {
                number: 7,
                title: "Improve project metadata".to_owned(),
                url: "https://github.com/owner/repo/pull/7".to_owned(),
                state: ChangeRequestState::Open,
                description: String::new(),
                issues: Vec::new(),
                base: None,
            }),
            fetch_state: CiFetchState::Ready,
            updated_at: 1,
            stale: false,
            error: None,
            repository: bkpr::ci_contract::ForgeRepository {
                provider: ForgeProvider::Github,
                host: "github.com".to_owned(),
                path: "owner/repo".to_owned(),
                web_url: "https://github.com/owner/repo".to_owned(),
            },
            head_sha: "0123456789abcdef".to_owned(),
            branch: Some("feature/日本語".to_owned()),
            local_head_sha: "0123456789abcdef".to_owned(),
            checked_ref: Some("origin/feature/日本語".to_owned()),
            truncated: false,
            commit_missing: false,
        }
    }

    fn snapshot() -> Snapshot {
        let project = identity("/repo/.git");
        Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision: 1,
            session: "test".to_owned(),
            connected: true,
            error: None,
            focused_pane_id: Some("1".to_owned()),
            workspaces: vec![bkpr::Workspace {
                id: "workspace".to_owned(),
                name: "日本語 workspace".to_owned(),
                active_tab_id: "tab".to_owned(),
                projects: vec![project.clone()],
                status: Status::Working,
                attention: None,
            }],
            panes: vec![bkpr::Pane {
                id: "1".to_owned(),
                terminal_id: "1".to_owned(),
                workspace_id: "workspace".to_owned(),
                tab_id: "tab".to_owned(),
                name: "日本語 agent".to_owned(),
                description: "Native activity".to_owned(),
                harness: Some("codex".to_owned()),
                agent_id: None,
                status: Status::Working,
                attention: None,
                cwd: Some("/repo".to_owned()),
                branch: Some("feature/日本語/long-long-branch".to_owned()),
                branch_changes: None,
                project: Some(project),
                ci: Some(ci(CiStatus::Passed)),
            }],
        }
    }

    fn visible(rendered: &str) -> String {
        rendered
            .split('\u{1b}')
            .map(|part| {
                part.strip_prefix('[')
                    .and_then(|sgr| sgr.split_once('m').map(|(_, text)| text))
                    .unwrap_or(part)
            })
            .collect()
    }

    #[test]
    fn working_clock_frames_rotate_clockwise_without_changing_row_geometry() {
        let frames = [
            '\u{f1456}',
            '\u{f144b}',
            '\u{f144c}',
            '\u{f144d}',
            '\u{f144e}',
            '\u{f144f}',
            '\u{f1450}',
            '\u{f1451}',
            '\u{f1452}',
            '\u{f1453}',
            '\u{f1454}',
            '\u{f1455}',
        ];
        let mut state = snapshot();
        let rows = tree(&state, &Default::default());
        let pane = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Pane { .. }))
            .unwrap();
        let theme = Theme::default();
        for (frame, glyph) in frames.into_iter().enumerate() {
            assert_eq!(working_clock_icon(frame), format!("{glyph} "));
            assert_eq!(working_clock_icon(frame + 12), working_clock_icon(frame));
            for columns in 0..=40 {
                let rendered = visible(&render_row(
                    pane,
                    &state,
                    &theme,
                    columns,
                    RowStyle {
                        selected: false,
                        hovered: false,
                        selected_group: false,
                        clock_frame: frame,
                    },
                ));
                let baseline = visible(&render_row(
                    pane,
                    &state,
                    &theme,
                    columns,
                    RowStyle {
                        selected: false,
                        hovered: false,
                        selected_group: false,
                        clock_frame: 0,
                    },
                ));
                assert_eq!(rendered.replace(glyph, "\u{f1456}"), baseline);
                assert_eq!(width(&rendered), columns);
            }
        }
        for harness in [None, Some("cargo")] {
            state.panes[0].harness = harness.map(str::to_owned);
            assert_eq!(
                render_row(pane, &state, &theme, 40, RowStyle::default()),
                render_row(
                    pane,
                    &state,
                    &theme,
                    40,
                    RowStyle {
                        clock_frame: 6,
                        ..RowStyle::default()
                    }
                ),
            );
        }
        state.panes[0].harness = Some("codex".to_owned());
        state.panes[0].status = Status::Done;
        assert_eq!(
            render_row(pane, &state, &theme, 40, RowStyle::default()),
            render_row(
                pane,
                &state,
                &theme,
                40,
                RowStyle {
                    clock_frame: 6,
                    ..RowStyle::default()
                }
            ),
        );
    }

    #[test]
    fn popup_clock_metadata_marks_only_recognized_working_harness_icons() {
        let mut state = snapshot();
        state.panes[0].ci = Some(ci(CiStatus::Running));
        state.panes[0].ci.as_mut().unwrap().stale = true;
        for harness in [None, Some("cargo"), Some("codex")] {
            state.panes[0].harness = harness.map(str::to_owned);
            for status in [Status::Idle, Status::Working, Status::Blocked, Status::Done] {
                state.panes[0].status = status;
                let popup = tooltip(&Item::Pane("1".to_owned()), &state);
                let marked: Vec<_> = popup
                    .rows()
                    .into_iter()
                    .flat_map(|row| row.spans)
                    .filter(|span| span.working_clock)
                    .collect();
                let expected = harness == Some("codex") && status == Status::Working;
                assert_eq!(marked.len(), usize::from(expected));
                if expected {
                    assert_eq!(marked[0].text, working_clock_icon(0));
                    assert!(popup.subtitle.plain().contains(status.label()));
                }
            }
        }
        let original = PopupSpan::new("label", PopupColor::Text);
        let wire = serde_json::to_string(&original).unwrap();
        assert!(!wire.contains("working_clock"));
        assert_eq!(serde_json::from_str::<PopupSpan>(&wire).unwrap(), original);
        let working = native_summary(Status::Working, None).spans.remove(0);
        let wire = serde_json::to_string(&working).unwrap();
        assert_eq!(serde_json::from_str::<PopupSpan>(&wire).unwrap(), working);
    }

    #[test]
    fn sidebar_item_uses_last_column_without_a_right_gutter() {
        let mut state = snapshot();
        state.panes[0].harness = None;
        state.panes[0].ci = None;
        let collapsed = std::collections::BTreeSet::from(["workspace".to_owned()]);
        let rows = tree(&state, &collapsed);
        let rendered = visible(&render_row(
            &rows[0],
            &state,
            &Theme::default(),
            32,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        ));
        assert_eq!(width(&rendered), 32);
        assert!(rendered.starts_with(" ● "));
        assert!(rendered.ends_with('·'));
    }

    #[test]
    fn project_colors_follow_identity_not_name_branch_or_lifecycle() {
        let theme = Theme::default();
        let project = identity("/repo/.git");
        assert_ne!(project_color(Some(&project), &theme), theme.muted);
        let mut renamed = project.clone();
        renamed.name = "a new name".to_owned();
        assert_eq!(
            project_color(Some(&project), &theme),
            project_color(Some(&renamed), &theme)
        );
        renamed.repository = false;
        assert_eq!(project_color(Some(&renamed), &theme), theme.muted);
        assert_eq!(project_color(None, &theme), theme.muted);

        let mut state = snapshot();
        let rows = tree(&state, &Default::default());
        let pane = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Pane { .. }))
            .unwrap();
        let projects = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Projects { .. }))
            .unwrap();
        let workspace = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Workspace { .. }))
            .unwrap();
        let color = project_color(state.panes[0].project.as_ref(), &theme);
        assert!(
            render_row(projects, &state, &Theme::default(), 40, RowStyle::default())
                .contains(color)
        );
        for status in [
            Status::Idle,
            Status::Working,
            Status::Blocked,
            Status::Done,
            Status::Unknown,
        ] {
            state.panes[0].status = status;
            state.workspaces[0].status = status;
            state.panes[0].branch = Some("changed".to_owned());
            let rendered = render_row(
                pane,
                &state,
                &Theme::default(),
                40,
                RowStyle {
                    selected: true,
                    hovered: false,
                    selected_group: true,
                    clock_frame: 0,
                },
            );
            assert!(rendered.contains(&format!("{color}● ")));
            assert!(!visible(&rendered).contains("○"));
            let parent = render_row(
                workspace,
                &state,
                &Theme::default(),
                40,
                RowStyle {
                    selected: false,
                    hovered: true,
                    selected_group: false,
                    clock_frame: 0,
                },
            );
            assert!(parent.contains(&format!("{TEXT}● ")));
        }
    }

    #[test]
    fn assigned_slots_match_sidebar_finder_and_passive_popups_across_workspaces() {
        let mut state = snapshot();
        let workspace = state.workspaces[0].clone();
        let pane = state.panes[0].clone();
        state.workspaces.clear();
        state.panes.clear();
        for group in 0..2 {
            let mut workspace = workspace.clone();
            workspace.id = format!("workspace-{group}");
            workspace.projects.clear();
            for slot in group * 6..(group + 1) * 6 {
                let mut project = identity(&format!("/repo-{slot}/.git"));
                project.name = format!("repo-{slot}");
                project.color_slot = Some(slot);
                workspace.projects.push(project.clone());
                let mut pane = pane.clone();
                pane.id = format!("pane-{slot}");
                pane.workspace_id = workspace.id.clone();
                pane.project = Some(project);
                pane.ci = None;
                state.panes.push(pane);
            }
            state.workspaces.push(workspace);
        }
        let mut theme = Theme::default();
        theme.prepare_snapshot(&state);
        let rows = tree(&state, &Default::default());
        let mut index = crate::pane_search::Index::default();
        index.rebuild(&state);
        assert_eq!(index.search("").len(), 12);
        for pane in &state.panes {
            let project = pane.project.as_ref().unwrap();
            let color = project_color(Some(project), &theme);
            assert_ne!(color, theme.muted);
            let row = rows
                .iter()
                .find(|row| {
                    row.item.as_ref() == Some(&Item::Pane(pane.id.clone()))
                        && matches!(row.kind, RowKind::Pane { .. })
                })
                .unwrap();
            let rendered = render_row(row, &state, &theme, 80, RowStyle::default());
            assert!(rendered.contains(&format!("{color}● ")));
            for item in [
                Item::Pane(pane.id.clone()),
                Item::Workspace(pane.workspace_id.clone()),
            ] {
                let popup = tooltip(&item, &state);
                let popup_rows = popup.rows();
                let spans: Vec<_> = popup_rows.iter().flat_map(|row| &row.spans)
                    .filter(|span| matches!(&span.color, PopupColor::Project(identity) if identity.id == project.id))
                    .collect();
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].color.ansi(&theme), color);
            }
            let labels = rows
                .iter()
                .find(|row| {
                    row.workspace_id.as_ref() == Some(&pane.workspace_id)
                        && matches!(row.kind, RowKind::Projects { .. })
                })
                .unwrap();
            assert!(
                render_row(labels, &state, &theme, 200, RowStyle::default())
                    .contains(&format!("{color}{}", project.name))
            );
        }
        for result in 0..12 {
            let pane = &state.panes[index.results()[result].pane];
            let color = project_color(pane.project.as_ref(), &theme);
            assert!(
                crate::finder_entry::render(&mut index, &state, result, false, 80, &theme)
                    .contains(&format!("{color}● "))
            );
        }
        let folded = tree(
            &state,
            &std::collections::BTreeSet::from(["workspace-1".into()]),
        );
        assert_eq!(
            folded
                .iter()
                .filter(|row| matches!(row.kind, RowKind::Pane { .. }))
                .count(),
            6
        );
        let unique: std::collections::BTreeSet<_> = state
            .panes
            .iter()
            .map(|pane| project_color(pane.project.as_ref(), &theme))
            .collect();
        assert_eq!(unique.len(), 12);
    }

    #[test]
    fn ci_badges_reserve_unicode_cells_and_do_not_change_agent_state() {
        let mut state = snapshot();
        let rows = tree(&state, &Default::default());
        for stale in [false, true] {
            state.panes[0].ci.as_mut().unwrap().stale = stale;
            for row in &rows {
                for columns in 0..=80 {
                    let rendered = render_row(
                        row,
                        &state,
                        &Theme::default(),
                        columns,
                        RowStyle {
                            selected: true,
                            hovered: false,
                            selected_group: true,
                            clock_frame: 0,
                        },
                    );
                    assert_eq!(
                        width(&visible(&rendered)),
                        columns,
                        "{columns}: {rendered:?}"
                    );
                }
            }
            let detail = rows
                .iter()
                .find(|row| matches!(row.kind, RowKind::Detail { .. }))
                .unwrap();
            let rendered = visible(&render_row(
                detail,
                &state,
                &Theme::default(),
                40,
                RowStyle {
                    selected: false,
                    hovered: false,
                    selected_group: false,
                    clock_frame: 0,
                },
            ));
            assert!(
                rendered.ends_with(' '),
                "Reserve the last cell for wide CI artwork"
            );
            assert_eq!(rendered.matches(WORKFLOW).count(), 1);
            for status in [
                CiStatus::Passed,
                CiStatus::Failed,
                CiStatus::Running,
                CiStatus::Unknown,
            ] {
                assert!(!rendered.contains(ci_glyph(status)));
            }
            assert_eq!(state.panes[0].status, Status::Working);
            assert_eq!(state.panes[0].attention, None);
        }
        state.panes[0].ci.as_mut().unwrap().truncated = true;
        assert_eq!(
            ci_state(state.panes[0].ci.as_ref().unwrap()),
            CiStatus::Unknown
        );
    }

    #[test]
    fn refresh_headers_are_exact_deduplicated_and_disabled_during_loading() {
        let mut state = snapshot();
        let mut other = state.panes[0].clone();
        other.id = "2".to_owned();
        state.panes.push(other);
        let target = CiRefreshTarget {
            repository: state.panes[0].ci.as_ref().unwrap().repository.clone(),
            head_sha: state.panes[0].ci.as_ref().unwrap().head_sha.clone(),
        };
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let headers: Vec<_> = tooltip(&item, &state)
                .rows()
                .into_iter()
                .filter(|row| row.refresh.is_some())
                .collect();
            assert_eq!(headers.len(), 1);
            assert_eq!(headers[0].refresh, Some(target.clone()));
            assert_eq!(headers[0].right[0].text, refresh_button_text());
            assert!(headers[0].link.is_none());
        }
        for pane in &mut state.panes {
            pane.ci.as_mut().unwrap().fetch_state = CiFetchState::Loading;
        }
        let loading = tooltip(&Item::Workspace("workspace".to_owned()), &state);
        assert!(loading.rows().iter().all(|row| row.refresh.is_none()));
        let header = loading
            .rows()
            .into_iter()
            .find(|row| !row.right.is_empty())
            .unwrap();
        assert_eq!(header.right[0].text, refresh_button_text());
        assert_eq!(header.right[0].color, PopupColor::Muted);
        state.panes[0].ci.as_mut().unwrap().head_sha.clear();
        assert!(
            tooltip(&Item::Pane("1".to_owned()), &state)
                .rows()
                .iter()
                .all(|row| row
                    .right
                    .iter()
                    .all(|span| span.text != refresh_button_text()))
        );
        state.panes[0].ci = None;
        assert!(
            tooltip(&Item::Pane("1".to_owned()), &state)
                .rows()
                .iter()
                .all(|row| row.refresh.is_none())
        );
    }

    #[test]
    fn remote_checks_identify_checked_ref_and_distinct_local_head() {
        let mut state = snapshot();
        let local_branch = state.panes[0].branch.clone();
        let local_changes = state.panes[0].branch_changes.clone();
        let ci = state.panes[0].ci.as_mut().unwrap();
        ci.head_sha = "abcdef0123456789".to_owned();
        ci.local_head_sha = "76543210fedcba98".to_owned();
        ci.checked_ref = Some("origin/feature/日本語".to_owned());
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let rows = tooltip(&item, &state).rows();
            assert!(
                rows.iter()
                    .any(|row| { row.plain() == "Checks for origin/feature/日本語 @ abcdef0" })
            );
            assert!(rows.iter().any(|row| {
                row.plain() == "Local HEAD @ 7654321 · differs from checked commit"
            }));
            let target = rows.iter().find_map(|row| row.refresh.as_ref()).unwrap();
            assert_eq!(target.head_sha, "abcdef0123456789");
            assert_ne!(
                target.head_sha,
                state.panes[0].ci.as_ref().unwrap().local_head_sha
            );
        }
        assert_eq!(state.panes[0].branch, local_branch);
        assert_eq!(state.panes[0].branch_changes, local_changes);
        state.panes[0].ci.as_mut().unwrap().local_head_sha = "abcdef0123456789".to_owned();
        assert!(
            tooltip(&Item::Pane("1".to_owned()), &state)
                .rows()
                .iter()
                .all(|row| !row.plain().contains("differs from checked commit"))
        );
    }

    #[test]
    fn workspace_ci_deduplicates_shared_commit_and_summarizes_sources_without_jobs() {
        let mut state = snapshot();
        let mut duplicate = state.panes[0].clone();
        duplicate.id = "2".to_owned();
        duplicate.cwd = Some("/repo-linked".to_owned());
        state.panes.push(duplicate);
        assert_eq!(workspace_ci(&state, "workspace").len(), 1);
        let popup = tooltip(&Item::Workspace("workspace".to_owned()), &state);
        let rows = popup.rows();
        let ci = state.panes[0].ci.as_ref().unwrap();
        let request = ci.change_request.as_ref().unwrap();
        let identities: Vec<_> = rows
            .iter()
            .filter(|row| row.plain().contains(&format!("#{}", request.number)))
            .collect();
        assert_eq!(identities.len(), 1);
        assert!(identities[0].plain().contains(&ci.repository.path));
        assert_eq!(identities[0].link.as_deref(), Some(request.url.as_str()));
        assert_eq!(
            rows.iter()
                .filter(|row| row.plain().contains(&request.title))
                .count(),
            1
        );
        let summaries: Vec<_> = rows.iter().filter_map(|row| row.refresh.as_ref()).collect();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].repository, ci.repository);
        assert_eq!(summaries[0].head_sha, ci.head_sha);
        assert_eq!(popup.sections.len(), 1);
        for check in &ci.checks {
            assert!(rows.iter().all(|row| !row.plain().contains(&check.name)));
            if let Some(url) = &check.url {
                assert!(rows.iter().all(|row| row.link.as_ref() != Some(url)));
            }
        }
        state.panes[1].ci.as_mut().unwrap().stale = true;
        assert_eq!(
            ci_state(workspace_ci(&state, "workspace")[0].1),
            CiStatus::Unknown
        );
        state.panes[1].ci.as_mut().unwrap().head_sha = "different-current-commit".to_owned();
        assert_eq!(workspace_ci(&state, "workspace").len(), 2);
        state.panes[1].ci.as_mut().unwrap().fetch_state = CiFetchState::Unavailable;
        state.panes[1].ci.as_mut().unwrap().error = Some("provider unavailable 日本語".repeat(30));
        let workspace_rows = tooltip(&Item::Workspace("workspace".to_owned()), &state).rows();
        let summaries: Vec<_> = workspace_rows
            .iter()
            .filter(|row| row.refresh.is_some())
            .collect();
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().all(|row| row.truncate));
        assert!(summaries.iter().any(|row| {
            row.spans
                .iter()
                .any(|span| span.color == PopupColor::Ci(CiStatus::Unknown))
        }));
        for pane in &state.panes {
            for check in &pane.ci.as_ref().unwrap().checks {
                assert!(
                    workspace_rows
                        .iter()
                        .all(|row| !row.plain().contains(&check.name))
                );
                if let Some(url) = &check.url {
                    assert!(
                        workspace_rows
                            .iter()
                            .all(|row| row.link.as_ref() != Some(url))
                    );
                }
            }
        }

        let popup = tooltip(&Item::Pane("1".to_owned()), &state);
        let rows = popup.rows();
        assert!(popup.intro.iter().any(|row| {
            row.spans
                .iter()
                .any(|span| span.text == state.panes[0].description)
        }));
        let check = rows
            .iter()
            .find(|row| row.plain().contains("unit tests"))
            .unwrap();
        assert!(check.truncate);
        assert_eq!(
            check.link.as_deref(),
            Some("https://github.com/owner/repo/actions/runs/12")
        );
        assert!(
            check
                .spans
                .iter()
                .any(|span| span.color == PopupColor::Ci(CiStatus::Passed))
        );
        assert!(rows.iter().any(|row| matches!(
            row.spans.first().map(|span| &span.color),
            Some(PopupColor::Project(_))
        )));
    }

    #[test]
    fn popup_warnings_never_present_cached_success_as_current_success() {
        let mut state = snapshot();
        let ci = state.panes[0].ci.as_mut().unwrap();
        ci.stale = true;
        ci.truncated = true;
        ci.error = Some(
            "GitHub rate limit; retry later https://api.github.com/long/provider/path".to_owned(),
        );
        let content = tooltip(&Item::Pane("1".to_owned()), &state);
        let rows = content.rows();
        let summary = rows
            .iter()
            .find(|row| {
                row.spans.first().is_some_and(|span| {
                    span.color == PopupColor::Ci(CiStatus::Unknown) && span.bold
                })
            })
            .unwrap();
        assert_eq!(summary.spans[0].color, PopupColor::Ci(CiStatus::Unknown));
        assert!(
            rows.iter()
                .any(|row| row.plain().contains("Cached results"))
        );
        assert!(
            rows.iter()
                .any(|row| row.plain().contains("pagination limit"))
        );
        assert!(
            rows.iter()
                .any(|row| row.plain().contains("GitHub rate limit; retry later"))
        );
        assert!(!rows.iter().any(|row| row.plain().contains("https://")));
    }

    #[test]
    fn managed_agent_id_appears_only_on_its_harness_pane_popup() {
        let mut state = snapshot();
        let id = "0123456789abcdef0123456789abcdef";
        state.panes[0].agent_id = Some(id.to_owned());
        let pane = Item::Pane("1".to_owned());
        let popup = tooltip(&pane, &state);
        let identity = popup
            .intro
            .iter()
            .find(|row| row.plain().contains("Agent ID"))
            .unwrap();
        assert_eq!(identity.plain(), format!("\u{f0efe} Agent ID: {id}"));
        assert_eq!(identity.spans[1].color, PopupColor::Text);
        assert!(
            !tooltip(&Item::Workspace("workspace".to_owned()), &state)
                .rows()
                .iter()
                .any(|row| row.plain().contains(id))
        );
        state.panes[0].harness = None;
        assert!(
            !tooltip(&pane, &state)
                .rows()
                .iter()
                .any(|row| row.plain().contains(id))
        );
    }

    #[test]
    fn popup_missing_metadata_and_shell_remain_coherent() {
        let mut state = snapshot();
        let pane = &mut state.panes[0];
        pane.harness = None;
        pane.project = None;
        pane.branch = None;
        pane.ci = None;
        pane.cwd = None;
        pane.description = "fish shell".to_owned();
        let content = tooltip(&Item::Pane("1".to_owned()), &state);
        assert_eq!(content.title.plain(), state.panes[0].name);
        assert_eq!(content.sections.len(), 1);
        assert!(content.intro.iter().any(|row| {
            row.spans
                .iter()
                .any(|span| span.text == state.panes[0].description)
        }));
        state.panes[0].description.clear();
        let content = tooltip(&Item::Pane("1".to_owned()), &state);
        assert!(content.intro.is_empty());
        assert!(content.rows().iter().all(|row| row.link.is_none()));
    }

    fn changes() -> bkpr::BranchChanges {
        bkpr::BranchChanges {
            base_ref: "refs/remotes/origin/main".to_owned(),
            merge_base: "abcdef0123456789abcdef0123456789abcdef01".to_owned(),
            total: bkpr::DiffStat {
                added: 1234,
                deleted: 567,
                binary_files: 2,
            },
        }
    }

    #[test]
    fn count_width_boundaries_include_zero_and_maximum_values() {
        let mut changes = changes();
        for number in [0, u64::MAX] {
            changes.total.added = number;
            changes.total.deleted = number;
            let expected = format!("+{number} -{number}");
            let columns = width(&expected);
            assert!(branch_counts(Some(&changes), columns - 1, &Theme::default()).is_none());
            let (text, reserved) =
                branch_counts(Some(&changes), columns, &Theme::default()).unwrap();
            assert_eq!(visible(&text), expected);
            assert_eq!(reserved, columns);
        }
        assert!(branch_counts(None, 80, &Theme::default()).is_none());
    }

    #[test]
    fn workspace_gaps_stay_blank_and_unselectable_when_groups_fold() {
        let mut state = snapshot();
        for id in ["empty", "third"] {
            let mut workspace = state.workspaces[0].clone();
            workspace.id = id.to_owned();
            state.workspaces.push(workspace);
        }
        for collapsed in [
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::from(["workspace".to_owned()]),
            state
                .workspaces
                .iter()
                .map(|workspace| workspace.id.clone())
                .collect(),
        ] {
            let rows = tree(&state, &collapsed);
            assert!(!matches!(rows.first().unwrap().kind, RowKind::Spacer));
            assert!(!matches!(rows.last().unwrap().kind, RowKind::Spacer));
            for workspace in state.workspaces.iter().skip(1) {
                let item = Item::Workspace(workspace.id.clone());
                let header = *item_band(&rows, &item).unwrap().start();
                let gap = header - 1;
                assert!(matches!(rows[gap].kind, RowKind::Spacer));
                assert!(rows[gap].workspace_id.is_none());
                assert_eq!(item_at(&rows, gap), None);
                assert_eq!(item_at(&rows, header), Some(item));
                assert!(
                    render_row(
                        &rows[gap],
                        &state,
                        &Theme::default(),
                        38,
                        RowStyle::default(),
                    )
                    .is_empty()
                );
            }
        }
    }

    #[test]
    fn compact_tree_hit_testing_keeps_neighbor_ownership_under_outlines() {
        let mut state = snapshot();
        let mut second = state.panes[0].clone();
        second.id = "2".to_owned();
        state.panes.push(second);
        let rows = tree(&state, &Default::default());
        let workspace = Item::Workspace("workspace".to_owned());
        let pane = Item::Pane("1".to_owned());
        let second = Item::Pane("2".to_owned());
        assert_eq!(item_band(&rows, &workspace), Some(0..=1));
        assert_eq!(item_band(&rows, &pane), Some(2..=3));
        assert_eq!(item_band(&rows, &second), Some(4..=5));
        assert_eq!(
            items(&rows),
            vec![workspace.clone(), pane.clone(), second.clone()]
        );
        for item in [&workspace, &pane, &second] {
            for index in item_band(&rows, item).unwrap() {
                assert_eq!(item_at(&rows, index), Some(item.clone()));
            }
        }
        assert_eq!(rows.len(), 6);
        assert_eq!(item_at(&rows, 1), Some(workspace.clone()));
        assert_eq!(item_at(&rows, 4), Some(second));
        assert_eq!(item_at(&rows, 3), Some(pane));
        assert_eq!(item_at(&rows, rows.len()), None);
        let folded = tree(
            &state,
            &std::collections::BTreeSet::from(["workspace".to_owned()]),
        );
        assert_eq!(folded.len(), 1);
        assert_eq!(item_band(&folded, &workspace), Some(0..=0));
    }

    #[test]
    fn cwd_detail_rows_and_density_do_not_depend_on_program_or_native_status() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let rows = tree(&state, &Default::default());
        let detail = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Detail { .. }))
            .unwrap();
        let expected = render_row(
            detail,
            &state,
            &Theme::default(),
            40,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        for harness in [None, Some("codex"), Some("claude"), Some("omp")] {
            state.panes[0].harness = harness.map(str::to_owned);
            for status in [
                Status::Idle,
                Status::Working,
                Status::Blocked,
                Status::Unknown,
            ] {
                state.panes[0].status = status;
                assert_eq!(tree(&state, &Default::default()).len(), rows.len());
                assert_eq!(
                    render_row(detail, &state, &Theme::default(), 40, RowStyle::default()),
                    expected
                );
            }
        }
        state.panes[0].branch = None;
        let fallback = render_row(
            detail,
            &state,
            &Theme::default(),
            40,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(visible(&fallback).contains("Native activity"));
        assert!(!fallback.contains(WORKFLOW));
        state.panes[0].harness = None;
        assert_eq!(
            render_row(detail, &state, &Theme::default(), 40, RowStyle::default()),
            fallback
        );
        assert_eq!(tree(&state, &Default::default()).len(), rows.len());
    }

    #[test]
    fn branch_numbers_are_complete_or_omitted_without_displacing_status() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let rows = tree(&state, &Default::default());
        let detail = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Detail { .. }))
            .unwrap();
        let pane = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Pane { .. }))
            .unwrap();
        let status_before = render_row(
            pane,
            &state,
            &Theme::default(),
            16,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        for columns in 0..=80 {
            let rendered = render_row(
                detail,
                &state,
                &Theme::default(),
                columns,
                RowStyle {
                    selected: false,
                    hovered: false,
                    selected_group: false,
                    clock_frame: 0,
                },
            );
            let text = visible(&rendered);
            assert_eq!(width(&text), columns);
            let counts: Vec<_> = text
                .split_whitespace()
                .filter(|word| word.starts_with('+') || word.starts_with('-'))
                .collect();
            assert!(counts.is_empty() || counts == ["+1234", "-567"]);
            assert!(!text.contains("CI "));
        }
        let wide = render_row(
            detail,
            &state,
            &Theme::default(),
            38,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(visible(&wide).contains("+1234 -567"));
        let narrow = render_row(
            detail,
            &state,
            &Theme::default(),
            16,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(!visible(&narrow).contains(['+', '-']));
        state.panes[0].branch = Some("topic".to_owned());
        let adjacent = render_row(
            detail,
            &state,
            &Theme::default(),
            80,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(visible(&adjacent).contains("topic +1234 -567"));
        state.panes[0].branch_changes = None;
        assert_eq!(
            status_before,
            render_row(pane, &state, &Theme::default(), 16, RowStyle::default())
        );
        assert!(
            !visible(&render_row(
                detail,
                &state,
                &Theme::default(),
                80,
                RowStyle::default()
            ))
            .contains(['+', '-'])
        );
        assert_eq!(state.panes[0].status, Status::Working);
        assert_eq!(state.panes[0].attention, None);
    }

    #[test]
    fn branch_popups_deduplicate_context_and_do_not_invent_zero_counts() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let mut duplicate = state.panes[0].clone();
        duplicate.id = "2".to_owned();
        state.panes.push(duplicate);
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let popup = tooltip(&item, &state);
            let rows = popup.rows();
            let text = rows
                .iter()
                .map(PopupRow::plain)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches("+1234").count(), 1);
            assert!(text.contains("-567"));
            assert!(text.contains("Binary files: 2 (no lines)"));
            assert!(!text.contains("Implementation") && !text.contains("tests/docs"));
            assert!(
                rows.iter()
                    .flat_map(|row| row.spans.iter().chain(&row.suffix))
                    .any(|span| span.color == PopupColor::Added)
            );
            assert!(
                rows.iter()
                    .flat_map(|row| row.spans.iter().chain(&row.suffix))
                    .any(|span| span.color == PopupColor::Deleted)
            );
        }
        for pane in &mut state.panes {
            pane.branch_changes = None;
            pane.ci = None;
        }
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let text = tooltip(&item, &state)
                .rows()
                .iter()
                .map(PopupRow::plain)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("unavailable"));
            assert!(!text.contains("+0") && !text.contains("-0"));
        }
    }

    #[test]
    fn popup_links_preserve_only_safe_https_metadata() {
        for url in [
            "http://github.com/repo",
            "https://user@github.com/repo",
            "https://github.com/repo\u{1b}]8;;",
            "https://github.com/repo\nnext",
            "https://github.com/repo\u{202e}",
        ] {
            assert!(safe_link(url).is_none(), "{url:?}");
        }
        assert_eq!(
            safe_link("https://github.com/owner/repo/actions/runs/12?check=1"),
            Some("https://github.com/owner/repo/actions/runs/12?check=1")
        );
    }

    #[test]
    fn duration_metadata_distinguishes_missing_zero_and_complete_elapsed_time() {
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
    }

    #[test]
    fn request_metadata_preserves_verified_links_and_native_status() {
        let mut state = snapshot();
        for request_state in [
            ChangeRequestState::Draft,
            ChangeRequestState::Open,
            ChangeRequestState::Merged,
            ChangeRequestState::Closed,
        ] {
            let ci = state.panes[0].ci.as_mut().unwrap();
            ci.change_request.as_mut().unwrap().state = request_state;
            ci.change_request.as_mut().unwrap().title =
                "日本語 title \u{1b}[31m untrusted".to_owned();
            let context = forge_context(ci, Some(&identity("/repo/.git")), Vec::new());
            assert_eq!(
                context[0].link.as_deref(),
                Some(ci.change_request.as_ref().unwrap().url.as_str())
            );
            assert_eq!(
                context[1].link.as_deref(),
                Some(ci.change_request.as_ref().unwrap().url.as_str())
            );
            assert!(!context[1].plain().contains('\u{1b}'));
            assert_eq!(state.panes[0].status, Status::Working);
        }
    }

    #[test]
    fn fetch_loading_and_unavailable_are_explicit_not_check_results() {
        let mut snapshot = ci(CiStatus::Passed);
        for (fetch_state, label) in [
            (CiFetchState::Loading, "Loading"),
            (CiFetchState::Unavailable, "Unavailable"),
        ] {
            snapshot.fetch_state = fetch_state;
            snapshot.updated_at = 0;
            snapshot.checks.clear();
            assert_eq!(ci_state(&snapshot), CiStatus::Unknown);
            assert!(ci_rows(&snapshot)[0].plain().contains(label));
        }
    }

    #[test]
    fn popup_repository_identity_is_colored_once_with_only_real_remote_links() {
        let mut state = snapshot();
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let popup = tooltip(&item, &state);
            let rows = popup.rows();
            let names: Vec<_> = rows
                .iter()
                .filter(|row| {
                    row.spans
                        .iter()
                        .any(|span| matches!(span.color, PopupColor::Project(_)))
                })
                .collect();
            assert_eq!(names.len(), 1);
            assert_eq!(names[0].plain(), "\u{f09b} owner/repo #7");
            assert_eq!(
                names[0].link.as_deref(),
                Some("https://github.com/owner/repo/pull/7")
            );
            assert_eq!(
                names[0].spans[0].color.ansi(&Theme::default()),
                project_color(state.panes[0].project.as_ref(), &Theme::default())
            );
        }
        state.panes[0].ci = None;
        for item in [
            Item::Pane("1".to_owned()),
            Item::Workspace("workspace".to_owned()),
        ] {
            let popup = tooltip(&item, &state);
            let rows = popup.rows();
            let name = rows
                .iter()
                .find(|row| {
                    row.spans
                        .iter()
                        .any(|span| matches!(span.color, PopupColor::Project(_)))
                })
                .unwrap();
            assert_eq!(name.plain(), state.panes[0].project.as_ref().unwrap().name);
            assert!(name.link.is_none());
            assert!(rows.iter().all(|row| !row.plain().contains('\u{f09b}')));
        }
    }

    #[test]
    fn repository_identity_prefers_the_verified_request_without_inventing_links() {
        let mut snapshot = ci(CiStatus::Passed);
        snapshot.change_request.as_mut().unwrap().number = 42;
        snapshot.change_request.as_mut().unwrap().url =
            "https://github.com/owner/repo/pull/42".into();
        let heading = forge_repository(&snapshot, None);
        assert_eq!(
            heading.link.as_deref(),
            Some("https://github.com/owner/repo/pull/42")
        );
        assert!(heading.plain().ends_with("owner/repo #42"));
        assert!(
            !request_row(snapshot.change_request.as_ref().unwrap())
                .plain()
                .contains("#42")
        );

        snapshot.change_request.as_mut().unwrap().url = String::new();
        assert!(forge_repository(&snapshot, None).link.is_none());
        snapshot.change_request.as_mut().unwrap().url = "javascript:alert(1)".into();
        assert!(forge_repository(&snapshot, None).link.is_none());
        snapshot.change_request = None;
        let heading = forge_repository(&snapshot, None);
        assert_eq!(
            heading.link.as_deref(),
            Some("https://github.com/owner/repo")
        );
        assert!(!heading.plain().contains('#'));
        snapshot.repository.web_url = String::new();
        assert!(forge_repository(&snapshot, None).link.is_none());
    }

    #[test]
    fn linked_tickets_follow_repository_identity_before_other_pane_context() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let request = state.panes[0]
            .ci
            .as_mut()
            .unwrap()
            .change_request
            .as_mut()
            .unwrap();
        request.issues = vec![
            LinkedIssue {
                provider: IssueTracker::Linear,
                identifier: "ENG-42".into(),
                title: Some("Repair rendering".into()),
                url: "https://linear.app/real-team/issue/ENG-42/repair-rendering".into(),
            },
            LinkedIssue {
                provider: IssueTracker::Linear,
                identifier: "OPS-9".into(),
                title: None,
                url: "https://linear.app/other-team/issue/OPS-9".into(),
            },
        ];
        for item in [Item::Pane("1".into()), Item::Workspace("workspace".into())] {
            let rows = tooltip(&item, &state).rows();
            let heading = rows
                .iter()
                .position(|row| row.plain().contains("owner/repo #7"))
                .unwrap();
            assert_eq!(
                rows[heading].link.as_deref(),
                Some("https://github.com/owner/repo/pull/7")
            );
            assert!(
                rows[heading + 1]
                    .plain()
                    .contains("Improve project metadata")
            );
            assert_eq!(
                rows[heading + 1].link.as_deref(),
                Some("https://github.com/owner/repo/pull/7")
            );
            assert_eq!(
                rows[heading + 2].link.as_deref(),
                Some("https://linear.app/real-team/issue/ENG-42/repair-rendering")
            );
            assert!(
                rows[heading + 2]
                    .plain()
                    .contains("ENG-42 · Repair rendering")
            );
            assert_eq!(
                rows[heading + 3].link.as_deref(),
                Some("https://linear.app/other-team/issue/OPS-9")
            );
            assert!(rows[heading + 3].plain().ends_with("OPS-9"));
            assert_eq!(
                rows.iter().filter(|row| row.plain().contains("#7")).count(),
                1
            );
            assert!(
                rows.iter()
                    .any(|row| !row.suffix.is_empty() && row.link.is_none())
            );
        }
    }

    #[test]
    fn workspace_multiple_requests_keep_each_identity_link_and_number_once() {
        let mut state = snapshot();
        let mut other = state.panes[0].clone();
        other.id = "2".into();
        let ci = other.ci.as_mut().unwrap();
        ci.head_sha = "another-head".into();
        ci.branch = Some("another-branch".into());
        let request = ci.change_request.as_mut().unwrap();
        request.number = 8;
        request.url = "https://github.com/owner/repo/pull/8".into();
        state.panes.push(other);
        let rows = tooltip(&Item::Workspace("workspace".into()), &state).rows();
        for number in [7, 8] {
            let matching: Vec<_> = rows
                .iter()
                .filter(|row| row.plain().contains(&format!("#{number}")))
                .collect();
            assert_eq!(matching.len(), 1);
            assert_eq!(
                matching[0].link,
                Some(format!("https://github.com/owner/repo/pull/{number}"))
            );
        }
    }

    #[test]
    fn pane_header_orders_status_path_and_activity_before_provider_checks() {
        let mut state = snapshot();
        state.panes[0].cwd = Some("/日本語/local/path".to_owned());
        state.panes[0].description = "Investigating a failing check".to_owned();
        let popup = tooltip(&Item::Pane("1".to_owned()), &state);
        let rows = popup.rows();
        assert_eq!(rows[1], popup.subtitle);
        assert_eq!(rows[2].plain(), "\u{f07b} /日本語/local/path");
        assert_eq!(rows[3].spans[1].text, state.panes[0].description);
        assert!(rows[3].spans[0].color == PopupColor::Muted);
        assert!(
            rows.iter()
                .position(|row| row.plain().contains("unit tests"))
                .unwrap()
                > 3
        );
        state.panes[0].ci = None;
        state.panes[0].branch = None;
        state.panes[0].project.as_mut().unwrap().repository = false;
        let popup = tooltip(&Item::Pane("1".to_owned()), &state);
        assert_eq!(popup.intro.len(), 2);
        assert!(
            popup
                .rows()
                .iter()
                .all(|row| row.right.is_empty() && row.suffix.is_empty())
        );
        assert!(popup.rows().iter().all(|row| row.link.is_none()));
    }

    #[test]
    fn provider_target_labels_remain_visible_when_the_commit_stats_are_unavailable() {
        let mut state = snapshot();
        let ci = state.panes[0].ci.as_mut().unwrap();
        ci.change_request.as_mut().unwrap().base = Some(bkpr::ci_contract::ChangeRequestBase {
            branch: "stack/parent".into(),
            commit: "f".repeat(40),
            repository: ci.repository.clone(),
        });
        state.panes[0].branch_changes = None;
        for item in [Item::Pane("1".into()), Item::Workspace("workspace".into())] {
            let rows = tooltip(&item, &state).rows();
            let branch = rows
                .iter()
                .find(|row| !row.right.is_empty() && row.right[0].text.contains("stack/parent"))
                .unwrap();
            assert_eq!(branch.right[0].text, "\u{f419} stack/parent");
            assert!(branch.suffix.is_empty());
            assert!(
                rows.iter()
                    .any(|row| row.plain() == "Branch changes unavailable")
            );
            assert!(!rows.iter().any(|row| row.plain().contains("origin/main")));
        }
    }

    #[test]
    fn branch_repository_row_combines_normal_totals_and_explicit_unlinked_base() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let popup = tooltip(&Item::Pane("1".to_owned()), &state);
        let row = popup
            .rows()
            .into_iter()
            .find(|row| !row.suffix.is_empty())
            .unwrap();
        assert!(row.truncate);
        assert_eq!(
            row.spans[0].text,
            format!("\u{f062c} {}", state.panes[0].branch.as_ref().unwrap())
        );
        assert_eq!(
            row.suffix
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            " +1234 -567"
        );
        assert_eq!(row.right[0].text, "\u{f419} origin/main");
        assert_eq!(row.right[0].color, PopupColor::Muted);
        assert!(row.link.is_none());
    }

    #[test]
    fn workspace_repository_groups_keep_distinct_projects_and_deduplicate_sources() {
        let mut state = snapshot();
        let mut duplicate = state.panes[0].clone();
        duplicate.id = "2".to_owned();
        state.panes.push(duplicate);
        let project = ProjectIdentity {
            id: "/other/.git".to_owned(),
            name: "other".to_owned(),
            repository: true,
            color_slot: Some(1),
        };
        state.workspaces[0].projects.push(project.clone());
        let mut other = state.panes[0].clone();
        other.id = "3".to_owned();
        other.project = Some(project.clone());
        other.ci.as_mut().unwrap().repository.path = "someone/other".to_owned();
        other.ci.as_mut().unwrap().repository.web_url =
            "https://github.com/someone/other".to_owned();
        other.ci.as_mut().unwrap().change_request = None;
        state.panes.push(other);
        let popup = tooltip(&Item::Workspace("workspace".to_owned()), &state);
        assert_eq!(popup.sections.len(), 2);
        assert!(
            popup
                .sections
                .iter()
                .any(|section| section.heading.as_ref().unwrap().link.as_deref()
                    == Some("https://github.com/someone/other"))
        );
        let rows = popup.rows();
        assert_eq!(
            rows.iter()
                .filter(
                    |row| row.link.as_deref() == Some("https://github.com/owner/repo/pull/7")
                        && row
                            .spans
                            .iter()
                            .any(|span| matches!(span.color, PopupColor::Project(_)))
                )
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.plain().contains("\u{f062c}"))
                .count(),
            2
        );
        assert!(rows.iter().any(|row| {
            row.spans
                .iter()
                .any(|span| span.color == PopupColor::Project(project.clone()))
        }));
    }

    #[test]
    fn ordinary_programs_have_no_status_artwork_or_subtitle_and_keep_context() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        state.panes[0].name = "abcdefghijklmno".to_owned();
        let rows = tree(&state, &Default::default());
        let pane_row = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Pane { .. }))
            .unwrap();
        let detail_row = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Detail { .. }))
            .unwrap();
        let harness_popup = tooltip(&Item::Pane("1".to_owned()), &state);
        let detail = render_row(
            detail_row,
            &state,
            &Theme::default(),
            80,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(visible(&detail).contains("+1234 -567"));
        assert!(visible(&detail).contains(WORKFLOW));
        for harness in [None, Some("cargo"), Some("shell"), Some("unknown-app")] {
            state.panes[0].harness = harness.map(str::to_owned);
            let mut baseline = None;
            for (status, attention) in [
                (Status::Idle, None),
                (Status::Working, None),
                (Status::Done, None),
                (Status::Unknown, None),
                (Status::Blocked, Some(Attention::Permission)),
                (Status::Blocked, Some(Attention::Question)),
                (Status::Blocked, Some(Attention::Auth)),
                (Status::Blocked, Some(Attention::Error)),
            ] {
                state.panes[0].status = status;
                state.panes[0].attention = attention;
                let rendered = render_row(
                    pane_row,
                    &state,
                    &Theme::default(),
                    22,
                    RowStyle {
                        selected: false,
                        hovered: false,
                        selected_group: false,
                        clock_frame: 0,
                    },
                );
                let text = visible(&rendered);
                assert_eq!(width(&text), 22);
                assert!(text.contains(&state.panes[0].name));
                assert!(!text.contains(status_icon(status, attention).trim_end()));
                assert!(!text.contains(harness_icon("unknown-app")));
                if let Some(baseline) = &baseline {
                    assert_eq!(&rendered, baseline);
                } else {
                    baseline = Some(rendered);
                }
                let popup = tooltip(&Item::Pane("1".to_owned()), &state);
                assert_eq!(popup.title.plain(), state.panes[0].name);
                assert_eq!(popup.subtitle, PopupRow::default());
                assert_eq!(popup.intro, harness_popup.intro);
                assert_eq!(popup.sections, harness_popup.sections);
                assert_eq!(popup.rows()[1], popup.intro[0]);
                assert_eq!(
                    render_row(
                        detail_row,
                        &state,
                        &Theme::default(),
                        80,
                        RowStyle::default(),
                    ),
                    detail
                );
                for row in &rows {
                    for columns in 0..=40 {
                        assert_eq!(
                            width(&visible(&render_row(
                                row,
                                &state,
                                &Theme::default(),
                                columns,
                                RowStyle {
                                    hovered: true,
                                    ..RowStyle::default()
                                },
                            ))),
                            columns
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn recognized_harnesses_keep_unknown_state_and_distinct_attention() {
        let mut state = snapshot();
        let rows = tree(&state, &Default::default());
        let pane_row = rows
            .iter()
            .find(|row| matches!(row.kind, RowKind::Pane { .. }))
            .unwrap();
        for harness in ["codex", "cursor-agent", "agy", "claude-code"] {
            state.panes[0].harness = Some(harness.to_owned());
            for (status, attention) in [
                (Status::Unknown, None),
                (Status::Idle, None),
                (Status::Working, None),
                (Status::Done, None),
                (Status::Blocked, Some(Attention::Permission)),
                (Status::Blocked, Some(Attention::Question)),
                (Status::Blocked, Some(Attention::Auth)),
                (Status::Blocked, Some(Attention::Error)),
            ] {
                state.panes[0].status = status;
                state.panes[0].attention = attention;
                let rendered = render_row(
                    pane_row,
                    &state,
                    &Theme::default(),
                    40,
                    RowStyle {
                        selected: false,
                        hovered: false,
                        selected_group: false,
                        clock_frame: 0,
                    },
                );
                assert!(visible(&rendered).contains(status_icon(status, attention)));
                assert!(visible(&rendered).contains(harness_icon(harness)));
                assert!(rendered.contains(harness_color(harness, &Theme::default())));
                let popup = tooltip(&Item::Pane("1".to_owned()), &state);
                assert!(
                    popup
                        .subtitle
                        .plain()
                        .contains(status_icon(status, attention))
                );
                assert!(
                    popup
                        .subtitle
                        .plain()
                        .contains(harness_name(harness).unwrap())
                );
                assert_eq!(popup.subtitle.spans[0].color, PopupColor::Native(status));
                assert_eq!(
                    popup.title.spans[0].color,
                    PopupColor::Harness(harness.to_owned())
                );
            }
        }
    }

    #[test]
    fn workspace_status_requires_a_member_harness_without_hiding_ci_or_counts() {
        let mut state = snapshot();
        state.panes[0].branch_changes = Some(changes());
        let mut other = state.panes[0].clone();
        other.id = "2".to_owned();
        state.panes.push(other.clone());
        let item = Item::Workspace("workspace".to_owned());
        let harness_popup = tooltip(&item, &state);
        state.panes[0].harness = None;
        state.panes[1].harness = Some("cargo".to_owned());
        other.id = "3".to_owned();
        other.workspace_id = "elsewhere".to_owned();
        state.panes.push(other);
        let collapsed = tree(&state, &["workspace".to_owned()].into());
        let expanded = tree(&state, &Default::default());
        let mut baseline = None;
        for status in [
            Status::Unknown,
            Status::Working,
            Status::Blocked,
            Status::Done,
        ] {
            state.workspaces[0].status = status;
            state.workspaces[0].attention = Some(Attention::Permission);
            let popup = tooltip(&item, &state);
            assert_eq!(popup.sections, harness_popup.sections);
            assert!(popup.subtitle.plain().contains('2'));
            assert!(
                popup
                    .subtitle
                    .spans
                    .iter()
                    .all(|span| span.color == PopupColor::Muted)
            );
            let rendered = render_row(
                &collapsed[0],
                &state,
                &Theme::default(),
                50,
                RowStyle {
                    selected: false,
                    hovered: false,
                    selected_group: false,
                    clock_frame: 0,
                },
            );
            assert!(visible(&rendered).contains(WORKFLOW));
            assert!(!visible(&rendered).contains(status_icon(status, Some(Attention::Permission))));
            if let Some(baseline) = &baseline {
                assert_eq!(&rendered, baseline);
            } else {
                baseline = Some(rendered);
            }
            for columns in 0..=60 {
                assert_eq!(
                    width(&visible(&render_row(
                        &collapsed[0],
                        &state,
                        &Theme::default(),
                        columns,
                        RowStyle::default(),
                    ))),
                    columns
                );
            }
        }
        let expanded_before = render_row(
            &expanded[0],
            &state,
            &Theme::default(),
            50,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        state.panes[1].harness = Some("codex".to_owned());
        let popup = tooltip(&item, &state);
        assert_eq!(
            popup.subtitle.spans[0].color,
            PopupColor::Native(Status::Done)
        );
        let rendered = render_row(
            &collapsed[0],
            &state,
            &Theme::default(),
            50,
            RowStyle {
                selected: false,
                hovered: false,
                selected_group: false,
                clock_frame: 0,
            },
        );
        assert!(visible(&rendered).contains(status_icon(Status::Done, None)));
        assert!(visible(&rendered).contains(WORKFLOW));
        assert_eq!(
            render_row(
                &expanded[0],
                &state,
                &Theme::default(),
                50,
                RowStyle::default(),
            ),
            expanded_before
        );
    }
}
