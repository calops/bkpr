//! Metadata-only pane finder. No terminal contents, transport, or backend access.
//!
//! Query tokenization and nucleo matching adapted from bkpr-zellij's
//! pane_search.rs (Copyright (c) 2026 Rémi Labeyrie, MIT). The repository's
//! MIT license notice applies. Projection, identity, and retained painting here
//! use only the current protocol. Forge PR/CI/ticket facts are not available.

use bkpr_protocol::{
    Cursor, CursorShape, KeyAction, KeyEvent, PaneSnapshot, PaneStatus, RegistrySnapshot, Size,
};
use bkpr_protocol::{MouseAction, MouseButton, MouseEvent};
use bkpr_tui_framework::Rect;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use bkpr_tui_framework::{Role, RoleSpan, TextInput, Theme, UiSurface};

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
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
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
    project_stale: bool,
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

#[derive(Clone, Copy)]
struct FinderLayout {
    list: Rect,
    preview: Rect,
}
impl FinderLayout {
    fn new(size: Size, compact: CompactView) -> Self {
        let height = size.rows.saturating_sub(if size.rows > 3 { 3 } else { 2 });
        let mut layout = Self {
            list: Rect {
                x: 0,
                y: 2,
                width: size.cols,
                height,
            },
            preview: Rect {
                x: 0,
                y: 2,
                width: 0,
                height: 0,
            },
        };
        if size.cols >= 84 {
            let split = size.cols / 2;
            layout.list.width = split.saturating_sub(1);
            layout.preview = Rect {
                x: split,
                y: 2,
                width: size.cols - split,
                height,
            };
        } else if height >= 12 {
            let split = height / 2;
            layout.list.height = split;
            layout.preview = Rect {
                x: 0,
                y: 2 + split,
                width: size.cols,
                height: height - split,
            };
        } else if compact == CompactView::Preview {
            layout.preview = layout.list;
            layout.list.height = 0;
        }
        layout
    }
    fn count(self) -> usize {
        usize::from(self.list.height.div_ceil(2))
    }
}
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}
fn preview_capacity(rect: Rect) -> usize {
    usize::from(rect.height.saturating_sub(u16::from(rect.height > 1)))
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
    preview_lines: Vec<String>,
    preview_key: Option<(u64, String, u16)>,
}
impl Finder {
    pub fn new<'a>(
        registry: &RegistrySnapshot,
        initial_pane: Option<&str>,
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
        };
        finder.update_registry(registry, snapshot);
        if let Some(index) = finder.results.iter().position(|result| {
            Some(finder.documents[result.document].target.pane_id.as_str()) == initial_pane
        }) {
            finder.selected = index;
        }
        finder
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
                    document.harness_visible
                        == crate::controller::recognized_harness(pane, known).is_some()
                        && document.status_visible
                            == crate::controller::harness_status(pane, known).is_some()
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
                project_stale: pane
                    .project
                    .as_ref()
                    .is_some_and(|project| project.state != bkpr_protocol::ProjectState::Ready),
            };
            // First value is always the title, including for an unnamed pane.
            document.values.push(Value::new(
                Field::Text,
                if pane.title.is_empty() {
                    &pane.id
                } else {
                    &pane.title
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
        FinderLayout::new(size, self.compact).count()
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
        if FinderLayout::new(size, self.compact).list.height == 0 {
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
        for (index, value) in document.values.iter().enumerate() {
            let label = if index == 0 { "title" } else { value.label() };
            wrap_preview(
                &format!("{label}: {}", value.text),
                width as usize,
                &mut self.preview_lines,
            );
        }
        self.preview_key = Some((revision, document.target.pane_id.clone(), width));
    }
    fn scroll_preview(&mut self, delta: isize, size: Size) {
        self.compact = CompactView::Preview;
        let rect = FinderLayout::new(size, self.compact).preview;
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
        let layout = FinderLayout::new(size, self.compact);
        let in_list = contains(layout.list, x, y);
        let in_preview = contains(layout.preview, x, y);
        let hit = in_list
            .then(|| self.scroll + usize::from((y - layout.list.y) / 2))
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
                let page =
                    preview_capacity(FinderLayout::new(size, CompactView::Preview).preview).max(1);
                self.scroll_preview(-(page as isize), size);
            }
            Some(Action::Navigate(Navigation::PageNext)) => {
                let page =
                    preview_capacity(FinderLayout::new(size, CompactView::Preview).preview).max(1);
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

    /// Paint a complete focused popup content surface at its supplied local size.
    /// Only visible documents are annotated. Matched graphemes are bold; the
    /// selected result retains the surface's normal selection background.
    pub fn paint(&mut self, surface: &mut UiSurface, size: Size) {
        let layout = FinderLayout::new(size, self.compact);
        self.reveal(layout.count());
        self.prepare_preview(layout.preview.width);
        let preview_count = preview_capacity(layout.preview);
        self.preview_scroll = self
            .preview_scroll
            .min(self.preview_lines.len().saturating_sub(preview_count));
        let mut lines = Vec::new();
        let mut highlights = Vec::new();
        let full = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        push_line(
            &mut lines,
            full,
            0,
            self.input.error().unwrap_or("Find terminal").into(),
            false,
        );
        let (visible, used) = self.input.visible(usize::from(size.cols.saturating_sub(2)));
        push_line(&mut lines, full, 1, format!("> {visible}"), false);
        if !self.unsupported.is_empty() {
            push_line(
                &mut lines,
                full,
                2,
                format!(
                    "Unavailable metadata filters: {}",
                    self.unsupported.join(", ")
                ),
                false,
            );
        } else if self.results.is_empty() {
            push_line(&mut lines, full, 2, "No matching terminals".into(), false);
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
                let y = visible as u16 * 2;
                let title = &document.values[0];
                self.positions.clear();
                annotate(title, &self.terms, &mut self.matcher, &mut self.positions);
                let (text, marks) = excerpt(title, &self.positions, layout.list.width as usize);
                push_line(&mut lines, layout.list, y, text, selected);
                highlights.push((
                    layout.list.y + y,
                    marks
                        .into_iter()
                        .map(|column| column + usize::from(layout.list.x))
                        .collect::<Vec<_>>(),
                ));
                let context = document
                    .values
                    .iter()
                    .skip(1)
                    .filter_map(|value| {
                        let score = self
                            .terms
                            .iter()
                            .filter(|term| term.field == Field::Text || term.field == value.field)
                            .filter_map(|term| term.atom.as_ref())
                            .filter_map(|atom| atom.score(value.view(), &mut self.matcher))
                            .max();
                        score.map(|score| (score, value))
                    })
                    .max_by_key(|(score, _)| *score)
                    .map(|(_, value)| value)
                    .or_else(|| {
                        document
                            .values
                            .iter()
                            .find(|value| value.field == Field::Cwd)
                    });
                if let Some(value) = context.filter(|_| y + 1 < layout.list.height) {
                    self.positions.clear();
                    annotate(value, &self.terms, &mut self.matcher, &mut self.positions);
                    let prefix = format!("  {}: ", value.label());
                    let prefix_width = display_width(&prefix);
                    let (text, marks) = excerpt(
                        value,
                        &self.positions,
                        (layout.list.width as usize).saturating_sub(prefix_width),
                    );
                    push_line(
                        &mut lines,
                        layout.list,
                        y + 1,
                        format!("{prefix}{text}"),
                        selected,
                    );
                    highlights.push((
                        layout.list.y + y + 1,
                        marks
                            .into_iter()
                            .map(|column| column + prefix_width + usize::from(layout.list.x))
                            .collect(),
                    ));
                }
            }
            if layout.preview.width > 0 && layout.preview.height > 0 {
                let header = u16::from(layout.preview.height > 1);
                if header > 0 {
                    push_line(
                        &mut lines,
                        layout.preview,
                        0,
                        "Details · PgUp/PgDn scroll".into(),
                        false,
                    );
                }
                for (index, text) in self
                    .preview_lines
                    .iter()
                    .skip(self.preview_scroll)
                    .take(preview_count)
                    .enumerate()
                {
                    push_line(
                        &mut lines,
                        layout.preview,
                        index as u16 + header,
                        text.clone(),
                        false,
                    );
                }
            }
        }
        if size.rows > 3 {
            push_line(
                &mut lines,
                full,
                size.rows - 1,
                format!(
                    "{} terminals · Tab views · Enter select · Esc cancel",
                    self.results.len()
                ),
                false,
            );
        }
        let cursor = (size.rows > 1 && size.cols > 2 && self.compact == CompactView::Input)
            .then_some(Cursor {
                x: (2 + used) as u16,
                y: 1,
                visible: true,
                blinking: true,
                shape: CursorShape::Bar,
                color: None,
            });
        // Only fuzzy-result emphasis is ours to clear. Titles and preview
        // headings belong to the retained theme and may not repaint this frame.
        for row in surface
            .frame
            .rows
            .iter_mut()
            .skip(usize::from(layout.list.y))
            .take(usize::from(layout.list.height))
        {
            for cell in row
                .cells
                .iter_mut()
                .skip(usize::from(layout.list.x))
                .take(usize::from(layout.list.width))
            {
                cell.style.bold = false;
            }
        }
        surface.paint(size, lines, cursor);
        for (y, columns) in highlights {
            if let Some(row) = surface.frame.rows.get_mut(y as usize) {
                for column in columns {
                    if let Some(cell) = row.cells.get_mut(column) {
                        cell.style.bold = true;
                    }
                }
            }
        }
    }

    pub(crate) fn apply_theme(&self, surface: &mut UiSurface, theme: &Theme) {
        let size = surface.frame.size;
        let layout = FinderLayout::new(size, self.compact);
        let full = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: 1,
        };
        let mut spans = vec![
            RoleSpan {
                rect: full,
                role: if self.input.error().is_some() {
                    Role::Error
                } else {
                    Role::Title
                },
            },
            RoleSpan {
                rect: Rect { y: 1, ..full },
                role: Role::Accent,
            },
            RoleSpan {
                rect: Rect { y: 2, ..full },
                role: Role::Muted,
            },
            RoleSpan {
                rect: Rect {
                    y: size.rows.saturating_sub(1),
                    ..full
                },
                role: Role::Muted,
            },
        ];
        for (visible, result) in self
            .results
            .iter()
            .skip(self.scroll)
            .take(layout.count())
            .enumerate()
        {
            let y = layout.list.y + visible as u16 * 2;
            let hovered =
                self.hovered.as_ref() == Some(&self.documents[result.document].target.pane_id);
            spans.push(RoleSpan {
                rect: Rect {
                    y,
                    height: 1,
                    ..layout.list
                },
                role: if hovered { Role::Hover } else { Role::Panel },
            });
            if y + 1 < layout.list.y + layout.list.height {
                spans.push(RoleSpan {
                    rect: Rect {
                        y: y + 1,
                        height: 1,
                        ..layout.list
                    },
                    role: if hovered { Role::Hover } else { Role::Muted },
                });
            }
        }
        if layout.preview.width > 0 && layout.preview.height > 0 {
            spans.push(RoleSpan {
                rect: layout.preview,
                role: Role::Muted,
            });
            spans.push(RoleSpan {
                rect: Rect {
                    height: 1,
                    ..layout.preview
                },
                role: Role::Title,
            });
        }
        surface.apply_theme(theme, Role::Panel, &spans);
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

fn wrap_preview(text: &str, width: usize, lines: &mut Vec<String>) {
    if width == 0 {
        return;
    }
    let mut line = String::new();
    let mut columns = 0;
    for grapheme in text.graphemes(true) {
        let length = display_width(grapheme);
        if columns + length > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            columns = 0;
        }
        line.push_str(grapheme);
        columns += length;
    }
    if !line.is_empty() {
        lines.push(line);
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
    // Match UiSurface's scalar-width renderer, including wide-glyph continuation.
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}
fn excerpt(value: &Value, positions: &[u32], width: usize) -> (String, Vec<usize>) {
    let first = positions.iter().copied().min().unwrap_or(0) as usize;
    let mut start = 0;
    let mut preceding = 0;
    // Shift the context when the first match would otherwise be clipped away.
    for (index, grapheme) in value.text.graphemes(true).enumerate().take(first) {
        preceding += display_width(grapheme);
        if preceding >= width.saturating_sub(2) {
            start = index + 1;
        }
    }
    let mut text = String::new();
    let mut marks = Vec::new();
    let mut column = 0;
    if start > 0 && width > 0 {
        text.push('…');
        column = 1;
    }
    for (index, grapheme) in value.text.graphemes(true).enumerate().skip(start) {
        let length = display_width(grapheme);
        if column + length > width {
            break;
        }
        text.push_str(grapheme);
        if positions.contains(&(index as u32)) {
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
        let mut finder = Finder::new(&registry(), Some("b"), |_| None);
        finder.set_query("build");
        let size = size();
        let layout = FinderLayout::new(size, finder.compact);
        let mut surface = UiSurface::new(size);
        let mut theme = Theme::default();
        finder.paint(&mut surface, size);
        finder.apply_theme(&mut surface, &theme);
        let cursor = surface.frame.cursor.clone();
        let selected = finder.selected_target().cloned();
        let y = usize::from(layout.list.y);
        let x = usize::from(layout.list.x);
        assert!(surface.frame.rows[y].cells[x].style.bold);
        surface.frame.rows[y].cells[x].style.underline = 1;
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
            finder.apply_theme(&mut surface, &theme);
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
            finder.apply_theme(&mut surface, &theme);
            assert!(surface.frame.rows[0].cells[0].style.bold);
            assert!(surface.frame.rows[y].cells[x].style.bold);
        }
    }
    #[test]
    fn ranks_and_terms_match_separate_values_not_cross_value_fiction() {
        let mut finder = Finder::new(&registry(), None, |_| None);
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
        let mut finder = Finder::new(&registry(), None, |_| None);
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
        let mut finder = Finder::new(&registry, Some("b"), |_| None);
        registry.panes.reverse();
        registry.panes[1].title = "a completely different title".into();
        registry.revision += 1;
        finder.update_registry(&registry, |_| None);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        registry.panes.retain(|pane| pane.id != "b");
        registry.revision += 1;
        finder.update_registry(&registry, |_| None);
        assert_eq!(finder.selected_target().unwrap().pane_id, "a");
        finder.navigate(isize::MAX, size());
        assert_eq!(finder.selected, 0);
        assert_eq!(finder.scroll, 0);
    }
    #[test]
    fn window_queries_keep_exact_pane_owner_across_same_workspace_windows() {
        let mut registry = registry();
        let mut finder = Finder::new(&registry, None, |_| None);
        finder.set_query("window:builder");
        assert_eq!(finder.results.len(), 1);
        let target = finder.selected_target().unwrap().clone();
        assert_eq!(target.workspace_id.as_deref(), Some("w"));
        assert_eq!(target.pane_id, "b");
        registry.workspaces[0].windows.reverse();
        registry.panes.reverse();
        registry.revision += 1;
        finder.update_registry(&registry, |_| None);
        assert_eq!(finder.selected_target(), Some(&target));
        assert_eq!(
            finder.handle_key(&key("Enter"), size()),
            FinderIntent::Activate(target)
        );
    }
    #[test]
    fn unicode_editing_erases_graphemes_and_bounds_pastes() {
        let mut finder = Finder::new(&registry(), None, |_| None);
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
        let mut finder = Finder::new(&registry(), Some("b"), |_| None);
        finder.handle_key(&key("Tab"), size());
        finder.handle_key(&key("Tab"), size());
        assert_eq!(finder.compact, CompactView::Preview);
        assert_eq!(FinderLayout::new(size(), finder.compact).list.height, 0);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        finder.handle_key(&key("PageDown"), size());
        assert!(!finder.preview_lines.is_empty());
        finder.handle_key(&key("ArrowUp"), size());
        assert_ne!(FinderLayout::new(size(), finder.compact).list.height, 0);
    }
    #[test]
    fn pointer_scroll_routes_to_preview_and_click_activation_uses_stable_id() {
        let mut registry = registry();
        registry.panes[0].cwd = format!("/{}", "long-path/".repeat(40)).into();
        let mut finder = Finder::new(&registry, Some("b"), |_| None);
        let wide = Size {
            cols: 90,
            rows: 10,
            ..size()
        };
        finder.handle_mouse(
            &mouse(60.0, 3.0, MouseButton::WheelDown, MouseAction::Press),
            wide,
        );
        assert!(finder.preview_scroll > 0);
        assert_eq!(finder.selected_target().unwrap().pane_id, "b");
        assert_eq!(
            finder.handle_mouse(
                &mouse(2.0, 2.0, MouseButton::Left, MouseAction::Press),
                wide
            ),
            FinderIntent::None
        );
        let selected = finder.selected_target().unwrap().clone();
        registry.panes.reverse();
        registry.revision += 1;
        finder.update_registry(&registry, |_| None);
        assert_eq!(
            finder.handle_mouse(
                &mouse(2.0, 2.0, MouseButton::Left, MouseAction::Press),
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
    fn narrow_layout_and_unicode_preview_wrap_keep_content_reachable() {
        let tiny = Size {
            cols: 8,
            rows: 3,
            ..size()
        };
        assert_eq!(FinderLayout::new(tiny, CompactView::List).count(), 1);
        assert_eq!(
            FinderLayout::new(tiny, CompactView::Preview).preview.height,
            1
        );
        let mut lines = Vec::new();
        wrap_preview("界e\u{301}tail", 3, &mut lines);
        assert_eq!(lines.concat(), "界e\u{301}tail");
        assert!(lines.iter().all(|line| display_width(line) <= 3));
    }
    #[test]
    fn rejected_paste_and_cursor_motion_preserve_ranked_selection() {
        let mut finder = Finder::new(&registry(), None, |_| None);
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
}
