use serde::{Deserialize, Serialize};
use shephrd::ci_contract::{
    ChangeRequestState, CiFetchState, CiRefreshTarget, CiSnapshot, CiStatus, ForgeProvider,
};
use shephrd::{Attention, ProjectIdentity, Snapshot, Status};
use unicode_width::UnicodeWidthStr;

pub const RESET: &str = "\u{1b}[0m";
// Keep the sidebar palette independent of Zellij's UI theme.
pub const BASE: &str = "\u{1b}[22;38;2;205;214;244;48;2;24;24;37m";
pub const MUTED: &str = "\u{1b}[22;38;2;108;112;134m";
pub const BRIGHT: &str = "\u{1b}[22;38;2;166;173;200m";
const TEXT: &str = "\u{1b}[22;38;2;205;214;244m";
const ADDED: &str = "\u{1b}[22;2;38;2;128;158;126m";
const DELETED: &str = "\u{1b}[22;2;38;2;174;119;134m";
const WORKFLOW: &str = "\u{f52e}"; // nf-oct-workflow

pub struct Theme {
    pub selected: &'static str,
    pub hovered: &'static str,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selected: "\u{1b}[48;2;49;50;68m",
            hovered: "\u{1b}[48;2;69;71;90m",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    Workspace { index: usize, collapsed: bool },
    Projects { index: usize, has_children: bool },
    Pane { index: usize, last: bool },
    Detail { index: usize, last: bool },
}

pub fn tree(state: &Snapshot, collapsed: &std::collections::BTreeSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for (index, workspace) in state.workspaces.iter().enumerate() {
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

/// Hit testing follows the actual item, never an overlapping hover outline.
pub fn item_at(rows: &[TreeRow], index: usize) -> Option<Item> {
    rows.get(index)?.item.clone()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinSide {
    Left,
    Right,
}

pub fn sidebar_edges(
    top: bool,
    bottom: bool,
    joined: Option<JoinSide>,
) -> (&'static str, &'static str) {
    if top && bottom {
        return match joined {
            Some(JoinSide::Left) => ("─", "│"),
            Some(JoinSide::Right) => ("│", "─"),
            None => ("│", "│"),
        };
    }
    let left = if joined == Some(JoinSide::Left) {
        if top || bottom { "─" } else { " " }
    } else if top {
        "╭"
    } else if bottom {
        "╰"
    } else {
        "│"
    };
    let right = if joined == Some(JoinSide::Right) {
        if top || bottom { "─" } else { " " }
    } else if top {
        "╮"
    } else if bottom {
        "╯"
    } else {
        "│"
    };
    (left, right)
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

fn branch_label(pane: &shephrd::Pane) -> Option<std::borrow::Cow<'_, str>> {
    pane.branch
        .as_deref()
        .map(clean)
        .filter(|branch| !branch.trim().is_empty())
}

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate(text: &str, columns: usize) -> String {
    let text = clean(text);
    if width(&text) <= columns {
        return text.into_owned();
    }
    if columns == 0 {
        return String::new();
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

pub fn status_color(status: Status) -> &'static str {
    match status {
        Status::Idle => "\u{1b}[38;2;166;227;161m",
        Status::Working => "\u{1b}[38;2;247;227;166m",
        Status::Blocked => "\u{1b}[38;2;243;139;168m",
        Status::Done => "\u{1b}[38;2;122;213;213m",
        Status::Unknown => "\u{1b}[38;2;108;112;134m",
    }
}

/// Fixed FNV-1a, rather than a randomized/default hasher, survives owner recovery.
pub fn project_color(project: Option<&ProjectIdentity>) -> &'static str {
    let Some(project) = project.filter(|project| project.repository) else {
        return MUTED;
    };
    const PALETTE: [&str; 6] = [
        "\u{1b}[38;2;137;180;250m",
        "\u{1b}[38;2;203;166;247m",
        "\u{1b}[38;2;148;226;213m",
        "\u{1b}[38;2;250;179;135m",
        "\u{1b}[38;2;245;194;231m",
        "\u{1b}[38;2;180;190;254m",
    ];
    let hash = project
        .id
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    PALETTE[(hash % PALETTE.len() as u64) as usize]
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

fn ci_color(status: CiStatus) -> &'static str {
    match status {
        CiStatus::Failed => status_color(Status::Blocked),
        CiStatus::Passed => status_color(Status::Idle),
        CiStatus::Pending | CiStatus::Running => status_color(Status::Working),
        _ => MUTED,
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
    changes: Option<&shephrd::BranchChanges>,
    columns: usize,
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
                "{ADDED}+{}{MUTED} {DELETED}-{}{MUTED}",
                total.added, total.deleted
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

fn project_labels(projects: &[ProjectIdentity], columns: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for (index, project) in projects.iter().enumerate() {
        if used >= columns {
            break;
        }
        if index > 0 {
            let separator = truncate(", ", columns - used);
            used += width(&separator);
            result.push_str(MUTED);
            result.push_str(&separator);
        }
        let name = truncate(&project.name, columns.saturating_sub(used));
        used += width(&name);
        result.push_str(project_color(Some(project)));
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
    pub fn ansi(&self) -> &'static str {
        match self {
            Self::Text => TEXT,
            Self::Muted => MUTED,
            Self::Added => "\u{1b}[22;38;2;166;227;161m",
            Self::Deleted => "\u{1b}[22;38;2;243;139;168m",
            Self::Native(status) => status_color(*status),
            Self::Ci(status) => ci_color(*status),
            Self::Project(project) => project_color(Some(project)),
            Self::Harness(harness) => harness_color(harness),
            Self::ChangeRequest(state) => request_presentation(*state).1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupSpan {
    pub text: String,
    pub color: PopupColor,
    pub bold: bool,
}

impl PopupSpan {
    fn new(text: impl Into<String>, color: PopupColor) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
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

    pub fn plain(&self) -> String {
        self.spans
            .iter()
            .chain(&self.suffix)
            .map(|span| clean(&span.text))
            .collect()
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
        let mut rows = vec![self.title.clone(), self.subtitle.clone()];
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
    let mut text = format!("{} {}", status_icon(status, attention), status.label());
    if status == Status::Blocked {
        text.push_str(" · ");
        text.push_str(attention.map(Attention::label).unwrap_or("Needs attention"));
    }
    PopupRow::text(text, PopupColor::Native(status))
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

fn request_presentation(state: ChangeRequestState) -> (&'static str, &'static str) {
    match state {
        ChangeRequestState::Draft => ("\u{f4dd}", MUTED), // nf-oct-git_pull_request_draft
        ChangeRequestState::Open => ("\u{f407}", status_color(Status::Idle)),
        ChangeRequestState::Merged => ("\u{f419}", "\u{1b}[22;38;2;203;166;247m"),
        ChangeRequestState::Closed => ("\u{f4dc}", status_color(Status::Blocked)),
    }
}

fn forge_repository(ci: &CiSnapshot, project: Option<&ProjectIdentity>) -> PopupRow {
    let mut repository = PopupRow::text(
        format!(
            "{} {}",
            provider_icon(ci.repository.provider),
            ci.repository.path
        ),
        project
            .cloned()
            .map(PopupColor::Project)
            .unwrap_or(PopupColor::Text),
    );
    repository.link = safe_link(&ci.repository.web_url).map(str::to_owned);
    repository
}

fn request_row(request: &shephrd::ci_contract::ChangeRequest) -> PopupRow {
    PopupRow {
        spans: vec![
            PopupSpan::new(
                format!(
                    "{}  #{} ",
                    request_presentation(request.state).0,
                    request.number
                ),
                PopupColor::ChangeRequest(request.state),
            ),
            PopupSpan::new(request.title.clone(), PopupColor::Text),
        ],
        link: safe_link(&request.url).map(str::to_owned),
        ..PopupRow::default()
    }
}

fn forge_context(ci: &CiSnapshot, project: Option<&ProjectIdentity>) -> Vec<PopupRow> {
    let mut rows = vec![forge_repository(ci, project)];
    rows.extend(ci.change_request.as_ref().map(request_row));
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

fn ci_rows(ci: &CiSnapshot) -> Vec<PopupRow> {
    let status = ci_state(ci);
    let mut rows = vec![ci_summary(ci)];
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

fn status_icon(status: Status, attention: Option<Attention>) -> &'static str {
    // Reserve a trailing cell for Nerd Font glyphs that paint double-width.
    match status {
        Status::Idle => "\u{f03e6} ",    // nf-md-pause_circle_outline
        Status::Working => "\u{f0996} ", // nf-md-progress_clock
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

fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "claude" | "claude-code" => "\u{ec82}", // nf-cod-claude
        "omp" => "\u{f03ff}",                   // nf-md-pi
        "codex" => "\u{ec81}",                  // nf-cod-openai
        "opencode" => "\u{f0174}",              // nf-md-code_tags
        "agy" | "antigravity" => "\u{f0463}",   // nf-md-rocket
        _ => "\u{f167a}",                       // nf-md-robot_outline
    }
}

fn harness_color(harness: &str) -> &'static str {
    match harness {
        "claude" | "claude-code" => "\u{1b}[38;2;250;179;135m",
        "omp" => "\u{1b}[38;2;203;166;247m",
        "codex" => "\u{1b}[38;2;166;227;161m",
        "opencode" => "\u{1b}[38;2;137;180;250m",
        "agy" | "antigravity" => "\u{1b}[38;2;148;226;213m",
        _ => TEXT,
    }
}

pub fn render_row(
    row: &TreeRow,
    state: &Snapshot,
    theme: &Theme,
    columns: usize,
    selected: bool,
    hovered: bool,
    selected_pane_group: bool,
) -> String {
    // Permanent outer gutters keep the hover outline clear of text and status glyphs.
    let left = if columns > 0 { " " } else { "" };
    let right = if columns > 1 { " " } else { "" };
    let content = render_content_row(
        row,
        state,
        theme,
        columns.saturating_sub(2),
        selected,
        hovered,
        selected_pane_group,
    );
    let background = if selected {
        theme.selected
    } else if hovered {
        theme.hovered
    } else {
        ""
    };
    format!("{RESET}{BASE}{background}{left}{content}{BASE}{background}{right}{RESET}")
}

fn render_content_row(
    row: &TreeRow,
    state: &Snapshot,
    theme: &Theme,
    columns: usize,
    selected: bool,
    hovered: bool,
    selected_pane_group: bool,
) -> String {
    let background = if selected {
        theme.selected
    } else if hovered {
        theme.hovered
    } else {
        ""
    };
    let guides = if selected || selected_pane_group {
        BRIGHT
    } else {
        MUTED
    };
    let body = match &row.kind {
        RowKind::Workspace { index, collapsed } => {
            let workspace = &state.workspaces[*index];
            let dot = truncate("● ", columns);
            let icon = if *collapsed {
                truncate(
                    status_icon(workspace.status, workspace.attention),
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
                "{TEXT}{dot}{TEXT}\u{1b}[1m{name}\u{1b}[22m{MUTED}{tail}{}{}{badge}{}{icon}",
                if badge.is_empty() { "" } else { " " },
                ci_status.map(ci_color).unwrap_or(MUTED),
                status_color(workspace.status),
            )
        }
        RowKind::Projects {
            index,
            has_children,
        } => {
            let projects = &state.workspaces[*index].projects;
            let lead = truncate(if *has_children { "│ " } else { "  " }, columns);
            format!(
                "{guides}{lead}{MUTED}{}",
                project_labels(projects, columns.saturating_sub(width(&lead)))
            )
        }
        RowKind::Pane { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { "╰─╴" } else { "├─╴" }, columns);
            let indicator = truncate("● ", columns.saturating_sub(width(&lead)));
            let status = truncate(
                status_icon(pane.status, pane.attention),
                columns.saturating_sub(width(&lead) + width(&indicator)),
            );
            let available =
                columns.saturating_sub(width(&lead) + width(&indicator) + width(&status));
            // Keep the harness beside its status, with a trailing cell for wide glyphs.
            let icon = pane.harness.as_deref().map(harness_icon).unwrap_or("");
            let logo_color = pane.harness.as_deref().map(harness_color).unwrap_or(TEXT);
            let icon_columns = width(icon) + usize::from(!icon.is_empty());
            let (icon, icon_columns) = if available >= icon_columns {
                (icon, icon_columns)
            } else {
                ("", 0)
            };
            let name = fit(&pane.name, available.saturating_sub(icon_columns));
            format!(
                "{guides}{lead}{}{indicator}{logo_color}{icon}{}{TEXT}{name}{}{status}",
                project_color(pane.project.as_ref()),
                if icon.is_empty() { "" } else { " " },
                status_color(pane.status),
            )
        }
        RowKind::Detail { index, last } => {
            let pane = &state.panes[*index];
            let lead = truncate(if *last { "     " } else { "│    " }, columns);
            let available = columns.saturating_sub(width(&lead));
            let Some(branch) = branch_label(pane) else {
                return format!(
                    "{RESET}{BASE}{background}{guides}{lead}{MUTED}{}{RESET}",
                    fit(&pane.description, available)
                );
            };
            let request = pane.ci.as_ref().and_then(|ci| ci.change_request.as_ref());
            let (glyph, glyph_color) = request
                .map(|request| request_presentation(request.state))
                .unwrap_or(("\u{f062c}", MUTED));
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
            );
            let counts_columns = counts.as_ref().map_or(0, |(_, columns)| columns + 1);
            let branch = truncate(&branch, content_columns.saturating_sub(counts_columns));
            let fill = content_columns.saturating_sub(width(&branch) + counts_columns);
            format!(
                "{guides}{lead}{glyph_color}{symbol}{symbol_space}{MUTED}{branch}{}{}{}{}{}{badge}",
                if counts.is_some() { " " } else { "" },
                counts.map(|(text, _)| text).unwrap_or_default(),
                " ".repeat(fill),
                if badge.is_empty() { "" } else { " " },
                ci_status.map(ci_color).unwrap_or(MUTED),
            )
        }
    };
    format!("{RESET}{BASE}{background}{body}{RESET}")
}

fn repository_heading(project: &ProjectIdentity, ci: Option<&CiSnapshot>) -> PopupRow {
    ci.map(|ci| forge_repository(ci, Some(project)))
        .unwrap_or_else(|| {
            PopupRow::text(project.name.clone(), PopupColor::Project(project.clone()))
        })
}

fn branch_rows(branch: Option<&str>, changes: Option<&shephrd::BranchChanges>) -> Vec<PopupRow> {
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
    let base = changes
        .base_ref
        .strip_prefix("refs/heads/")
        .or_else(|| changes.base_ref.strip_prefix("refs/remotes/"))
        .unwrap_or(&changes.base_ref);
    row.right = vec![PopupSpan::new(
        format!("\u{f419} {base}"),
        PopupColor::Muted,
    )];
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
                let mut subtitle = native_summary(workspace.status, workspace.attention);
                let panes: Vec<_> = state
                    .panes
                    .iter()
                    .filter(|p| &p.workspace_id == id)
                    .collect();
                subtitle.spans.push(PopupSpan::new(
                    format!(" · {} terminals", panes.len()),
                    PopupColor::Muted,
                ));
                let groups = workspace_ci(state, id);
                let mut sections = Vec::new();
                for project in &workspace.projects {
                    let sources: Vec<_> = groups
                        .iter()
                        .filter(|(identity, _)| identity.id == project.id)
                        .collect();
                    let mut rows = Vec::new();
                    let mut contexts: Vec<&shephrd::Pane> = Vec::new();
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
                            ));
                        }
                    }
                    if project.repository && contexts.is_empty() {
                        rows.extend(branch_rows(None, None));
                    }
                    let mut requests = std::collections::BTreeSet::new();
                    for (_, ci) in &sources {
                        if let Some(request) = &ci.change_request
                            && requests.insert(request.number)
                        {
                            rows.push(request_row(request));
                        }
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
                if let Some(harness) = &pane.harness {
                    title.spans.push(
                        PopupSpan::new(
                            format!("{} ", harness_icon(harness)),
                            PopupColor::Harness(harness.clone()),
                        )
                        .bold(),
                    );
                }
                title
                    .spans
                    .push(PopupSpan::new(pane.name.clone(), PopupColor::Text).bold());
                let mut subtitle = native_summary(pane.status, pane.attention);
                if let Some(harness) = &pane.harness {
                    subtitle.spans.push(PopupSpan::new(
                        format!(" · {}", shephrd::agent_contract::harness_name(harness)),
                        PopupColor::Muted,
                    ));
                }
                let mut intro = Vec::new();
                if let Some(cwd) = pane.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
                    intro.push(PopupRow::text(format!("\u{f07b} {cwd}"), PopupColor::Muted));
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
                let mut repository = pane
                    .ci
                    .as_ref()
                    .map(|ci| forge_context(ci, pane.project.as_ref()))
                    .unwrap_or_else(|| {
                        pane.project
                            .as_ref()
                            .map(|project| vec![repository_heading(project, None)])
                            .unwrap_or_default()
                    });
                if pane.branch_changes.is_some()
                    || pane.branch.is_some()
                    || pane.ci.is_some()
                    || pane
                        .project
                        .as_ref()
                        .is_some_and(|project| project.repository)
                {
                    let branches = branch_rows(
                        pane.branch
                            .as_deref()
                            .or_else(|| pane.ci.as_ref().and_then(|ci| ci.branch.as_deref())),
                        pane.branch_changes.as_ref(),
                    );
                    repository.splice(
                        usize::from(!repository.is_empty())..usize::from(!repository.is_empty()),
                        branches,
                    );
                }
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
    Create {
        name: Input,
        cwd: Input,
        cwd_active: bool,
    },
    CreatePane {
        workspace_id: String,
        cwd: Input,
    },
    Rename {
        item: Item,
        input: Input,
    },
    Close {
        item: Item,
        name: String,
    },
}

impl Modal {
    pub fn input_mut(&mut self) -> Option<&mut Input> {
        match self {
            Self::Create {
                name,
                cwd,
                cwd_active,
            } => Some(if *cwd_active { cwd } else { name }),
            Self::CreatePane { cwd, .. } => Some(cwd),
            Self::Rename { input, .. } => Some(input),
            Self::Close { .. } => None,
        }
    }

    pub fn lines(&self, columns: usize) -> (Vec<String>, Option<(usize, usize)>) {
        match self {
            Self::Create {
                name,
                cwd,
                cwd_active,
            } => {
                let (name_line, name_cursor) = name.visible(columns.saturating_sub(2));
                let (cwd_line, cwd_cursor) = cwd.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Create workspace".to_owned(),
                        "Name (optional):".to_owned(),
                        format!("  {name_line}"),
                        "Directory (optional):".to_owned(),
                        format!("  {cwd_line}"),
                        String::new(),
                        "Tab field · Enter create".to_owned(),
                        "Esc cancel".to_owned(),
                    ],
                    Some(if *cwd_active {
                        (2 + cwd_cursor, 4)
                    } else {
                        (2 + name_cursor, 2)
                    }),
                )
            }
            Self::CreatePane { cwd, .. } => {
                let (line, cursor) = cwd.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Create terminal".to_owned(),
                        "Directory (optional):".to_owned(),
                        format!("  {line}"),
                        String::new(),
                        "Enter create · Esc cancel".to_owned(),
                    ],
                    Some((2 + cursor, 2)),
                )
            }
            Self::Rename { input, .. } => {
                let (line, cursor) = input.visible(columns.saturating_sub(2));
                (
                    vec![
                        "Rename".to_owned(),
                        String::new(),
                        format!("  {line}"),
                        String::new(),
                        "Enter save · Esc cancel".to_owned(),
                    ],
                    Some((2 + cursor, 2)),
                )
            }
            Self::Close { name, .. } => (
                vec![
                    "Close permanently?".to_owned(),
                    name.clone(),
                    String::new(),
                    "This terminates native terminals.".to_owned(),
                    "Enter / y confirm".to_owned(),
                    "Esc / n cancel".to_owned(),
                ],
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn identity(id: &str) -> ProjectIdentity {
        ProjectIdentity {
            id: id.to_owned(),
            name: "same-name".to_owned(),
            repository: true,
        }
    }

    fn ci(status: CiStatus) -> CiSnapshot {
        CiSnapshot {
            status,
            checks: vec![shephrd::ci_contract::CiCheck {
                name: "unit tests".to_owned(),
                status,
                url: Some("https://github.com/owner/repo/actions/runs/12".to_owned()),
                duration_seconds: Some(123),
            }],
            change_request: Some(shephrd::ci_contract::ChangeRequest {
                number: 7,
                title: "Improve project metadata".to_owned(),
                url: "https://github.com/owner/repo/pull/7".to_owned(),
                state: ChangeRequestState::Open,
            }),
            fetch_state: CiFetchState::Ready,
            updated_at: 1,
            stale: false,
            error: None,
            repository: shephrd::ci_contract::ForgeRepository {
                provider: ForgeProvider::Github,
                host: "github.com".to_owned(),
                path: "owner/repo".to_owned(),
                web_url: "https://github.com/owner/repo".to_owned(),
            },
            head_sha: "0123456789abcdef".to_owned(),
            branch: Some("feature/日本語".to_owned()),
            truncated: false,
        }
    }

    fn snapshot() -> Snapshot {
        let project = identity("/repo/.git");
        Snapshot {
            protocol: shephrd::PROTOCOL_VERSION,
            instance_id: "native:test".to_owned(),
            revision: 1,
            session: "test".to_owned(),
            connected: true,
            error: None,
            focused_pane_id: Some("1".to_owned()),
            workspaces: vec![shephrd::Workspace {
                id: "workspace".to_owned(),
                name: "日本語 workspace".to_owned(),
                active_tab_id: "tab".to_owned(),
                projects: vec![project.clone()],
                status: Status::Working,
                attention: None,
            }],
            panes: vec![shephrd::Pane {
                id: "1".to_owned(),
                terminal_id: "1".to_owned(),
                workspace_id: "workspace".to_owned(),
                tab_id: "tab".to_owned(),
                name: "日本語 agent".to_owned(),
                description: "Native activity".to_owned(),
                harness: Some("codex".to_owned()),
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
    fn project_colors_follow_identity_not_name_branch_or_lifecycle() {
        let project = identity("/repo/.git");
        let mut renamed = project.clone();
        renamed.name = "a new name".to_owned();
        assert_eq!(project_color(Some(&project)), project_color(Some(&renamed)));
        renamed.repository = false;
        assert_eq!(project_color(Some(&renamed)), MUTED);
        assert_eq!(project_color(None), MUTED);

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
        let color = project_color(state.panes[0].project.as_ref());
        assert!(
            render_row(projects, &state, &Theme::default(), 40, false, false, false)
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
            let rendered = render_row(pane, &state, &Theme::default(), 40, true, false, true);
            assert!(rendered.contains(&format!("{color}● ")));
            assert!(!visible(&rendered).contains("○"));
            let parent = render_row(workspace, &state, &Theme::default(), 40, false, true, false);
            assert!(parent.contains(&format!("{TEXT}● ")));
        }
    }

    #[test]
    fn ci_badges_reserve_unicode_cells_and_do_not_change_agent_state() {
        let mut state = snapshot();
        let rows = tree(&state, &Default::default());
        for stale in [false, true] {
            state.panes[0].ci.as_mut().unwrap().stale = stale;
            for row in &rows {
                for columns in 0..=80 {
                    let rendered =
                        render_row(row, &state, &Theme::default(), columns, true, false, true);
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
                false,
                false,
                false,
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
    fn workspace_ci_deduplicates_shared_commit_and_summarizes_sources_without_jobs() {
        let mut state = snapshot();
        let mut duplicate = state.panes[0].clone();
        duplicate.id = "2".to_owned();
        duplicate.cwd = Some("/repo-linked".to_owned());
        state.panes.push(duplicate);
        assert_eq!(workspace_ci(&state, "workspace").len(), 1);
        let popup = tooltip(&Item::Workspace("workspace".to_owned()), &state);
        let rows = popup.rows();
        assert_eq!(
            rows.iter()
                .filter(|row| row.plain().contains("unit tests"))
                .count(),
            0
        );
        assert_eq!(popup.sections.len(), 1);
        assert!(
            rows.iter().any(|row| row.plain().contains("#7")
                && row.plain().contains("Improve project metadata"))
        );
        assert!(!rows.iter().any(|row| row.plain().contains("01234567")));
        assert!(rows.iter().any(|row| row.plain().contains("ago")));
        assert!(!rows.iter().any(|row| row.plain().contains("/repo/.git")
            || row.plain().contains("https://")
            || row.plain().contains("Unix")));
        assert_eq!(
            rows.iter()
                .filter(|row| row.plain().contains("feature/日本語"))
                .count(),
            1
        );
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
        assert!(
            summaries
                .iter()
                .any(|row| row.plain().contains("Unavailable")
                    && row.plain().contains("provider unavailable 日本語")
                    && row.plain().contains("stale"))
        );
        assert!(
            !workspace_rows
                .iter()
                .any(|row| row.plain().contains("unit tests"))
        );

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

    fn changes() -> shephrd::BranchChanges {
        shephrd::BranchChanges {
            base_ref: "refs/remotes/origin/main".to_owned(),
            merge_base: "abcdef0123456789abcdef0123456789abcdef01".to_owned(),
            total: shephrd::DiffStat {
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
            assert!(branch_counts(Some(&changes), columns - 1).is_none());
            let (text, reserved) = branch_counts(Some(&changes), columns).unwrap();
            assert_eq!(visible(&text), expected);
            assert_eq!(reserved, columns);
        }
        assert!(branch_counts(None, 80).is_none());
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
        let expected = render_row(detail, &state, &Theme::default(), 40, false, false, false);
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
                    render_row(detail, &state, &Theme::default(), 40, false, false, false),
                    expected
                );
            }
        }
        state.panes[0].branch = None;
        let fallback = render_row(detail, &state, &Theme::default(), 40, false, false, false);
        assert!(visible(&fallback).contains("Native activity"));
        assert!(!fallback.contains(WORKFLOW));
        state.panes[0].harness = None;
        assert_eq!(
            render_row(detail, &state, &Theme::default(), 40, false, false, false),
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
        let status_before = render_row(pane, &state, &Theme::default(), 16, false, false, false);
        for columns in 0..=80 {
            let rendered = render_row(
                detail,
                &state,
                &Theme::default(),
                columns,
                false,
                false,
                false,
            );
            let text = visible(&rendered);
            assert_eq!(width(&text), columns);
            if rendered.contains(ADDED) {
                assert!(text.contains("+1234"));
                assert!(text.contains("-567"));
                assert!(rendered.contains(DELETED));
            }
            assert!(!text.contains("CI "));
        }
        let wide = render_row(detail, &state, &Theme::default(), 38, false, false, false);
        assert!(wide.contains(ADDED) && wide.contains(DELETED));
        let narrow = render_row(detail, &state, &Theme::default(), 16, false, false, false);
        assert!(!narrow.contains(ADDED) && !narrow.contains(DELETED));
        state.panes[0].branch = Some("topic".to_owned());
        let adjacent = render_row(detail, &state, &Theme::default(), 80, false, false, false);
        assert!(visible(&adjacent).contains("topic +1234 -567"));
        assert!(adjacent.contains(&format!("topic {ADDED}+1234")));
        state.panes[0].branch_changes = None;
        assert_eq!(
            status_before,
            render_row(pane, &state, &Theme::default(), 16, false, false, false)
        );
        assert!(
            !render_row(detail, &state, &Theme::default(), 80, false, false, false).contains(ADDED)
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
    fn request_presentation_matches_sidebar_and_popup_without_changing_native_status() {
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
            let context = forge_context(ci, Some(&identity("/repo/.git")));
            assert_eq!(
                context[0].link.as_deref(),
                Some(ci.repository.web_url.as_str())
            );
            assert_eq!(
                context[1].link.as_deref(),
                Some(ci.change_request.as_ref().unwrap().url.as_str())
            );
            assert!(!context[1].plain().contains('\u{1b}'));
            let rows = tree(&state, &Default::default());
            let detail = rows
                .iter()
                .find(|row| matches!(row.kind, RowKind::Detail { .. }))
                .unwrap();
            let sidebar = render_row(detail, &state, &Theme::default(), 38, false, false, false);
            assert!(sidebar.contains(request_presentation(request_state).0));
            assert!(sidebar.contains(request_presentation(request_state).1));
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
            assert_eq!(names[0].plain(), "\u{f09b} owner/repo");
            assert_eq!(
                names[0].link.as_deref(),
                Some("https://github.com/owner/repo")
            );
            assert_eq!(
                names[0].spans[0].color.ansi(),
                project_color(state.panes[0].project.as_ref())
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
        };
        state.workspaces[0].projects.push(project.clone());
        let mut other = state.panes[0].clone();
        other.id = "3".to_owned();
        other.project = Some(project.clone());
        other.ci.as_mut().unwrap().repository.path = "someone/other".to_owned();
        other.ci.as_mut().unwrap().repository.web_url =
            "https://github.com/someone/other".to_owned();
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
                .filter(|row| row.link.as_deref() == Some("https://github.com/owner/repo"))
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
}
