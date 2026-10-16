//! Cached fuzzy search over published pane metadata, never terminal contents.

use std::fmt::{self, Write};
use std::ops::Range;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};
use bkpr::agent_contract::harness_name;
use bkpr::ci_contract::{ChangeRequestState, CiFetchState, ForgeProvider, IssueTracker};
use bkpr::{Pane, Snapshot};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub pane: usize,
    pub score: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Field {
    All,
    Pr,
    Branch,
    Project,
    Workspace,
    Harness,
    Status,
    Ci,
    Cwd,
    Ticket,
}

impl Field {
    fn named(name: &str) -> Option<Self> {
        [
            ("pr", Self::Pr),
            ("branch", Self::Branch),
            ("project", Self::Project),
            ("workspace", Self::Workspace),
            ("harness", Self::Harness),
            ("status", Self::Status),
            ("ci", Self::Ci),
            ("cwd", Self::Cwd),
            ("ticket", Self::Ticket),
        ]
        .into_iter()
        .find_map(|(prefix, field)| name.eq_ignore_ascii_case(prefix).then_some(field))
    }
}

/// A metadata value, retaining the field that made a filter applicable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Value {
    pub field: Field,
    pub index: usize,
}

impl Value {
    pub const fn new(field: Field, index: usize) -> Self {
        Self { field, index }
    }
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "text",
            Self::Pr => "pr",
            Self::Branch => "branch",
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::Harness => "harness",
            Self::Status => "status",
            Self::Ci => "ci",
            Self::Cwd => "cwd",
            Self::Ticket => "ticket",
        }
    }
}

#[derive(Default)]
struct Text {
    utf8: String,
    unicode: Vec<char>,
    values: Vec<Range<usize>>,
    byte_values: Vec<Range<usize>>,
    byte_offsets: Vec<usize>,
    ascii: bool,
}

impl Text {
    fn clear(&mut self) {
        self.utf8.clear();
        self.unicode.clear();
        self.values.clear();
        self.byte_values.clear();
        self.byte_offsets.clear();
    }

    fn add(&mut self, text: &str) {
        if !text.trim().is_empty() {
            let start = self.utf8.len();
            self.utf8.push_str(text);
            self.values.push(start..self.utf8.len());
        }
    }

    fn formatted(&mut self, args: fmt::Arguments<'_>) {
        let start = self.utf8.len();
        // Writing to a String cannot fail.
        self.utf8.write_fmt(args).unwrap();
        if self.utf8.len() > start {
            self.values.push(start..self.utf8.len());
        }
    }

    fn prepare(&mut self) {
        self.ascii = self.utf8.is_ascii();
        self.byte_values.extend(self.values.iter().cloned());
        if !self.ascii {
            for range in &mut self.values {
                let start = self.unicode.len();
                for (offset, grapheme) in self.utf8[range.clone()].grapheme_indices(true) {
                    self.byte_offsets.push(range.start + offset);
                    self.unicode.push(grapheme.chars().next().unwrap());
                }
                *range = start..self.unicode.len();
            }
        }
    }

    fn view(&self) -> Utf32Str<'_> {
        if self.ascii {
            Utf32Str::Ascii(self.utf8.as_bytes())
        } else {
            Utf32Str::Unicode(&self.unicode)
        }
    }

    fn score(&self, atom: &Atom, matcher: &mut Matcher) -> Option<u16> {
        let text = self.view();
        self.values
            .iter()
            .filter_map(|range| atom.score(text.slice(range.clone()), matcher))
            .max()
    }

    fn glyph(&self, value: usize, index: usize) -> &str {
        let range = &self.values[value];
        if self.ascii {
            &self.utf8[range.start + index..range.start + index + 1]
        } else {
            let start = self.byte_offsets[range.start + index];
            let end = if index + 1 < range.len() {
                self.byte_offsets[range.start + index + 1]
            } else {
                self.byte_values[value].end
            };
            &self.utf8[start..end]
        }
    }
}

#[derive(Default)]
struct Document {
    fields: [Text; 10],
}

impl Document {
    fn field(&mut self, field: Field) -> &mut Text {
        &mut self.fields[field as usize]
    }

    fn rebuild(&mut self, pane: &Pane, snapshot: &Snapshot) {
        for field in &mut self.fields {
            field.clear();
        }
        let all = self.field(Field::All);
        all.add(&pane.name);
        all.add(&pane.description);
        all.add(&pane.id);
        all.add(&pane.terminal_id);

        let workspace = self.field(Field::Workspace);
        workspace.add(&pane.workspace_id);
        if let Some(value) = snapshot
            .workspaces
            .iter()
            .find(|ws| ws.id == pane.workspace_id)
        {
            workspace.add(&value.name);
        }
        if let Some(cwd) = &pane.cwd {
            self.field(Field::Cwd).add(cwd);
        }
        // Generic OSC producers have no displayed program status. Do not turn
        // their raw status into a harness/status search result either.
        if let Some((id, name)) = pane
            .harness
            .as_deref()
            .and_then(|id| harness_name(id).map(|name| (id, name)))
        {
            let harness = self.field(Field::Harness);
            harness.add(id);
            harness.add(name);
            let status = self.field(Field::Status);
            status.add(pane.status.label());
            if let Some(attention) = pane.attention {
                status.add(attention.label());
            }
        }
        if let Some(project) = &pane.project {
            let field = self.field(Field::Project);
            field.add(&project.name);
            field.add(&project.id);
            if !project.repository {
                self.field(Field::All)
                    .add("Local directory · not a Git repository");
            }
        }
        if let Some(branch) = &pane.branch {
            self.field(Field::Branch).add(branch);
        }
        if let Some(changes) = &pane.branch_changes {
            let branch = self.field(Field::Branch);
            branch.add(&changes.base_ref);
            branch.add(&changes.merge_base);
            self.field(Field::All).formatted(format_args!(
                "+{} -{} {} binary files",
                changes.total.added, changes.total.deleted, changes.total.binary_files,
            ));
        }
        if let Some(ci) = &pane.ci {
            let project = self.field(Field::Project);
            project.add(match ci.repository.provider {
                ForgeProvider::Github => "GitHub",
            });
            project.add(&ci.repository.host);
            project.add(&ci.repository.path);
            project.add(&ci.repository.web_url);
            if let Some(branch) = &ci.branch {
                self.field(Field::Branch).add(branch);
            }
            if let Some(request) = &ci.change_request {
                let pr = self.field(Field::Pr);
                pr.formatted(format_args!("#{}", request.number));
                pr.add(&request.title);
                pr.add(&request.description);
                pr.add(&request.url);
                pr.add(match request.state {
                    ChangeRequestState::Draft => "draft",
                    ChangeRequestState::Open => "open",
                    ChangeRequestState::Merged => "merged",
                    ChangeRequestState::Closed => "closed",
                });
                let ticket = self.field(Field::Ticket);
                for issue in &request.issues {
                    ticket.add(match issue.provider {
                        IssueTracker::Linear => "Linear",
                    });
                    ticket.add(&issue.identifier);
                    if let Some(title) = &issue.title {
                        ticket.add(title);
                    }
                    ticket.add(&issue.url);
                }
            }
            let field = self.field(Field::Ci);
            field.add("CI checks");
            field.add(ci.status.label());
            field.add(match ci.fetch_state {
                CiFetchState::Loading => "loading fetching checks",
                CiFetchState::Ready => "ready",
                CiFetchState::Unavailable => "unavailable",
            });
            field.add(&ci.head_sha);
            if ci.stale {
                field.add("stale cached results; current check state is not confirmed");
            }
            if ci.truncated {
                field.add("incomplete provider pagination limit reached");
            }
            if let Some(error) = &ci.error {
                field.add(error);
            }
            for check in &ci.checks {
                field.add(&check.name);
                field.add(check.status.label());
                if let Some(url) = &check.url {
                    field.add(url);
                }
                if let Some(seconds) = check.duration_seconds {
                    field.formatted(format_args!("{seconds}s"));
                    match seconds {
                        60..3600 => {
                            field.formatted(format_args!("{}:{:02}", seconds / 60, seconds % 60))
                        }
                        3600.. => field.formatted(format_args!(
                            "{}h {}m",
                            seconds / 3600,
                            seconds % 3600 / 60
                        )),
                        _ => {}
                    }
                }
            }
            if ci.checks.is_empty() {
                field.add("no check results available");
            }
        } else {
            self.field(Field::All).add("Checks not available");
        }

        // Preserve metadata value boundaries: one fuzzy term must match one
        // value, while separate AND terms may match different values or fields.
        // Presegment only at metadata refresh; ranking borrows these buffers.
        for field in &mut self.fields {
            field.prepare();
        }
    }
}

struct Term {
    field: Field,
    // An empty recognized filter is a presence test, not an empty fuzzy atom.
    atom: Option<Atom>,
}

struct Highlight {
    value: Value,
    positions: Range<usize>,
}

/// Borrowed annotations for one displayed document; matcher scratch stays in Index.
pub(crate) struct Annotations<'a> {
    document: &'a Document,
    highlights: &'a [Highlight],
    positions: &'a [u32],
}

impl Annotations<'_> {
    pub fn len(&self, value: Value) -> usize {
        self.document.fields[value.field as usize].values[value.index].len()
    }

    pub fn glyph(&self, value: Value, index: usize) -> &str {
        self.document.fields[value.field as usize].glyph(value.index, index)
    }

    pub fn matched(&self, value: Value, index: usize) -> bool {
        self.highlights.iter().any(|highlight| {
            highlight.value == value
                && self.positions[highlight.positions.clone()].contains(&(index as u32))
        })
    }

    pub fn match_bounds(&self, value: Value) -> Option<(usize, usize)> {
        let mut bounds: Option<(usize, usize)> = None;
        for position in self
            .highlights
            .iter()
            .filter(|highlight| highlight.value == value)
            .flat_map(|highlight| &self.positions[highlight.positions.clone()])
        {
            let position = *position as usize;
            bounds = Some(match bounds {
                Some((first, last)) => (first.min(position), last.max(position)),
                None => (position, position),
            });
        }
        bounds
    }

    pub fn contexts<'a>(&'a self, visible: &'a [Value]) -> impl Iterator<Item = Value> + 'a {
        self.highlights
            .iter()
            .enumerate()
            .filter_map(move |(index, highlight)| {
                (!visible.contains(&highlight.value)
                    && !self.highlights[..index]
                        .iter()
                        .any(|earlier| earlier.value == highlight.value))
                .then_some(highlight.value)
            })
    }
}

/// Reusable matcher, metadata documents, parsed query, and ranked results.
/// Pane indices always refer to the most recently rebuilt snapshot.
#[derive(Default)]
pub struct Index {
    matcher: Matcher,
    documents: Vec<Document>,
    query: String,
    query_valid: bool,
    token: String,
    terms: Vec<Term>,
    matches: Vec<Match>,
    highlights: Vec<Highlight>,
    positions: Vec<u32>,
}

impl Index {
    pub fn rebuild(&mut self, snapshot: &Snapshot) {
        self.documents
            .resize_with(snapshot.panes.len(), Document::default);
        for (document, pane) in self.documents.iter_mut().zip(&snapshot.panes) {
            document.rebuild(pane, snapshot);
        }
        self.matches.clear();
        self.matches.reserve(snapshot.panes.len());
        self.query_valid = false;
    }

    pub fn search(&mut self, query: &str) -> &[Match] {
        if self.query_valid && self.query == query {
            return &self.matches;
        }
        if self.query != query || self.terms.is_empty() {
            self.parse(query);
            self.query.clear();
            self.query.push_str(query);
        }
        self.matches.clear();
        'candidate: for (pane, document) in self.documents.iter().enumerate() {
            let mut score = 0_u32;
            for term in &self.terms {
                let fields = match term.field {
                    Field::All => document.fields.as_slice(),
                    field => &document.fields[field as usize..=field as usize],
                };
                if let Some(atom) = &term.atom {
                    let Some(value) = fields
                        .iter()
                        .filter_map(|text| text.score(atom, &mut self.matcher))
                        .max()
                    else {
                        continue 'candidate;
                    };
                    score = score.saturating_add(u32::from(value));
                } else if !fields.iter().any(|text| !text.values.is_empty()) {
                    continue 'candidate;
                }
            }
            self.matches.push(Match { pane, score });
        }
        self.matches.sort_unstable_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.pane.cmp(&right.pane))
        });
        self.query_valid = true;
        &self.matches
    }

    pub fn results(&self) -> &[Match] {
        &self.matches
    }

    pub(crate) fn value(&self, pane: usize, field: Field, value: &str) -> Option<Value> {
        let text = &self.documents.get(pane)?.fields[field as usize];
        text.byte_values
            .iter()
            .position(|range| &text.utf8[range.clone()] == value)
            .map(|index| Value::new(field, index))
    }

    /// Annotate only a visible result, never rerank the corpus during rendering.
    /// Prefer matching displayed values; otherwise retain the best full-metadata
    /// value for a compact context excerpt. Presence tests have no glyph matches.
    pub(crate) fn annotate(&mut self, pane: usize, visible: &[Value]) -> Annotations<'_> {
        self.highlights.clear();
        self.positions.clear();
        let document = &self.documents[pane];
        for term in &self.terms {
            let Some(atom) = &term.atom else {
                continue;
            };
            let before = self.highlights.len();
            for &value in visible {
                if term.field != Field::All && term.field != value.field {
                    continue;
                }
                let text = &document.fields[value.field as usize];
                let start = self.positions.len();
                if atom
                    .indices(
                        text.view().slice(text.values[value.index].clone()),
                        &mut self.matcher,
                        &mut self.positions,
                    )
                    .is_some()
                {
                    self.highlights.push(Highlight {
                        value,
                        positions: start..self.positions.len(),
                    });
                } else {
                    self.positions.truncate(start);
                }
            }
            if self.highlights.len() != before {
                continue;
            }
            let mut best = None;
            for (field_index, text) in document.fields.iter().enumerate() {
                if term.field != Field::All && term.field as usize != field_index {
                    continue;
                }
                for (index, range) in text.values.iter().enumerate() {
                    if let Some(score) =
                        atom.score(text.view().slice(range.clone()), &mut self.matcher)
                        && best
                            .as_ref()
                            .is_none_or(|(previous, _, _)| score > *previous)
                    {
                        best = Some((score, field_index, index));
                    }
                }
            }
            if let Some((_, field_index, index)) = best {
                let text = &document.fields[field_index];
                let start = self.positions.len();
                let _ = atom.indices(
                    text.view().slice(text.values[index].clone()),
                    &mut self.matcher,
                    &mut self.positions,
                );
                let field = [
                    Field::All,
                    Field::Pr,
                    Field::Branch,
                    Field::Project,
                    Field::Workspace,
                    Field::Harness,
                    Field::Status,
                    Field::Ci,
                    Field::Cwd,
                    Field::Ticket,
                ][field_index];
                self.highlights.push(Highlight {
                    value: Value::new(field, index),
                    positions: start..self.positions.len(),
                });
            }
        }
        Annotations {
            document,
            highlights: &self.highlights,
            positions: &self.positions,
        }
    }

    fn parse(&mut self, query: &str) {
        self.terms.clear();
        self.token.clear();
        let mut quote = None;
        let mut chars = query.chars().peekable();
        while let Some(character) = chars.next() {
            match character {
                '\\' => {
                    // Preserve ordinary path backslashes. Escape syntax is
                    // limited to whitespace, quotes, and the escape itself.
                    if chars.peek().is_some_and(|next| {
                        next.is_whitespace() || matches!(next, '\\' | '\'' | '"')
                    }) {
                        self.token.push(chars.next().unwrap());
                    } else {
                        self.token.push(character);
                    }
                }
                '\'' | '"' if quote == Some(character) => quote = None,
                '\'' | '"'
                    if quote.is_none() && (self.token.is_empty() || self.token.ends_with(':')) =>
                {
                    quote = Some(character)
                }
                value if value.is_whitespace() && quote.is_none() => self.finish_term(),
                value => self.token.push(value),
            }
        }
        // An open quote is intentional during live typing; match its contents.
        self.finish_term();
    }

    fn finish_term(&mut self) {
        if self.token.is_empty() {
            return;
        }
        let (field, value) = self
            .token
            .split_once(':')
            .and_then(|(name, value)| Field::named(name).map(|field| (field, value)))
            .unwrap_or((Field::All, self.token.as_str()));
        self.terms.push(Term {
            field,
            atom: (!value.trim().is_empty()).then(|| {
                Atom::new(
                    value,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                )
            }),
        });
        self.token.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::ci_contract::{
        ChangeRequest, CiCheck, CiSnapshot, CiStatus, ForgeRepository, LinkedIssue,
    };
    use bkpr::{Attention, BranchChanges, DiffStat, ProjectIdentity, Status, Workspace};

    fn snapshot() -> Snapshot {
        let pane = Pane {
            id: "pane-1".into(),
            terminal_id: "terminal-1".into(),
            workspace_id: "engine".into(),
            tab_id: "tab-1".into(),
            name: "compiler".into(),
            description: "Investigating scheduler fairness".into(),
            harness: Some("claude".into()),
            agent_id: None,
            status: Status::Working,
            attention: Some(Attention::Question),
            cwd: Some("/work/engine:cache".into()),
            branch: Some("feature/queues".into()),
            branch_changes: Some(BranchChanges {
                base_ref: "origin/main".into(),
                merge_base: "abc123".into(),
                total: DiffStat {
                    added: 321,
                    deleted: 17,
                    binary_files: 0,
                },
            }),
            project: Some(ProjectIdentity {
                id: "git:engine".into(),
                name: "Engine".into(),
                repository: true,
            }),
            ci: Some(CiSnapshot {
                status: CiStatus::Failed,
                checks: vec![CiCheck {
                    name: "Integration suite".into(),
                    status: CiStatus::Failed,
                    url: Some("https://github.com/acme/engine/actions/runs/9".into()),
                    duration_seconds: Some(123),
                }],
                change_request: Some(ChangeRequest {
                    number: 87,
                    title: "Queue fairness".into(),
                    description: "Eliminate starvation in the dispatcher".into(),
                    url: "https://github.com/acme/engine/pull/87".into(),
                    state: ChangeRequestState::Open,
                    issues: vec![LinkedIssue {
                        provider: IssueTracker::Linear,
                        identifier: "LIN-42".into(),
                        title: Some("Prevent worker lockout".into()),
                        url: "https://linear.app/acme/issue/LIN-42".into(),
                    }],
                    base: None,
                }),
                fetch_state: CiFetchState::Ready,
                updated_at: 1,
                stale: false,
                error: None,
                repository: ForgeRepository {
                    provider: ForgeProvider::Github,
                    host: "github.com".into(),
                    path: "acme/engine".into(),
                    web_url: "https://github.com/acme/engine".into(),
                },
                head_sha: "deadbeef".into(),
                branch: Some("feature/queues".into()),
                truncated: false,
                commit_missing: false,
            }),
        };
        let other = Pane {
            id: "pane-2".into(),
            terminal_id: "terminal-2".into(),
            workspace_id: "docs".into(),
            name: "Notebook".into(),
            description: "Café 東京".into(),
            harness: None,
            agent_id: None,
            status: Status::Blocked,
            attention: None,
            cwd: None,
            branch: None,
            branch_changes: None,
            project: None,
            ci: None,
            ..pane.clone()
        };
        Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
            instance_id: "test".into(),
            revision: 1,
            session: "test".into(),
            connected: true,
            error: None,
            focused_pane_id: None,
            workspaces: vec![
                Workspace {
                    id: "engine".into(),
                    name: "Engine Room".into(),
                    active_tab_id: "tab-1".into(),
                    projects: vec![],
                    status: Status::Working,
                    attention: None,
                },
                Workspace {
                    id: "docs".into(),
                    name: "Documentation".into(),
                    active_tab_id: "tab-2".into(),
                    projects: vec![],
                    status: Status::Unknown,
                    attention: None,
                },
            ],
            panes: vec![pane, other],
        }
    }

    fn panes(index: &mut Index, query: &str) -> Vec<usize> {
        index
            .search(query)
            .iter()
            .map(|matched| matched.pane)
            .collect()
    }

    #[test]
    fn full_text_covers_name_activity_and_pull_request_body() {
        let mut index = Index::default();
        index.rebuild(&snapshot());
        for query in [
            "cmplr",
            "scheduler",
            "starvation",
            "dispatcher",
            "lockout",
            "Integration",
            "acme/engine",
            "+321",
        ] {
            assert_eq!(panes(&mut index, query), [0], "{query}");
        }
    }

    #[test]
    fn fuzzy_terms_preserve_metadata_value_boundaries_and_multiline_values() {
        let atom = |value| {
            Atom::new(
                value,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            )
        };
        let mut matcher = Matcher::default();
        let mut text = Text::default();
        text.add("compiler");
        text.add("Notebook");
        text.prepare();
        assert!(text.score(&atom("cmplr"), &mut matcher).is_some());
        assert!(
            text.score(&atom("compilerNotebook"), &mut matcher)
                .is_none()
        );

        text.clear();
        text.add("Café");
        text.add("東京");
        text.prepare();
        assert!(text.score(&atom("CAFÉ"), &mut matcher).is_some());
        assert!(text.score(&atom("東京"), &mut matcher).is_some());
        assert!(text.score(&atom("é東"), &mut matcher).is_none());

        text.clear();
        text.add("Café\n東京");
        text.prepare();
        assert!(text.score(&atom("é東"), &mut matcher).is_some());

        text.clear();
        text.add("Notebook");
        text.prepare();
        assert!(text.score(&atom("ntbk"), &mut matcher).is_some());
        assert!(text.score(&atom("CAFÉ"), &mut matcher).is_none());
    }

    #[test]
    fn filters_search_only_their_own_metadata() {
        let mut index = Index::default();
        index.rebuild(&snapshot());
        for query in [
            "pr:starvation",
            "pr:#87",
            "pr:open",
            "branch:origin/main",
            "project:acme/engine",
            "workspace:Room",
            "harness:Claude",
            "status:question",
            "ci:Integration",
            "ci:failed",
            "cwd:engine:cache",
            "ticket:LIN-42",
            "ticket:lockout",
        ] {
            assert_eq!(panes(&mut index, query), [0], "{query}");
        }
        for query in [
            "branch:starvation",
            "harness:Integration",
            "ci:lockout",
            "ticket:dispatcher",
            "status:failed",
            "project:starvation",
        ] {
            assert!(panes(&mut index, query).is_empty(), "{query}");
        }
    }

    #[test]
    fn quoted_unquoted_and_unfinished_values_are_usable_while_typing() {
        let mut index = Index::default();
        index.rebuild(&snapshot());
        for query in [
            "pr:Queue fairness",
            "pr:\"Queue fairness\"",
            "pr:'Queue fairness'",
            "pr:\"Queue fairness",
            "pr:Queue\\ fairness",
            "\"Queue fairness\"",
        ] {
            assert_eq!(panes(&mut index, query), [0], "{query}");
        }
        assert_eq!(panes(&mut index, "pr:Queue scheduler"), [0]);
        assert!(panes(&mut index, "pr:\"Queue scheduler\"").is_empty());
    }

    #[test]
    fn escaped_quotes_and_trailing_escape_do_not_break_live_queries() {
        let mut state = snapshot();
        state.panes[0].name = "Say \"hello\" in C:\\work\\".into();
        let mut index = Index::default();
        index.rebuild(&state);
        assert_eq!(panes(&mut index, "\"Say \\\"hello\\\"\""), [0]);
        assert_eq!(panes(&mut index, "C:\\work\\"), [0]);
    }

    #[test]
    fn terms_and_filters_combine_with_and() {
        let mut index = Index::default();
        index.rebuild(&snapshot());
        assert_eq!(
            panes(
                &mut index,
                "workspace:\"Engine Room\" harness:claude pr:87 ci:failed ticket:LIN-42 starvation"
            ),
            [0]
        );
        assert!(panes(&mut index, "harness:claude workspace:Documentation").is_empty());
        assert!(panes(&mut index, "compiler Notebook").is_empty());
    }

    #[test]
    fn empty_filters_require_present_nonempty_fields() {
        let mut index = Index::default();
        let mut state = snapshot();
        state.panes[1].cwd = Some("   ".into());
        index.rebuild(&state);
        for query in [
            "pr:", "branch:", "project:", "harness:", "status:", "ci:", "cwd:", "ticket:",
            "pr:\"\"",
        ] {
            assert_eq!(panes(&mut index, query), [0], "{query}");
        }
        assert_eq!(panes(&mut index, "workspace:"), [0, 1]);
    }

    #[test]
    fn generic_program_status_is_not_a_harness_status() {
        let mut state = snapshot();
        state.panes[1].harness = Some("ordinary-application".into());
        let mut index = Index::default();
        index.rebuild(&state);
        assert!(panes(&mut index, "status:blocked").is_empty());
        assert!(panes(&mut index, "harness:ordinary").is_empty());
        assert_eq!(panes(&mut index, "status:working"), [0]);
    }

    #[test]
    fn unknown_prefixes_and_urls_remain_literal_search_text() {
        let mut state = snapshot();
        state.panes[1].name = "proto:quasar".into();
        let mut index = Index::default();
        index.rebuild(&state);
        assert_eq!(panes(&mut index, "proto:quasar"), [1]);
        assert_eq!(
            panes(&mut index, "https://github.com/acme/engine/pull/87"),
            [0]
        );
        assert!(panes(&mut index, "unknown:").is_empty());
    }

    #[test]
    fn matching_is_case_insensitive_and_unicode_friendly() {
        let mut index = Index::default();
        index.rebuild(&snapshot());
        assert_eq!(panes(&mut index, "CAFÉ"), [1]);
        assert_eq!(panes(&mut index, "東京"), [1]);
        assert_eq!(panes(&mut index, "PR:starvation"), [0]);
        assert_eq!(panes(&mut index, "pr:STARVATION"), [0]);
    }

    #[test]
    fn refresh_removes_old_documents_and_reuses_the_current_query() {
        let mut state = snapshot();
        let mut index = Index::default();
        index.rebuild(&state);
        assert_eq!(panes(&mut index, "Notebook"), [1]);
        state.panes.remove(0);
        index.rebuild(&state);
        assert!(index.results().is_empty());
        assert_eq!(panes(&mut index, "Notebook"), [0]);
        state.panes.clear();
        index.rebuild(&state);
        assert!(panes(&mut index, "Notebook").is_empty());
        assert!(panes(&mut index, "").is_empty());
    }

    #[test]
    fn updated_metadata_invalidates_a_cached_match() {
        let mut state = snapshot();
        let mut index = Index::default();
        index.rebuild(&state);
        assert_eq!(panes(&mut index, "pr:starvation"), [0]);
        state.panes[0]
            .ci
            .as_mut()
            .unwrap()
            .change_request
            .as_mut()
            .unwrap()
            .description
            .clear();
        index.rebuild(&state);
        assert!(panes(&mut index, "pr:starvation").is_empty());
    }

    #[test]
    fn ranking_uses_fuzzy_scores_and_snapshot_order_for_ties() {
        let mut state = snapshot();
        state.panes[0].name = "abc".into();
        state.panes[0].description.clear();
        state.panes[0].ci = None;
        state.panes[0].branch = None;
        state.panes[0].branch_changes = None;
        let mut same = state.panes[0].clone();
        same.id = "pane-3".into();
        state.panes[1] = same;
        let mut spaced = state.panes[0].clone();
        spaced.id = "pane-4".into();
        spaced.name = "a-------b-------c".into();
        state.panes.push(spaced);
        let mut index = Index::default();
        index.rebuild(&state);
        assert_eq!(panes(&mut index, "abc"), [0, 1, 2]);
        assert_eq!(index.results()[0].score, index.results()[1].score);
        assert!(index.results()[1].score > index.results()[2].score);
        assert_eq!(panes(&mut index, ""), [0, 1, 2]);
        assert_eq!(panes(&mut index, "abc"), [0, 1, 2]);
    }

    #[test]
    fn annotations_keep_filtered_pr_and_ticket_matches_in_their_own_values() {
        let mut state = snapshot();
        state.panes[0].name = "starvation LIN-42".into();
        let mut index = Index::default();
        index.rebuild(&state);
        for (query, field, expected) in [
            (
                "pr:starvation",
                Field::Pr,
                "Eliminate starvation in the dispatcher",
            ),
            ("ticket:lockout", Field::Ticket, "Prevent worker lockout"),
            ("ticket:LIN42", Field::Ticket, "LIN-42"),
        ] {
            assert_eq!(panes(&mut index, query), [0]);
            let ranked = index.results().to_vec();
            let name = index.value(0, Field::All, &state.panes[0].name).unwrap();
            let visible = [name];
            let annotations = index.annotate(0, &visible);
            assert!(
                (0..annotations.len(name)).all(|position| !annotations.matched(name, position))
            );
            let context = annotations.contexts(&visible).next().unwrap();
            assert_eq!(context.field, field);
            let context_text: String = (0..annotations.len(context))
                .map(|position| annotations.glyph(context, position))
                .collect();
            assert_eq!(context_text, expected);
            assert!(annotations.match_bounds(context).is_some());
            assert_eq!(index.results(), ranked);
        }
    }

    #[test]
    fn visible_annotations_use_discontiguous_nucleo_alignment_and_presence_has_none() {
        let mut index = Index::default();
        let state = snapshot();
        index.rebuild(&state);
        let name = index.value(0, Field::All, "compiler").unwrap();
        index.search("cmplr");
        let visible = [name];
        let annotations = index.annotate(0, &visible);
        let matched: String = (0..annotations.len(name))
            .filter(|position| annotations.matched(name, *position))
            .map(|position| annotations.glyph(name, position))
            .collect();
        assert_eq!(matched, "cmplr");
        assert!(annotations.contexts(&visible).next().is_none());
        for query in ["", "pr:", "ticket:"] {
            index.search(query);
            let annotations = index.annotate(0, &visible);
            assert!(annotations.match_bounds(name).is_none());
            assert!(annotations.contexts(&visible).next().is_none());
        }
    }

    #[test]
    fn annotation_offsets_preserve_complete_unicode_graphemes() {
        let mut state = snapshot();
        state.panes[0].name = "Cafe\u{301} 👩\u{200d}💻 東京".into();
        let mut index = Index::default();
        index.rebuild(&state);
        index.search("cafe");
        let name = index.value(0, Field::All, &state.panes[0].name).unwrap();
        let visible = [name];
        let annotations = index.annotate(0, &visible);
        assert_eq!(annotations.glyph(name, 3), "e\u{301}");
        assert_eq!(annotations.glyph(name, 5), "👩\u{200d}💻");
        assert!(annotations.matched(name, 3));
        assert!(!annotations.matched(name, 5));
    }
}
