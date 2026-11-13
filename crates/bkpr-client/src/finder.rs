//! Metadata-only pane finder. No terminal contents, transport, or backend access.
//!
//! Query tokenization and nucleo matching adapted from bkpr-zellij's
//! pane_search.rs (Copyright (c) 2026 Rémi Labeyrie, MIT). The repository's
//! MIT license notice applies. Projection, identity, and retained painting here
//! use only current supplied protocol facts, including forge, CI and tickets.

use bkpr_protocol::{
    Cursor, CursorShape, KeyAction, KeyEvent, PaneSnapshot, PaneStatus, RegistrySnapshot, Size,
};
use bkpr_protocol::{MouseAction, MouseButton, MouseEvent};
use bkpr_tui_framework::Rect;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

// Presentation follows main 9ad93bd's finder.rs/finder_entry.rs and pane tooltip
// grouping (Copyright (c) 2026 Rémi Labeyrie, MIT); no legacy transport is reused.
mod entry;
use crate::metadata::{self, Paragraph, PreviewLine, ProjectSpan};
use crate::project_colors::{ProjectPalette, ProjectSlots};
use bkpr_tui_framework::{
    BackgroundSpan, ColorProbe, ForegroundSpan, Role, RoleSpan, TextInput, Theme, UiSurface,
};

/// Execution membership, not a guess from another workspace's projected layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinderTarget {
    pub workspace_id: Option<String>,
    pub pane_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinderIntent {
    None,
    Cancel,
    Activate(FinderTarget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Text,
    Workspace,
    Window,
    Harness,
    Status,
    Cwd,
    Project,
    Branch,
}
impl Field {
    fn named(name: &str) -> Option<Self> {
        [
            ("workspace", Self::Workspace),
            ("harness", Self::Harness),
            ("window", Self::Window),
            ("status", Self::Status),
            ("cwd", Self::Cwd),
            ("project", Self::Project),
            ("branch", Self::Branch),
        ]
        .into_iter()
        .find_map(|(label, field)| name.eq_ignore_ascii_case(label).then_some(field))
    }
    fn label(self) -> &'static str {
        match self {
            Self::Text => "pane",
            Self::Workspace => "workspace",
            Self::Window => "window",
            Self::Harness => "harness",
            Self::Status => "status",
            Self::Cwd => "cwd",
            Self::Project => "project",
            Self::Branch => "branch",
        }
    }
}

struct Value {
    field: Field,
    text: String,
    stale: bool,
    // One scalar per grapheme matches nucleo's grapheme-indexed input model.
    chars: Vec<char>,
}
impl Value {
    fn new(field: Field, text: &str) -> Self {
        // Published metadata is untrusted terminal text, not escape sequences.
        let text = metadata::clean(text);
        let chars = text
            .graphemes(true)
            .map(|g| g.chars().next().unwrap())
            .collect();
        Self {
            field,
            text,
            chars,
            stale: false,
        }
    }
    fn label(&self) -> &'static str {
        match (self.field, self.stale) {
            (Field::Project, true) => "project (stale)",
            (Field::Branch, true) => "branch (stale)",
            _ => self.field.label(),
        }
    }
    fn view(&self) -> Utf32Str<'_> {
        Utf32Str::Unicode(&self.chars)
    }
}
struct Document {
    target: FinderTarget,
    values: Vec<Value>,
    harness_visible: bool,
    status_visible: bool,
    terminal_error: Option<String>,
    project_stale: bool,
    project_slot: Option<u32>,
    preview: Vec<Paragraph>,
}
impl Document {
    fn add(&mut self, field: Field, text: &str) {
        if !text.trim().is_empty() {
            let mut value = Value::new(field, text);
            value.stale = self.project_stale && matches!(field, Field::Project | Field::Branch);
            self.values.push(value);
        }
    }
}
struct Term {
    field: Field,
    atom: Option<Atom>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinderResult {
    document: usize,
    pub score: u32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum CompactView {
    #[default]
    Input,
    List,
    Preview,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FinderLayout {
    title: Rect,
    input: Rect,
    list: Rect,
    preview: Rect,
    search_rule: Option<Rect>,
    body_rule: Option<Rect>,
    footer: Option<Rect>,
}
impl FinderLayout {
    fn raised(size: Size) -> Option<(Self, [(Rect, Rect); 3])> {
        if size.cols < 40 || size.rows < 14 {
            return None;
        }
        let search = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: 4,
        };
        let body = size.rows - 4;
        let (list, preview) = if size.cols >= 84 {
            let split = (u32::from(size.cols) * 2 / 5) as u16;
            (
                Rect {
                    x: 0,
                    y: 4,
                    width: split,
                    height: body,
                },
                Rect {
                    x: split,
                    y: 4,
                    width: size.cols - split,
                    height: body,
                },
            )
        } else {
            let split = ((u32::from(body) * 2 / 5) as u16).max(4);
            (
                Rect {
                    x: 0,
                    y: 4,
                    width: size.cols,
                    height: split,
                },
                Rect {
                    x: 0,
                    y: 4 + split,
                    width: size.cols,
                    height: body - split,
                },
            )
        };
        let inset = |rect: Rect| Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width - 4,
            height: rect.height - 2,
        };
        let title = Rect {
            x: 2,
            y: 1,
            width: size.cols - 4,
            height: 1,
        };
        let layout = Self {
            title,
            input: Rect { y: 2, ..title },
            list: inset(list),
            preview: inset(preview),
            ..Self::default()
        };
        Some((
            layout,
            [
                (search, Rect { height: 2, ..title }),
                (list, layout.list),
                (preview, layout.preview),
            ],
        ))
    }

    // The parent owns the rounded outer border; these are its inner cells.
    fn new(size: Size, compact: CompactView, graphical: bool) -> Self {
        if graphical && let Some((layout, _)) = Self::raised(size) {
            return layout;
        }
        let row = |y| Rect {
            x: 0,
            y,
            width: size.cols,
            height: 1,
        };
        if size.cols == 0 || size.rows == 0 {
            return Self::default();
        }
        if size.rows <= 2 {
            let mut layout = Self::default();
            if size.rows == 2 || compact == CompactView::Input {
                layout.input = row(0);
            }
            let body = row(size.rows - 1);
            match compact {
                CompactView::Preview => layout.preview = body,
                _ if size.rows == 2 || compact == CompactView::List => layout.list = body,
                _ => {}
            }
            return layout;
        }
        let mut layout = Self {
            title: if size.rows >= 5 {
                row(0)
            } else {
                Rect::default()
            },
            input: row(u16::from(size.rows >= 5)),
            ..Self::default()
        };
        let top = layout.input.y + 1;
        let rule = u16::from(size.rows >= 6);
        if rule != 0 {
            layout.search_rule = Some(row(top));
        }
        let footer = u16::from(size.rows >= 5);
        if footer != 0 {
            layout.footer = Some(row(size.rows - 1));
        }
        let body = Rect {
            x: 0,
            y: top + rule,
            width: size.cols,
            height: size.rows - top - rule - footer,
        };
        layout.list = body;
        if size.cols >= 84 && body.height >= 2 {
            let split = (u32::from(size.cols) * 2 / 5) as u16;
            layout.list.width = split;
            layout.body_rule = Some(Rect {
                x: split,
                width: 1,
                ..body
            });
            layout.preview = Rect {
                x: split + 1,
                width: size.cols - split - 1,
                ..body
            };
        } else if body.height >= 12 {
            let split = ((u32::from(body.height) * 2 / 5) as u16).max(3);
            layout.list.height = split;
            layout.body_rule = Some(Rect {
                y: body.y + split,
                height: 1,
                ..body
            });
            layout.preview = Rect {
                y: body.y + split + 1,
                height: body.height - split - 1,
                ..body
            };
        } else if compact == CompactView::Preview {
            layout.preview = body;
            layout.list = Rect::default();
        }
        layout
    }
    fn count(self) -> usize {
        usize::from(self.list.height)
    }
}
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}
fn preview_capacity(rect: Rect) -> usize {
    usize::from(rect.height)
}

/// Owns the bounded query, metadata index, ranked results, and stable selection.
/// Call `update_registry` for each authoritative registry publication; revision
/// changes invalidate the index. Query edits reset selection, metadata edits do not.
pub struct Finder {
    revision: Option<u64>,
    documents: Vec<Document>,
    matcher: Matcher,
    input: TextInput,
    terms: Vec<Term>,
    unsupported: Vec<String>,
    results: Vec<FinderResult>,
    selected: usize,
    scroll: usize,
    positions: Vec<u32>,
    preview_scroll: usize,
    compact: CompactView,
    hovered: Option<String>,
    last_click: Option<(String, Instant)>,
    preview_lines: Vec<PreviewLine>,
    preview_key: Option<(u64, String, u16)>,
    graphical: bool,
    roles: Vec<RoleSpan>,
    projects: Vec<ProjectSpan>,
    foregrounds: Vec<ForegroundSpan>,
    match_cells: Vec<(u16, u16)>,
}
impl Finder {
    pub fn new<'a>(
        registry: &RegistrySnapshot,
        initial_pane: Option<&str>,
        projects: &ProjectSlots,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    ) -> Self {
        let mut finder = Self {
            revision: None,
            documents: Vec::new(),
            matcher: Matcher::default(),
            input: TextInput::new(String::new(), crate::TEXT_INPUT_LIMIT),
            terms: Vec::new(),
            unsupported: Vec::new(),
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            positions: Vec::new(),
            preview_scroll: 0,
            compact: CompactView::default(),
            hovered: None,
            last_click: None,
            preview_lines: Vec::new(),
            preview_key: None,
            graphical: false,
            roles: Vec::new(),
            projects: Vec::new(),
            foregrounds: Vec::new(),
            match_cells: Vec::new(),
        };
        finder.update_registry(registry, projects, snapshot);
        if let Some(index) = finder.results.iter().position(|result| {
            Some(finder.documents[result.document].target.pane_id.as_str()) == initial_pane
        }) {
            finder.selected = index;
        }
        finder
    }
    /// Candidate regions are independent of artwork readiness. Every coordinate
    /// is local to the complete overlay, including the opaque text insets.
    pub(crate) fn graphic_regions(size: Size) -> Option<[(Rect, Rect); 3]> {
        FinderLayout::raised(size).map(|(_, regions)| regions)
    }
    pub(crate) fn set_graphical(&mut self, graphical: bool) {
        self.graphical = graphical;
    }
    pub(crate) fn is_graphical(&self) -> bool {
        self.graphical
    }
    fn layout(&self, size: Size) -> FinderLayout {
        FinderLayout::new(size, self.compact, self.graphical)
    }

    pub fn target(&self, result: &FinderResult) -> Option<&FinderTarget> {
        self.documents
            .get(result.document)
            .map(|document| &document.target)
    }
    pub fn selected_target(&self) -> Option<&FinderTarget> {
        self.target(self.results.get(self.selected)?)
    }

    pub fn update_registry<'a>(
        &mut self,
        registry: &RegistrySnapshot,
        projects: &ProjectSlots,
        snapshot: impl Fn(&str) -> Option<&'a PaneSnapshot>,
    ) {
        if self.revision == Some(registry.revision)
            && self.documents.len() == registry.panes.len()
            && self
                .documents
                .iter()
                .zip(&registry.panes)
                .all(|(document, pane)| {
                    let known = snapshot(&pane.id);
                    document.project_slot
                        == pane
                            .project
                            .as_ref()
                            .and_then(|project| projects.slot(project))
                        && document.harness_visible
                            == crate::controller::recognized_harness(pane, known).is_some()
                        && document.status_visible
                            == crate::controller::harness_status(pane, known).is_some()
                        && document.terminal_error.as_deref()
                            == known
                                .filter(|snapshot| snapshot.pane_id == pane.id)
                                .and_then(|snapshot| snapshot.error.as_deref())
                })
        {
            return;
        }
        let previous = self.selected_target().map(|target| target.pane_id.clone());
        self.documents.clear();
        self.preview_key = None;
        self.preview_scroll = 0;
        for pane in &registry.panes {
            let known = snapshot(&pane.id);
            let harness = crate::controller::recognized_harness(pane, known);
            let status = crate::controller::harness_status(pane, known);
            let mut document = Document {
                target: FinderTarget {
                    workspace_id: pane.workspace_id.clone(),
                    pane_id: pane.id.clone(),
                },
                values: Vec::new(),
                harness_visible: harness.is_some(),
                status_visible: status.is_some(),
                terminal_error: known
                    .filter(|snapshot| snapshot.pane_id == pane.id)
                    .and_then(|snapshot| snapshot.error.clone()),
                project_stale: pane
                    .project
                    .as_ref()
                    .is_some_and(|project| project.state != bkpr_protocol::ProjectState::Ready),
                project_slot: pane
                    .project
                    .as_ref()
                    .and_then(|project| projects.slot(project)),
                preview: metadata::content(
                    pane,
                    registry,
                    known,
                    false,
                    pane.project
                        .as_ref()
                        .and_then(|project| projects.slot(project)),
                ),
            };
            // First value is always the title, including for an unnamed pane.
            let title = metadata::pane_title(pane, known);
            document.values.push(Value::new(
                Field::Text,
                if title.is_empty() {
                    "Untitled terminal"
                } else {
                    title
                },
            ));
            document.add(Field::Text, &pane.id);
            if let Some(workspace) = pane.workspace_id.as_ref().and_then(|id| {
                registry
                    .workspaces
                    .iter()
                    .find(|workspace| &workspace.id == id)
            }) {
                document.add(Field::Workspace, &workspace.name);
                document.add(Field::Workspace, &workspace.id);
                if let Some(window) = workspace
                    .windows
                    .iter()
                    .find(|window| Some(&window.id) == pane.window_id.as_ref())
                {
                    document.add(Field::Window, &window.name);
                    document.add(Field::Window, &window.id);
                }
            }
            document.add(
                Field::Status,
                match pane.status {
                    PaneStatus::Running => "running",
                    PaneStatus::Stopped => "stopped",
                    PaneStatus::Exited => "exited",
                },
            );
            if let Some(status) = status {
                document.add(Field::Status, &status.state);
            }
            if let Some(harness) = harness {
                document.add(Field::Harness, harness.kind.as_str());
            }
            document.add(
                Field::Cwd,
                &pane
                    .foreground_cwd
                    .as_ref()
                    .unwrap_or(&pane.cwd)
                    .to_string_lossy(),
            );
            if let Some(repository) = pane
                .project
                .as_ref()
                .and_then(|project| project.repository.as_ref())
            {
                document.add(Field::Project, &repository.name);
                document.add(Field::Project, &repository.root.to_string_lossy());
                if let Some(branch) = &repository.branch {
                    document.add(Field::Branch, branch);
                }
            }
            self.documents.push(document);
        }
        self.revision = Some(registry.revision);
        self.rank();
        if let Some(index) = previous.and_then(|id| {
            self.results
                .iter()
                .position(|result| self.documents[result.document].target.pane_id == id)
        }) {
            self.selected = index;
        } else {
            self.selected = self.selected.min(self.results.len().saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.results.len().saturating_sub(1));
    }

    #[cfg(test)]
    fn set_query(&mut self, query: &str) {
        self.input.edit(bkpr_tui_framework::EditAction::Clear);
        self.insert(query);
    }

    /// Inserts printable text atomically. Oversize input preserves the existing
    /// query/cursor and exposes a visible error, never a truncated fuzzy query.
    pub fn paste(&mut self, text: &str) {
        self.insert(text);
    }

    fn insert(&mut self, text: &str) -> bool {
        let changed = self.input.insert(text);
        if changed {
            self.changed();
        }
        changed
    }
    fn changed(&mut self) {
        self.parse();
        self.rank();
        self.selected = 0;
        self.scroll = 0;
        self.preview_scroll = 0;
        self.compact = CompactView::Input;
        self.hovered = None;
        self.last_click = None;
    }
    fn parse(&mut self) {
        self.terms.clear();
        self.unsupported.clear();
        let mut token = String::new();
        let mut quote = None;
        let mut chars = self.input.value.chars().peekable();
        while let Some(character) = chars.next() {
            match character {
                '\\' if chars.peek().is_some_and(|next| {
                    next.is_whitespace() || matches!(next, '\\' | '\'' | '"')
                }) =>
                {
                    token.push(chars.next().unwrap())
                }
                '\'' | '"' if quote == Some(character) => quote = None,
                '\'' | '"' if quote.is_none() && (token.is_empty() || token.ends_with(':')) => {
                    quote = Some(character)
                }
                c if c.is_whitespace() && quote.is_none() => {
                    Self::finish_term(&mut token, &mut self.terms, &mut self.unsupported)
                }
                c => token.push(c),
            }
        }
        Self::finish_term(&mut token, &mut self.terms, &mut self.unsupported);
    }
    fn finish_term(token: &mut String, terms: &mut Vec<Term>, unsupported: &mut Vec<String>) {
        if token.is_empty() {
            return;
        }
        if let Some((name, _)) = token.split_once(':')
            && ["pr", "ci", "ticket"]
                .iter()
                .any(|field| name.eq_ignore_ascii_case(field))
        {
            let name = name.to_ascii_lowercase();
            if !unsupported.contains(&name) {
                unsupported.push(name);
            }
            token.clear();
            return;
        }
        // Unknown prefixes retain literal meaning (e.g. Windows paths).
        let (field, value) = token
            .split_once(':')
            .and_then(|(name, value)| Field::named(name).map(|field| (field, value)))
            .unwrap_or((Field::Text, token.as_str()));
        terms.push(Term {
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
        token.clear();
    }
    fn rank(&mut self) {
        self.results.clear();
        if !self.unsupported.is_empty() {
            return;
        }
        'candidate: for (index, document) in self.documents.iter().enumerate() {
            let mut score = 0_u32;
            for term in &self.terms {
                let values = document
                    .values
                    .iter()
                    .filter(|value| term.field == Field::Text || value.field == term.field);
                if let Some(atom) = &term.atom {
                    let Some(best) = values
                        .filter_map(|value| atom.score(value.view(), &mut self.matcher))
                        .max()
                    else {
                        continue 'candidate;
                    };
                    score = score.saturating_add(u32::from(best));
                } else if !values.into_iter().any(|value| !value.text.is_empty()) {
                    continue 'candidate;
                }
            }
            self.results.push(FinderResult {
                document: index,
                score,
            });
        }
        self.results.sort_unstable_by(|left, right| {
            right.score.cmp(&left.score).then_with(|| {
                self.documents[left.document]
                    .target
                    .pane_id
                    .cmp(&self.documents[right.document].target.pane_id)
            })
        });
    }
    fn visible_count(&self, size: Size) -> usize {
        self.layout(size).count()
    }
    fn reveal(&mut self, count: usize) {
        let count = count.max(1);
        self.scroll = self.scroll.min(self.results.len().saturating_sub(count));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + count {
            self.scroll = self.selected + 1 - count;
        }
    }
    pub fn navigate(&mut self, delta: isize, size: Size) {
        let previous = self.selected;
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.results.len().saturating_sub(1));
        if self.selected != previous {
            self.preview_scroll = 0;
        }
        if self.layout(size).list.height == 0 {
            self.compact = CompactView::List;
        }
        self.hovered = None;
        self.last_click = None;
        self.reveal(self.visible_count(size));
    }

    fn prepare_preview(&mut self, width: u16) {
        let Some(result) = self.results.get(self.selected) else {
            self.preview_lines.clear();
            self.preview_key = None;
            self.preview_scroll = 0;
            return;
        };
        let document = &self.documents[result.document];
        let revision = self.revision.unwrap_or_default();
        if self
            .preview_key
            .as_ref()
            .is_some_and(|(old_revision, pane, old_width)| {
                *old_revision == revision && pane == &document.target.pane_id && *old_width == width
            })
        {
            return;
        }
        self.preview_lines.clear();
        metadata::wrap(&document.preview, width, &mut self.preview_lines);
        self.preview_key = Some((revision, document.target.pane_id.clone(), width));
    }
    fn scroll_preview(&mut self, delta: isize, size: Size) {
        self.compact = CompactView::Preview;
        let rect = self.layout(size).preview;
        self.prepare_preview(rect.width);
        self.preview_scroll = self.preview_scroll.saturating_add_signed(delta).min(
            self.preview_lines
                .len()
                .saturating_sub(preview_capacity(rect)),
        );
    }

    /// Protocol mouse events with x/y converted by the parent to LOCAL CELL
    /// coordinates, not pixels. Motion highlights a result; one left click
    /// selects, a second on the same stable pane within 350ms activates. Wheel
    /// routing uses the actual pointer rectangle, never stale hover state.
    pub fn handle_mouse(&mut self, event: &MouseEvent, size: Size) -> FinderIntent {
        if !event.x.is_finite()
            || !event.y.is_finite()
            || event.x < 0.0
            || event.y < 0.0
            || event.x >= f32::from(size.cols)
            || event.y >= f32::from(size.rows)
        {
            self.hovered = None;
            return FinderIntent::None;
        }
        let (x, y) = (event.x as u16, event.y as u16);
        let layout = self.layout(size);
        let in_list = contains(layout.list, x, y);
        let in_preview = contains(layout.preview, x, y);
        let hit = in_list
            .then(|| self.scroll + usize::from(y - layout.list.y))
            .filter(|index| *index < self.results.len());
        self.hovered = hit.map(|index| {
            self.documents[self.results[index].document]
                .target
                .pane_id
                .clone()
        });
        if event.action == MouseAction::Press {
            let wheel = match event.button {
                MouseButton::WheelUp => -3,
                MouseButton::WheelDown => 3,
                _ => 0,
            };
            if wheel != 0 {
                if in_preview {
                    self.scroll_preview(wheel, size);
                } else if in_list {
                    self.navigate(wheel, size);
                }
                return FinderIntent::None;
            }
            if event.button == MouseButton::Left {
                if in_preview {
                    self.compact = CompactView::Preview;
                } else if in_list {
                    self.compact = CompactView::List;
                } else if contains(layout.input, x, y) {
                    self.compact = CompactView::Input;
                }
                if let Some(index) = hit {
                    if self.selected != index {
                        self.preview_scroll = 0;
                    }
                    self.selected = index;
                    let target = self.selected_target().unwrap().clone();
                    let now = Instant::now();
                    let activate = self.last_click.as_ref().is_some_and(|(pane, time)| {
                        pane == &target.pane_id
                            && now.duration_since(*time) <= Duration::from_millis(350)
                    });
                    self.last_click = Some((target.pane_id.clone(), now));
                    if activate {
                        self.last_click = None;
                        return FinderIntent::Activate(target);
                    }
                } else {
                    self.last_click = None;
                }
            }
        }
        FinderIntent::None
    }
    /// All keys are consumed by the focused finder, including ignored releases.
    pub fn handle_key(&mut self, event: &KeyEvent, size: Size) -> FinderIntent {
        if event.action == KeyAction::Release {
            return FinderIntent::None;
        }
        use crate::keyguide::{Action, Context, Navigation};
        let action = crate::keyguide::lookup(Context::Finder, event);
        if matches!(action, Some(Action::Edit(_))) {
            self.compact = CompactView::Input;
        }
        match action {
            Some(Action::Cancel) => return FinderIntent::Cancel,
            Some(Action::Submit) => {
                return self
                    .selected_target()
                    .cloned()
                    .map_or(FinderIntent::None, FinderIntent::Activate);
            }
            Some(Action::Navigate(Navigation::Previous)) => self.navigate(-1, size),
            Some(Action::Navigate(Navigation::Next)) => self.navigate(1, size),
            Some(Action::Navigate(Navigation::PagePrevious)) => {
                let page = preview_capacity(
                    FinderLayout::new(size, CompactView::Preview, self.graphical).preview,
                )
                .max(1);
                self.scroll_preview(-(page as isize), size);
            }
            Some(Action::Navigate(Navigation::PageNext)) => {
                let page = preview_capacity(
                    FinderLayout::new(size, CompactView::Preview, self.graphical).preview,
                )
                .max(1);
                self.scroll_preview(page as isize, size);
            }
            Some(Action::CycleView) => {
                self.compact = match self.compact {
                    CompactView::Input => CompactView::List,
                    CompactView::List => CompactView::Preview,
                    CompactView::Preview => CompactView::Input,
                };
            }
            Some(Action::Edit(edit)) => {
                if self.input.edit(edit) {
                    self.changed();
                }
            }
            None if event.modifiers & 14 == 0 => {
                self.insert(&event.text);
            }
            _ => {}
        }
        FinderIntent::None
    }

    /// Paint the supplied presentation size: full overlay for the three-card
    /// graphical view, or the parent's inner plain-popup rectangle otherwise.
    pub fn paint(&mut self, surface: &mut UiSurface, size: Size) {
        let layout = self.layout(size);
        self.reveal(layout.count());
        let preview_count = preview_capacity(layout.preview);
        if layout.preview.width > 0 && preview_count > 0 {
            self.prepare_preview(layout.preview.width);
            self.preview_scroll = self
                .preview_scroll
                .min(self.preview_lines.len().saturating_sub(preview_count));
        }
        // Restore only our previous fuzzy emphasis. Surface owns retained text,
        // and semantic title/heading styles are reapplied through RoleSpan.
        for (x, y) in self.match_cells.drain(..) {
            if let Some(cell) = surface
                .frame
                .rows
                .get_mut(usize::from(y))
                .and_then(|row| row.cells.get_mut(usize::from(x)))
            {
                cell.style.bold = false;
                cell.style.underline = 0;
            }
        }
        self.roles.clear();
        self.projects.clear();
        let mut lines = Vec::new();
        let title = self.input.error().map_or_else(
            || {
                format!(
                    "Pane finder · {}/{}",
                    self.results.len(),
                    self.documents.len()
                )
            },
            str::to_owned,
        );
        role_line(
            &mut lines,
            &mut self.roles,
            layout.title,
            0,
            title,
            if self.input.error().is_some() {
                Role::Error
            } else {
                Role::Title
            },
            false,
        );
        let prefix = if layout.input.width >= 3 { "> " } else { "" };
        let (visible, used) = self
            .input
            .visible(usize::from(layout.input.width).saturating_sub(prefix.len()));
        role_line(
            &mut lines,
            &mut self.roles,
            layout.input,
            0,
            format!("{prefix}{visible}"),
            Role::Accent,
            false,
        );
        if let Some(rule) = layout.search_rule {
            role_line(
                &mut lines,
                &mut self.roles,
                rule,
                0,
                format!(
                    "─ Results · {} {}",
                    self.results.len(),
                    "─".repeat(usize::from(rule.width))
                ),
                Role::Border,
                false,
            );
        }
        if let Some(rule) = layout.body_rule {
            let vertical = rule.width == 1;
            for row in 0..rule.height {
                role_line(
                    &mut lines,
                    &mut self.roles,
                    rule,
                    row,
                    if vertical {
                        "│".into()
                    } else {
                        "─".repeat(usize::from(rule.width))
                    },
                    Role::Border,
                    false,
                );
            }
        }
        let message = if !self.unsupported.is_empty() {
            Some(format!(
                "Unsupported filters: {}",
                self.unsupported.join(", ")
            ))
        } else if self.input.error().is_some() && layout.title.height == 0 {
            self.input.error().map(str::to_owned)
        } else if self.results.is_empty() {
            Some("No matching terminals".into())
        } else {
            None
        };
        if let Some(message) = message {
            // Compact preview must retain the reason for an empty result set.
            let rect = if layout.list.height > 0 {
                layout.list
            } else {
                layout.preview
            };
            role_line(
                &mut lines,
                &mut self.roles,
                rect,
                0,
                message,
                Role::Warning,
                false,
            );
        } else {
            for (visible, result) in self
                .results
                .iter()
                .skip(self.scroll)
                .take(layout.count())
                .enumerate()
            {
                let document = &self.documents[result.document];
                let selected = self.scroll + visible == self.selected;
                let row = visible as u16;
                let entry = entry::render(
                    document,
                    &self.terms,
                    &mut self.matcher,
                    &mut self.positions,
                    selected,
                    layout.list.width,
                );
                push_line(&mut lines, layout.list, row, entry.text, selected);
                for span in entry.spans {
                    match span {
                        entry::Span::Role(span) => self.roles.push(RoleSpan {
                            rect: Rect {
                                x: layout.list.x + span.rect.x,
                                y: layout.list.y + row,
                                ..span.rect
                            },
                            role: span.role,
                        }),
                        entry::Span::Project(rect) => self.projects.push(ProjectSpan {
                            rect: Rect {
                                x: layout.list.x + rect.x,
                                y: layout.list.y + row,
                                ..rect
                            },
                            slot: document.project_slot,
                        }),
                    }
                }
                if self.hovered.as_ref() == Some(&document.target.pane_id) && !selected {
                    self.roles.push(RoleSpan {
                        rect: Rect {
                            y: layout.list.y + row,
                            height: 1,
                            ..layout.list
                        },
                        role: Role::Hover,
                    });
                }
                self.match_cells.extend(
                    entry
                        .matches
                        .into_iter()
                        .map(|column| (layout.list.x + column as u16, layout.list.y + row)),
                );
            }
            for (index, line) in self
                .preview_lines
                .iter()
                .skip(self.preview_scroll)
                .take(preview_count)
                .enumerate()
            {
                role_line(
                    &mut lines,
                    &mut self.roles,
                    layout.preview,
                    index as u16,
                    line.text.clone(),
                    line.role,
                    false,
                );
                self.roles.extend(line.spans.iter().map(|span| RoleSpan {
                    rect: Rect {
                        x: layout.preview.x + span.rect.x,
                        y: layout.preview.y + index as u16,
                        ..span.rect
                    },
                    role: span.role,
                }));
                self.projects
                    .extend(line.projects.iter().map(|span| ProjectSpan {
                        rect: Rect {
                            x: layout.preview.x + span.rect.x,
                            y: layout.preview.y + index as u16,
                            ..span.rect
                        },
                        slot: span.slot,
                    }));
            }
        }
        if let Some(footer) = layout.footer {
            role_line(
                &mut lines,
                &mut self.roles,
                footer,
                0,
                "Tab views · ↑/↓ select · PgUp/PgDn preview · Enter open · Esc cancel".into(),
                Role::Muted,
                false,
            );
        }
        let cursor = (layout.input.height > 0
            && layout.input.width > 0
            && self.compact == CompactView::Input)
            .then(|| Cursor {
                x: layout.input.x
                    + (prefix.len() + used).min(usize::from(layout.input.width - 1)) as u16,
                y: layout.input.y,
                visible: true,
                blinking: true,
                shape: CursorShape::Bar,
                color: None,
            });
        surface.paint(size, lines, cursor);
        for &(x, y) in &self.match_cells {
            if let Some(cell) = surface
                .frame
                .rows
                .get_mut(usize::from(y))
                .and_then(|row| row.cells.get_mut(usize::from(x)))
            {
                cell.style.bold = true;
                cell.style.underline = 1;
            }
        }
    }

    pub(crate) fn apply_theme(
        &mut self,
        surface: &mut UiSurface,
        theme: &Theme,
        host: &ColorProbe,
        palette: &ProjectPalette,
    ) {
        let backgrounds = Self::graphic_regions(surface.frame.size)
            .filter(|_| self.graphical)
            .map(|regions| {
                std::array::from_fn::<_, 3, _>(|index| BackgroundSpan {
                    rect: regions[index].1,
                    background: crate::appearance::region_background(
                        host,
                        (index == 0).then_some(6),
                    )
                    .map_or(
                        bkpr_protocol::ColorSource::Default,
                        bkpr_protocol::ColorSource::Rgb,
                    ),
                })
            });
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
        surface.apply_theme_with_colors(
            theme,
            Role::Panel,
            &self.roles,
            &self.foregrounds,
            backgrounds.as_ref().map_or(&[], |spans| &spans[..]),
        );
        self.roles.truncate(roles_len);
    }
}

fn push_line(
    lines: &mut Vec<(Rect, String, bool)>,
    rect: Rect,
    row: u16,
    text: String,
    selected: bool,
) {
    if row < rect.height && rect.width > 0 {
        lines.push((
            Rect {
                x: rect.x,
                y: rect.y + row,
                width: rect.width,
                height: 1,
            },
            text,
            selected,
        ));
    }
}

fn role_line(
    lines: &mut Vec<(Rect, String, bool)>,
    roles: &mut Vec<RoleSpan>,
    rect: Rect,
    row: u16,
    text: String,
    role: Role,
    selected: bool,
) {
    if row < rect.height && rect.width > 0 {
        push_line(lines, rect, row, text, selected);
        roles.push(RoleSpan {
            rect: Rect {
                y: rect.y + row,
                height: 1,
                ..rect
            },
            role,
        });
    }
}

fn annotate(value: &Value, terms: &[Term], matcher: &mut Matcher, positions: &mut Vec<u32>) {
    for term in terms
        .iter()
        .filter(|term| term.field == Field::Text || term.field == value.field)
    {
        if let Some(atom) = &term.atom {
            let before = positions.len();
            if atom.indices(value.view(), matcher, positions).is_none() {
                positions.truncate(before);
            }
        }
    }
}
fn display_width(text: &str) -> usize {
    bkpr_tui_framework::text_width(text)
}
fn excerpt(value: &Value, positions: &[u32], width: usize) -> (String, Vec<usize>) {
    if width == 0 {
        return (String::new(), Vec::new());
    }
    let first = positions.iter().copied().min().unwrap_or(0) as usize;
    let last = positions.iter().copied().max().unwrap_or(0) as usize;
    let mut start = 0;
    // Fit the complete matched span when possible, not merely its first glyph.
    let through_match: usize = value
        .text
        .graphemes(true)
        .take(last + 1)
        .map(display_width)
        .sum();
    if !positions.is_empty() && through_match > width {
        start = first;
        let span: usize = value
            .text
            .graphemes(true)
            .skip(first)
            .take(last - first + 1)
            .map(display_width)
            .sum();
        let context_budget = width.saturating_sub(span + 2).min(width / 3);
        let byte = value
            .text
            .grapheme_indices(true)
            .nth(first)
            .map_or(0, |(byte, _)| byte);
        let mut before = 0;
        for glyph in value.text[..byte].graphemes(true).rev() {
            let length = display_width(glyph);
            if before + length > context_budget {
                break;
            }
            before += length;
            start -= 1;
        }
    }
    let mut text = String::new();
    let mut marks = Vec::new();
    let mut column = 0;
    let mut glyphs = value
        .text
        .graphemes(true)
        .enumerate()
        .skip(start)
        .peekable();
    if start > 0
        && glyphs
            .peek()
            .is_some_and(|(_, glyph)| display_width(glyph) < width)
    {
        text.push('…');
        column = 1;
    }
    for (index, glyph) in glyphs {
        let length = display_width(glyph);
        let matched = positions.contains(&(index as u32));
        let clipped = index + 1 < value.chars.len() && column + length >= width;
        if column + length > width || (clipped && !matched) {
            if column < width {
                text.push('…');
            }
            break;
        }
        text.push_str(glyph);
        if matched {
            marks.extend(column..column + length);
        }
        column += length;
    }
    (text, marks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PaneInfo, Window, Workspace};

    #[test]
    fn finder_and_preview_use_the_shared_client_mapping_and_invalidate_slot_changes() {
        use crate::project_colors::{ProjectColors, ProjectHost};
        use std::sync::Arc;
        let mut registry = registry();
        let project = crate::ci::tests::project();
        for pane in &mut registry.panes {
            pane.project = Some(project.clone());
        }
        let registry = Arc::new(registry);
        let local = ProjectHost::Local;
        let remote = ProjectHost::Remote {
            destination: "builder".into(),
            ssh_config: None,
        };
        let mut colors = ProjectColors::default();
        colors.reconcile([(&local, &registry), (&remote, &registry)]);
        let slots = colors.slots(&remote);
        let mut finder = Finder::new(&registry, Some("b"), slots, |_| None);
        finder.set_query("project:repo");
        let selected = finder.selected_target().cloned();
        assert!(
            finder
                .documents
                .iter()
                .all(|document| document.project_slot == Some(1))
        );
        let size = Size {
            cols: 120,
            rows: 40,
            ..size()
        };
        let mut surface = UiSurface::new(size);
        finder.paint(&mut surface, size);
        assert!(!finder.projects.is_empty());
        assert!(finder.projects.iter().all(|span| span.slot == Some(1)));
        let retained = finder.documents.as_ptr();
        finder.update_registry(&registry, slots, |_| None);
        assert_eq!(finder.documents.as_ptr(), retained);
        finder.update_registry(&registry, &ProjectSlots::default(), |_| None);
        assert_eq!(finder.selected_target(), selected.as_ref());
        assert!(
            finder
                .documents
                .iter()
                .all(|document| document.project_slot.is_none())
        );
        finder.paint(&mut surface, size);
        assert!(finder.projects.iter().all(|span| span.slot.is_none()));
    }
    fn registry() -> RegistrySnapshot {
        let pane = |id: &str, title: &str| PaneInfo {
            id: id.into(),
            workspace_id: Some("w".into()),
            window_id: Some(if id == "b" { "builder" } else { "shell" }.into()),
            title: title.into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: "/work/project".into(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: None,
            home: "/home/user".into(),
            isolated_environment: false,
            harness_status_changed_at_ms: None,
        };
        RegistrySnapshot {
            revision: 1,
            workspaces: vec![Workspace {
                id: "w".into(),
                name: "Main Work".into(),
                revision: 1,
                windows: ["builder", "shell"]
                    .into_iter()
                    .map(|id| Window {
                        id: id.into(),
                        name: id.into(),
                        revision: 1,
                        layout: None,
                    })
                    .collect(),
            }],
            panes: vec![pane("b", "builder"), pane("a", "build")],
        }
    }
    fn key(key: &str) -> KeyEvent {
        KeyEvent {
            key: key.into(),
            text: String::new(),
            modifiers: 0,
            action: KeyAction::Press,
            unshifted_codepoint: 0,
        }
    }
    fn size() -> Size {
        Size {
            cols: 60,
            rows: 12,
            cell_width: 0,
            cell_height: 0,
        }
    }

    #[test]
    fn palette_changes_retheme_unchanged_finder_without_losing_match_or_focus() {
        let mut finder = Finder::new(&registry(), Some("b"), &ProjectSlots::default(), |_| None);
        finder.set_query("build");
        let size = size();
        let mut surface = UiSurface::new(size);
        let mut theme = Theme::default();
        finder.paint(&mut surface, size);
        finder.apply_theme(
            &mut surface,
            &theme,
            &ColorProbe::default(),
            &Default::default(),
        );
        let cursor = surface.frame.cursor.clone();
        let selected = finder.selected_target().cloned();
        let (x, y) = finder.match_cells[0];
        let (x, y) = (usize::from(x), usize::from(y));
        assert!(surface.frame.rows[y].cells[x].style.bold);
        let mut host = bkpr_tui_framework::ColorProbe {
            truecolor: true,
            ..bkpr_tui_framework::ColorProbe::default()
        };
        for (foreground, background) in [
            ([30, 40, 50], [240, 245, 250]),
            ([220, 230, 240], [20, 30, 40]),
        ] {
            host.foreground = Some(foreground);
            host.background = Some(background);
            assert!(crate::appearance::update_theme(&mut theme, &host));
            let previous = surface.frame.rows[y].cells[x].style.background_source;
            finder.paint(&mut surface, size);
            finder.apply_theme(&mut surface, &theme, &host, &Default::default());
            let matched = &surface.frame.rows[y].cells[x];
            assert!(matched.style.bold);
            assert_eq!(matched.style.underline, 1);
            assert_ne!(matched.style.background_source, previous);
            assert!(surface.frame.rows[0].cells[0].style.bold);
            assert_eq!(surface.frame.cursor, cursor);
            assert_eq!(finder.selected_target(), selected.as_ref());
            // A second unchanged render must retain themed title attributes,
            // even though fuzzy-match emphasis is cleared and rebuilt.
            finder.paint(&mut surface, size);
            finder.apply_theme(&mut surface, &theme, &host, &Default::default());
            assert!(surface.frame.rows[0].cells[0].style.bold);
            assert!(surface.frame.rows[y].cells[x].style.bold);
        }
    }
    #[test]
    fn open_preview_updates_terminal_diagnostics_without_losing_query_or_selection() {
        let registry = registry();
        let mut snapshot: PaneSnapshot = serde_json::from_value(serde_json::json!({
            "pane_id":"b", "pid":null, "revision":1, "snapshot_revision":1,
            "frame":null, "image_refs":[], "resource_protocol":1, "writer":null,
            "lifecycle":"running", "exit_code":null, "error":"first terminal diagnostic",
            "program_status":[], "events":[], "event_sequence":0, "size_owner":null,
            "lease_generation":0, "views":[], "clipboard":null, "clipboard_pending":false,
            "capabilities":{"sixel":false,"normalized_frames":true,"native_mouse_reporting":false,
                "clipboard_timeout_ms":0,"view_timeout_ms":0,"poll_wait_max_ms":0,
                "row_deltas":false,"scene_encodings":[]}
        }))
        .unwrap();
        let mut finder = Finder::new(&registry, Some("b"), &ProjectSlots::default(), |id| {
            (id == "b").then_some(&snapshot)
        });
        finder.set_query("build");
        finder.navigate(1, size());
        let selected = finder.selected_target().cloned();
        assert_eq!(selected.as_ref().unwrap().pane_id, "b");
        let size = Size {
            cols: 120,
            rows: 40,
            ..size()
        };
        let mut surface = UiSurface::new(size);
        let visible = |surface: &UiSurface| {
            surface
                .frame
                .rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        };
        finder.paint(&mut surface, size);
        assert!(visible(&surface).contains("first terminal diagnostic"));
        for diagnostic in [Some("replacement terminal diagnostic"), None] {
            snapshot.snapshot_revision += 1;
            snapshot.error = diagnostic.map(str::to_owned);
            finder.update_registry(&registry, &ProjectSlots::default(), |id| {
                (id == "b").then_some(&snapshot)
            });
            finder.paint(&mut surface, size);
            let shown = visible(&surface);
            assert!(!shown.contains("first terminal diagnostic"));
            assert_eq!(
                shown.contains("replacement terminal diagnostic"),
                diagnostic.is_some()
            );
            assert_eq!(finder.selected_target(), selected.as_ref());
            assert_eq!(finder.input.value, "build");
        }
    }
    #[test]
    fn ranks_and_terms_match_separate_values_not_cross_value_fiction() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.set_query("build");
        assert_eq!(finder.selected_target().unwrap().pane_id, "a");
        finder.set_query("workspace:'Main Work' cwd:project status:running");
        assert_eq!(finder.results.len(), 2);
        finder.set_query("workspace: harness:");
        assert!(finder.results.is_empty());
        finder.set_query("Mainproject");
        assert!(finder.results.is_empty());
    }
    #[test]
    fn missing_forge_metadata_is_explicit_even_for_presence_queries() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.set_query("pr: ci:failed ticket:123");
        assert!(finder.results.is_empty());
        assert_eq!(finder.unsupported, ["pr", "ci", "ticket"]);
        finder.set_query("cwd:");
        assert_eq!(finder.results.len(), 2);
        assert!(finder.unsupported.is_empty());
    }
    #[test]
    fn registry_reorder_and_rename_preserve_identity_then_removed_selection_clamps() {
        let mut registry = registry();
        let mut finder = Finder::new(&registry, Some("b"), &ProjectSlots::default(), |_| None);
        registry.panes.reverse();
        registry.panes[1].title = "a completely different title".into();
        registry.revision += 1;
        finder.update_registry(&registry, &ProjectSlots::default(), |_| None);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        registry.panes.retain(|pane| pane.id != "b");
        registry.revision += 1;
        finder.update_registry(&registry, &ProjectSlots::default(), |_| None);
        assert_eq!(finder.selected_target().unwrap().pane_id, "a");
        finder.navigate(isize::MAX, size());
        assert_eq!(finder.selected, 0);
        assert_eq!(finder.scroll, 0);
    }
    #[test]
    fn window_queries_keep_exact_pane_owner_across_same_workspace_windows() {
        let mut registry = registry();
        let mut finder = Finder::new(&registry, None, &ProjectSlots::default(), |_| None);
        finder.set_query("window:builder");
        assert_eq!(finder.results.len(), 1);
        let target = finder.selected_target().unwrap().clone();
        assert_eq!(target.workspace_id.as_deref(), Some("w"));
        assert_eq!(target.pane_id, "b");
        registry.workspaces[0].windows.reverse();
        registry.panes.reverse();
        registry.revision += 1;
        finder.update_registry(&registry, &ProjectSlots::default(), |_| None);
        assert_eq!(finder.selected_target(), Some(&target));
        assert_eq!(
            finder.handle_key(&key("Enter"), size()),
            FinderIntent::Activate(target)
        );
    }
    #[test]
    fn unicode_editing_erases_graphemes_and_bounds_pastes() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.paste("e\u{301}👩‍💻界");
        finder.handle_key(&key("ArrowLeft"), size());
        finder.handle_key(&key("Backspace"), size());
        assert_eq!(finder.input.value, "e\u{301}界");
        finder.handle_key(&key("Delete"), size());
        assert_eq!(finder.input.value, "e\u{301}");
        finder.handle_key(&key("Backspace"), size());
        assert_eq!(finder.input.value, "");
        finder.paste(&"界".repeat(crate::TEXT_INPUT_LIMIT));
        assert_eq!(finder.input.value, "");
        assert_eq!(finder.input.cursor, 0);
        assert!(finder.input.error().is_some());
        finder.set_query("a\n\u{1b}b");
        assert_eq!(finder.input.value, "a  b");
    }
    #[test]
    fn context_excerpt_keeps_late_unicode_match_visible() {
        let value = Value::new(Field::Cwd, "long/path/界e\u{301}nd");
        let (text, columns) = excerpt(&value, &[10, 11], 6);
        assert!(text.contains('界'));
        assert!(!columns.is_empty());
        assert!(columns.iter().all(|column| *column < 6));
    }

    #[test]
    fn joined_emoji_excerpt_highlights_its_two_cells_without_clipping_the_cluster() {
        let value = Value::new(Field::Text, "a👩‍💻bc");
        assert_eq!(excerpt(&value, &[1], 3), ("a👩‍💻".into(), vec![1, 2]));
        assert_eq!(excerpt(&value, &[1], 2), ("👩‍💻".into(), vec![0, 1]));
        assert_eq!(excerpt(&value, &[1], 1), ("…".into(), vec![]));
    }

    #[test]
    fn finder_caret_tracks_joined_emoji_and_moves_by_whole_graphemes() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.paste("e\u{301}👩‍💻界");
        let mut surface = UiSurface::new(size());
        let input = finder.layout(size()).input;
        for (key_name, offset) in [(None, 7), (Some("ArrowLeft"), 5), (Some("ArrowLeft"), 3)] {
            if let Some(key_name) = key_name {
                finder.handle_key(&key(key_name), size());
            }
            finder.paint(&mut surface, size());
            assert!(surface.frame.cursor.visible);
            assert_eq!(surface.frame.cursor.x, input.x + offset);
        }
        let cells = &surface.frame.rows[usize::from(input.y)].cells;
        assert_eq!(cells[usize::from(input.x) + 2].text, "e\u{301}");
        assert_eq!(cells[usize::from(input.x) + 3].text, "👩‍💻");
        assert!(cells[usize::from(input.x) + 4].text.is_empty());
        assert_eq!(cells[usize::from(input.x) + 5].text, "界");
    }
    fn mouse(x: f32, y: f32, button: MouseButton, action: MouseAction) -> MouseEvent {
        MouseEvent {
            x,
            y,
            modifiers: 0,
            button,
            action,
        }
    }
    #[test]
    fn compact_preview_is_keyboard_reachable_and_selection_stays_put() {
        let mut finder = Finder::new(&registry(), Some("b"), &ProjectSlots::default(), |_| None);
        finder.handle_key(&key("Tab"), size());
        finder.handle_key(&key("Tab"), size());
        assert_eq!(finder.compact, CompactView::Preview);
        assert_eq!(finder.layout(size()).list.height, 0);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        finder.handle_key(&key("PageDown"), size());
        assert!(!finder.preview_lines.is_empty());
        finder.handle_key(&key("ArrowUp"), size());
        assert_ne!(finder.layout(size()).list.height, 0);
    }
    #[test]
    fn pointer_scroll_routes_to_preview_and_click_activation_uses_stable_id() {
        let mut registry = registry();
        registry.panes[0].cwd = format!("/{}", "long-path/".repeat(40)).into();
        let mut finder = Finder::new(&registry, Some("b"), &ProjectSlots::default(), |_| None);
        let wide = Size {
            cols: 90,
            rows: 10,
            ..size()
        };
        let list = finder.layout(wide).list;
        finder.handle_mouse(
            &mouse(60.0, 3.0, MouseButton::WheelDown, MouseAction::Press),
            wide,
        );
        assert!(finder.preview_scroll > 0);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        assert_eq!(
            finder.handle_mouse(
                &mouse(
                    f32::from(list.x + 2),
                    f32::from(list.y),
                    MouseButton::Left,
                    MouseAction::Press
                ),
                wide
            ),
            FinderIntent::None
        );
        let selected = finder.selected_target().unwrap().clone();
        registry.panes.reverse();
        registry.revision += 1;
        finder.update_registry(&registry, &ProjectSlots::default(), |_| None);
        assert_eq!(
            finder.handle_mouse(
                &mouse(
                    f32::from(list.x + 2),
                    f32::from(list.y),
                    MouseButton::Left,
                    MouseAction::Press
                ),
                wide
            ),
            FinderIntent::Activate(selected)
        );
        assert_eq!(
            finder.handle_mouse(
                &mouse(f32::NAN, 3.0, MouseButton::Left, MouseAction::Press),
                wide
            ),
            FinderIntent::None
        );
    }
    #[test]
    fn rejected_paste_and_cursor_motion_preserve_ranked_selection() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.set_query("build");
        finder.navigate(1, size());
        let selected = finder.selected_target().unwrap().clone();
        finder.handle_key(&key("ArrowLeft"), size());
        assert_eq!(finder.selected_target(), Some(&selected));
        let cursor = finder.input.cursor;
        finder.paste(&"x".repeat(crate::TEXT_INPUT_LIMIT));
        assert_eq!(finder.input.value, "build");
        assert_eq!(finder.input.cursor, cursor);
        assert_eq!(finder.selected_target(), Some(&selected));
        assert!(finder.input.error().is_some());
    }
    #[test]
    fn graphical_regions_share_painted_and_pointer_geometry() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        for (cols, rows) in [(120, 28), (84, 18), (60, 24), (40, 14)] {
            let size = Size {
                cols,
                rows,
                ..size()
            };
            let regions = Finder::graphic_regions(size).unwrap();
            finder.set_graphical(true);
            assert!(finder.is_graphical());
            let layout = finder.layout(size);
            assert_eq!(layout.list, regions[1].1);
            assert_eq!(layout.preview, regions[2].1);
            for (card, text) in regions {
                assert!(card.x + card.width <= cols && card.y + card.height <= rows);
                assert!(contains(card, text.x, text.y));
                assert!(contains(
                    card,
                    text.x + text.width - 1,
                    text.y + text.height - 1
                ));
            }
            let mut surface = UiSurface::new(size);
            finder.paint(&mut surface, size);
            let selected_row = layout.list.y + (finder.selected - finder.scroll) as u16;
            assert!(
                surface.frame.rows[usize::from(selected_row)].cells
                    [usize::from(layout.list.x + layout.list.width - 1)]
                .style
                .inverse
            );
            let first = finder
                .target(&finder.results[finder.scroll])
                .unwrap()
                .clone();
            let second = finder
                .target(&finder.results[finder.scroll + 1])
                .unwrap()
                .clone();
            let row_text = |y: u16| {
                surface.frame.rows[usize::from(y)]
                    .cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            };
            assert!(
                row_text(layout.list.y)
                    .contains(&finder.documents[finder.results[0].document].values[0].text)
            );
            assert!(
                row_text(layout.list.y + 1)
                    .contains(&finder.documents[finder.results[1].document].values[0].text)
            );
            for (row, target) in [(0, first), (1, second)] {
                finder.handle_mouse(
                    &mouse(
                        f32::from(layout.list.x),
                        f32::from(layout.list.y + row),
                        MouseButton::Left,
                        MouseAction::Press,
                    ),
                    size,
                );
                assert_eq!(finder.selected_target(), Some(&target));
            }
        }
    }

    #[test]
    fn presentation_toggle_and_tiny_resize_preserve_query_selection_and_preview_focus() {
        let mut finder = Finder::new(&registry(), None, &ProjectSlots::default(), |_| None);
        finder.paste("build");
        finder.navigate(1, size());
        finder.handle_key(&key("ArrowLeft"), size());
        finder.compact = CompactView::Preview;
        let target = finder.selected_target().cloned();
        let query = finder.input.value.clone();
        let cursor = finder.input.cursor;
        let mut surface = UiSurface::new(size());
        for (cols, rows) in [
            (110, 28),
            (60, 24),
            (40, 14),
            (30, 8),
            (8, 3),
            (1, 1),
            (0, 0),
            (110, 28),
        ] {
            let size = Size {
                cols,
                rows,
                ..size()
            };
            finder.set_graphical(Finder::graphic_regions(size).is_some());
            finder.paint(&mut surface, size);
            assert_eq!(finder.input.value, query);
            assert_eq!(finder.input.cursor, cursor);
            assert_eq!(finder.selected_target(), target.as_ref());
            assert_eq!(finder.compact, CompactView::Preview);
            assert!(!surface.frame.cursor.visible);
        }
    }

    #[test]
    fn compact_view_switch_keeps_preview_scroll_when_hidden() {
        let mut registry = registry();
        registry.panes[0].cwd = format!("/{}", "long-path/".repeat(40)).into();
        let mut finder = Finder::new(&registry, Some("b"), &ProjectSlots::default(), |_| None);
        finder.scroll_preview(3, size());
        let scroll = finder.preview_scroll;
        assert!(scroll > 0);
        let mut surface = UiSurface::new(size());
        finder.compact = CompactView::List;
        finder.paint(&mut surface, size());
        assert_eq!(finder.preview_scroll, scroll);
        finder.compact = CompactView::Preview;
        finder.paint(&mut surface, size());
        assert_eq!(finder.preview_scroll, scroll);
    }

    #[test]
    fn metadata_matches_stay_in_their_actual_one_row_excerpt() {
        let mut registry = registry();
        registry.panes[0].cwd = "/a/long/directory/with/needle/at/the/end".into();
        let mut finder = Finder::new(&registry, None, &ProjectSlots::default(), |_| None);
        finder.set_query("cwd:needle");
        assert_eq!(finder.results.len(), 1);
        for width in [12, 29, 60] {
            let document = &finder.documents[finder.results[0].document];
            let row = entry::render(
                document,
                &finder.terms,
                &mut finder.matcher,
                &mut finder.positions,
                true,
                width,
            );
            assert!(!row.matches.is_empty());
            assert_eq!(display_width(&row.text), usize::from(width));
            let matched: String = row
                .text
                .chars()
                .enumerate()
                .filter_map(|(column, ch)| row.matches.contains(&column).then_some(ch))
                .collect();
            assert!("needle".contains(&matched));
            assert!(matched.len() >= 2);
        }
    }

    #[test]
    fn preview_uses_local_repository_facts_without_exposing_identity_dump() {
        use bkpr_protocol::{ChangeSummary, ProjectSnapshot, ProjectState, RepositoryInfo};
        let mut registry = registry();
        registry.panes[0].id = "private-pane-uuid".into();
        registry.panes[0].project = Some(ProjectSnapshot {
            ci: None,
            cwd: "/work/project".into(),
            state: ProjectState::Loading,
            error: None,
            repository: Some(RepositoryInfo {
                name: "project".into(),
                root: "/work/project".into(),
                common_dir: "/work/project/.git".into(),
                git_dir: "/work/project/.git".into(),
                branch: Some("feature/日本語".into()),
                head_sha: Some("abcdef1234567890".into()),
                staged: ChangeSummary {
                    files: 2,
                    insertions: 8,
                    deletions: 3,
                    binary_files: 1,
                },
                unstaged: ChangeSummary::default(),
                untracked_files: 4,
                base_ref: Some("main".into()),
                base_sha: Some("1234567890abcdef".into()),
                committed: Some(ChangeSummary {
                    files: 3,
                    insertions: 20,
                    deletions: 7,
                    binary_files: 0,
                }),
            }),
        });
        let finder = Finder::new(
            &registry,
            Some("private-pane-uuid"),
            &ProjectSlots::default(),
            |_| None,
        );
        let document = &finder.documents[finder.results[finder.selected].document];
        let preview = document
            .preview
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!preview.contains("private-pane-uuid"));
        assert!(!preview.contains("PID:"));
        assert!(preview.contains("feature/日本語"));
        assert!(preview.contains("+8 −3"));
        assert!(preview.contains("+20 −7"));
        assert!(
            document
                .preview
                .iter()
                .any(|p| p.role == Role::Warning && p.text.contains("stale"))
        );
        assert!(document.preview.iter().any(|p| p.role == Role::Title));
        assert!(preview.contains("not available"));
    }
}
