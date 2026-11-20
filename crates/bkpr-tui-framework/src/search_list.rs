//! Retained fuzzy single-list content, independent of application identities and palettes.
//! The caller owns the outer window, title, placement, and cancellation rollback.

use crate::{
    BackgroundSpan, EditAction, ForegroundSpan, Rect, Role, RoleSpan, TextInput, Theme, UiSurface,
    window,
};
use bkpr_protocol::{
    Cursor, CursorShape, KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent, Size,
};
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;

/// Queries are bounded in UTF-8 bytes; oversized insertions are rejected atomically.
const QUERY_LIMIT: usize = 4096;

/// Action requested from the owner of a focused search list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchListIntent {
    /// No owner action is requested.
    None,
    /// Cancel the list; the owner restores any prior selection.
    Cancel,
    /// Commit the currently selected value.
    Accept,
}

/// Whether a pointer press only selects a row or also requests acceptance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseActivation {
    /// Select without accepting; the owner supplies a separate submit action.
    Select,
    /// Select and accept on a left press.
    #[default]
    Accept,
}

/// Presentation geometry; it never depends on artwork readiness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListLayout {
    /// Compact content inside a caller-owned window.
    #[default]
    Inline,
    /// Floating search/results cards, optionally reserving a details card.
    Cards {
        /// Reserve responsive details geometry for caller-owned content.
        details: bool,
    },
    /// Full-width search/results cards with details always below the results.
    /// Uses compact content only when the stacked cards cannot fit.
    StackedCards,
}

/// Current content rectangles in the same coordinates as paint and pointer input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentLayout {
    /// Optional heading row.
    pub title: Rect,
    /// Query editor row.
    pub input: Rect,
    /// Actual result rows, excluding any query error.
    pub results: Rect,
    /// Optional caller-owned details content.
    pub details: Rect,
}

struct Item<T> {
    value: T,
    label: String,
    // Nucleo indexes graphemes, rather than UTF-8 bytes or terminal columns.
    chars: Vec<char>,
    presentation: Option<(String, Vec<Option<u32>>)>,
}

impl<T> Item<T> {
    fn new(value: T, label: String) -> Self {
        let label = window::clean(&label);
        let chars = label
            .graphemes(true)
            .filter_map(|grapheme| grapheme.chars().next())
            .collect();
        Self {
            value,
            label,
            chars,
            presentation: None,
        }
    }
}

struct ResultRow {
    item: usize,
    score: u32,
    matches: Vec<u32>,
}

/// A bounded query editor followed by fuzzy-ranked, single-line choices.
/// Equal scores retain input order. Query changes select the best result; mere
/// resize, repaint, and cursor movement preserve the selected identity.
/// Match scores and positions are recomputed only when query content changes.
/// Paint into a dedicated content-sized surface, then apply the caller's theme.
pub struct SearchList<T> {
    items: Vec<Item<T>>,
    title: Option<String>,
    input: TextInput,
    matcher: Matcher,
    results: Vec<ResultRow>,
    selected: usize,
    scroll: usize,
    roles: Vec<RoleSpan>,
    match_cells: Vec<(u16, u16)>,
    mouse_activation: MouseActivation,
    select_on_hover: bool,
    layout: ListLayout,
}

impl<T: Clone + Eq> SearchList<T> {
    /// Builds choices in input order, selecting `initial` when present or the first choice.
    /// Labels are sanitized for terminal display; an empty list needs no placeholder value.
    pub fn new(items: impl IntoIterator<Item = (T, String)>, initial: Option<&T>) -> Self {
        let items: Vec<_> = items
            .into_iter()
            .map(|(value, label)| Item::new(value, label))
            .collect();
        let selected = initial
            .and_then(|initial| items.iter().position(|item| &item.value == initial))
            .unwrap_or(0);
        let mut list = Self {
            items,
            title: None,
            input: TextInput::new(String::new(), QUERY_LIMIT),
            matcher: Matcher::default(),
            results: Vec::new(),
            selected,
            scroll: 0,
            roles: Vec::new(),
            match_cells: Vec::new(),
            mouse_activation: MouseActivation::default(),
            select_on_hover: true,
            layout: ListLayout::default(),
        };
        list.rank();
        list
    }

    /// Replace choices without changing query text or the selected matching identity.
    /// If that identity disappears or stops matching, select the first result.
    pub fn replace_items(&mut self, items: impl IntoIterator<Item = (T, String)>) {
        let items: Vec<_> = items
            .into_iter()
            .map(|(value, label)| Item::new(value, label))
            .collect();
        let selected = self
            .selected()
            .and_then(|selected| items.iter().position(|item| &item.value == selected));
        self.items = items;
        self.rank();
        self.selected = selected
            .and_then(|item| self.results.iter().position(|result| result.item == item))
            .unwrap_or(0);
        self.scroll = self.scroll.min(self.results.len().saturating_sub(1));
    }

    /// Set pointer acceptance independently of keyboard Enter submission.
    pub fn set_mouse_activation(&mut self, activation: MouseActivation) {
        self.mouse_activation = activation;
    }

    /// Choose whether motion changes selection; disable for explicit-action lists.
    pub fn set_select_on_hover(&mut self, enabled: bool) {
        self.select_on_hover = enabled;
    }

    /// Change section presentation without changing query, caret or selection.
    pub fn set_layout(&mut self, layout: ListLayout) {
        self.layout = layout;
    }

    /// Override visible rows in original choice order without changing search or rank.
    ///
    /// `source_graphemes` maps each extended grapheme of sanitized `text` to an
    /// extended-grapheme index in the original sanitized search label. `None`,
    /// missing entries and out-of-range indices receive no fuzzy emphasis.
    /// Replacing choices clears these presentations. Query, caret, selection
    /// and pointer geometry are unchanged.
    /// Missing rows clear their override; excess rows are ignored.
    pub fn set_row_presentations(
        &mut self,
        rows: impl IntoIterator<Item = (String, Vec<Option<u32>>)>,
    ) {
        let mut rows = rows.into_iter();
        for item in &mut self.items {
            item.presentation = rows
                .next()
                .map(|(text, sources)| (window::clean(&text), sources));
        }
    }

    /// Derive content from current dimensions, including before the first paint.
    pub fn content_layout(&self, size: Size) -> ContentLayout {
        let cards = match self.layout {
            ListLayout::Inline => None,
            ListLayout::Cards { details } => crate::picker::PickerLayout::new(size, details),
            ListLayout::StackedCards => crate::picker::PickerLayout::stacked(size),
        };
        if let Some(layout) = cards {
            let mut results = layout.results;
            if self.input.error().is_some() {
                results.y += 1;
                results.height = results.height.saturating_sub(1);
            }
            return ContentLayout {
                title: layout.title,
                input: layout.input,
                results,
                details: layout.details,
            };
        }
        let heading = u16::from(self.title.is_some() && size.rows > 1);
        let row = |y| Rect {
            x: 0,
            y,
            width: size.cols,
            height: u16::from(y < size.rows),
        };
        let start = heading + 1 + u16::from(self.input.error().is_some());
        ContentLayout {
            title: if heading > 0 { row(0) } else { Rect::default() },
            input: row(heading),
            results: Rect {
                y: start.min(size.rows),
                height: size.rows.saturating_sub(start),
                ..row(0)
            },
            details: Rect::default(),
        }
    }

    /// Borrow visible values and row rectangles for caller-owned semantic annotations.
    /// Use the same dimensions as the preceding paint; rectangles are content-local.
    pub fn visible_items(&self, size: Size) -> impl Iterator<Item = (Rect, &T)> {
        self.results
            .iter()
            .skip(self.scroll)
            .take(if size.cols == 0 {
                0
            } else {
                self.capacity(size)
            })
            .enumerate()
            .map(move |(visible, result)| {
                (self.row_rect(size, visible), &self.items[result.item].value)
            })
    }

    /// A content heading, visible with either graphical or cell-based frames.
    /// The query keeps the only row when a viewport is too short for both.
    pub fn set_title(&mut self, title: &str) {
        if self.title.as_deref() != Some(title) {
            self.title = (!title.is_empty()).then(|| window::clean(title));
        }
    }

    /// Returns the selected matching value, or `None` when no result exists.
    pub fn selected(&self) -> Option<&T> {
        self.results
            .get(self.selected)
            .map(|result| &self.items[result.item].value)
    }

    /// Inserts bounded query text, reranking only when its content changes.
    pub fn paste(&mut self, text: &str) {
        if self.input.insert(text) {
            self.changed();
        }
    }

    /// All events belong to this focused widget; releases do not edit or accept.
    /// Shift and lock modifiers allow text; Ctrl/Alt/Super never insert text.
    pub fn handle_key(&mut self, event: &KeyEvent, size: Size) -> SearchListIntent {
        if event.action == KeyAction::Release {
            return SearchListIntent::None;
        }
        let modifiers = event.modifiers & 15;
        let edit = match (modifiers, event.key.as_str()) {
            (0, "Escape") => return SearchListIntent::Cancel,
            (0, "Enter") => {
                return if self.selected().is_some() {
                    SearchListIntent::Accept
                } else {
                    SearchListIntent::None
                };
            }
            (0, "ArrowUp") | (2, "KeyP") => {
                self.navigate(-1, size);
                return SearchListIntent::None;
            }
            (0, "ArrowDown") | (2, "KeyN") => {
                self.navigate(1, size);
                return SearchListIntent::None;
            }
            (0, "PageUp") => {
                self.navigate(-(self.capacity(size).max(1) as isize), size);
                return SearchListIntent::None;
            }
            (0, "PageDown") => {
                self.navigate(self.capacity(size).max(1) as isize, size);
                return SearchListIntent::None;
            }
            (0, "ArrowLeft") | (2, "KeyB") => Some(EditAction::Left),
            (0, "ArrowRight") | (2, "KeyF") => Some(EditAction::Right),
            (0, "Home") | (2, "KeyA") => Some(EditAction::Home),
            (0, "End") | (2, "KeyE") => Some(EditAction::End),
            (0, "Backspace") | (2, "KeyH") => Some(EditAction::Backspace),
            (0, "Delete") | (2, "KeyD") => Some(EditAction::Delete),
            (2, "KeyU") => Some(EditAction::Clear),
            _ => None,
        };
        if let Some(edit) = edit {
            if self.input.edit(edit) {
                self.changed();
            }
        } else if event.modifiers & 14 == 0 {
            self.paste(&event.text);
        }
        SearchListIntent::None
    }

    /// Coordinates are local CONTENT cells, excluding the caller's frame/title.
    /// Motion follows `set_select_on_hover`; left presses follow `set_mouse_activation`.
    /// Wheel events navigate results only while the pointer is in the list area.
    pub fn handle_mouse(&mut self, event: &MouseEvent, size: Size) -> SearchListIntent {
        let results = self.content_layout(size).results;
        if !event.x.is_finite()
            || !event.y.is_finite()
            || event.x < f32::from(results.x)
            || event.y < f32::from(results.y)
            || event.x >= f32::from(results.x + results.width)
            || event.y >= f32::from(results.y + results.height)
        {
            return SearchListIntent::None;
        }
        self.reveal(self.capacity(size));
        if event.action == MouseAction::Press {
            match event.button {
                MouseButton::WheelUp => {
                    self.navigate(-1, size);
                    return SearchListIntent::None;
                }
                MouseButton::WheelDown => {
                    self.navigate(1, size);
                    return SearchListIntent::None;
                }
                _ => {}
            }
        }
        let index = self.scroll + usize::from(event.y as u16 - results.y);
        if index < self.results.len()
            && ((event.action == MouseAction::Motion && self.select_on_hover)
                || (event.action == MouseAction::Press && event.button == MouseButton::Left))
        {
            self.selected = index;
            if event.action == MouseAction::Press
                && self.mouse_activation == MouseActivation::Accept
            {
                return SearchListIntent::Accept;
            }
        }
        SearchListIntent::None
    }

    /// Paints content and the query caret into a dedicated local surface.
    /// Call `apply_theme` afterward to apply semantic colors.
    pub fn paint(&mut self, surface: &mut UiSurface, size: Size) {
        self.reveal(self.capacity(size));
        // UiSurface retains text and styling. Remove only this widget's previous
        // emphasis before painting; semantic styling belongs to apply_theme.
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
        let mut lines = Vec::with_capacity(usize::from(size.rows));
        let mut cursor = None;
        let layout = self.content_layout(size);
        if layout.input.width > 0 && layout.input.height > 0 {
            if layout.title.height > 0 {
                let rect = layout.title;
                lines.push((rect, self.title.clone().unwrap_or_default(), false));
                self.roles.push(RoleSpan {
                    rect,
                    role: Role::Title,
                });
            }
            let prefix = if layout.input.width >= 3 { "> " } else { "" };
            let (visible, caret) = self
                .input
                .visible(usize::from(layout.input.width) - prefix.len());
            let input_rect = layout.input;
            lines.push((input_rect, format!("{prefix}{visible}"), false));
            self.roles.push(RoleSpan {
                rect: input_rect,
                role: if self.input.error().is_some() {
                    Role::Error
                } else {
                    Role::Accent
                },
            });
            cursor = Some(Cursor {
                x: input_rect.x
                    + (prefix.len() + caret).min(usize::from(input_rect.width - 1)) as u16,
                y: input_rect.y,
                visible: true,
                blinking: true,
                shape: CursorShape::Bar,
                color: None,
            });
            if let Some(error) = self
                .input
                .error()
                .filter(|_| layout.results.y > input_rect.y + 1 && layout.results.y <= size.rows)
            {
                let rect = Rect {
                    y: layout.results.y - 1,
                    x: layout.results.x,
                    width: layout.results.width,
                    height: 1,
                };
                lines.push((rect, error.to_owned(), false));
                self.roles.push(RoleSpan {
                    rect,
                    role: Role::Error,
                });
            }
            if self.results.is_empty() && self.capacity(size) > 0 {
                let rect = self.row_rect(size, 0);
                lines.push((rect, "No matches".to_owned(), false));
                self.roles.push(RoleSpan {
                    rect,
                    role: Role::Muted,
                });
            }
            for (visible, result) in self
                .results
                .iter()
                .skip(self.scroll)
                .take(self.capacity(size))
                .enumerate()
            {
                let rect = self.row_rect(size, visible);
                let y = rect.y;
                let item = &self.items[result.item];
                let display = item
                    .presentation
                    .as_ref()
                    .map_or(item.label.as_str(), |(text, _)| text.as_str());
                let mut label = window::fit(display, rect.width);
                let used = display_width(&label);
                label.extend(std::iter::repeat_n(' ', usize::from(rect.width) - used));
                lines.push((
                    self.row_rect(size, visible),
                    label,
                    self.scroll + visible == self.selected,
                ));
                let mut x = usize::from(rect.x);
                for (index, grapheme) in display.graphemes(true).enumerate() {
                    let width = unicode_width::UnicodeWidthStr::width(grapheme);
                    if x + width > usize::from(rect.x + rect.width) {
                        break;
                    }
                    let source = item
                        .presentation
                        .as_ref()
                        .map_or(Some(index as u32), |(_, map)| {
                            map.get(index).copied().flatten()
                        });
                    if source.is_some_and(|source| result.matches.binary_search(&source).is_ok()) {
                        self.match_cells
                            .extend((x..x + width).map(|column| (column as u16, y)));
                    }
                    x += width;
                }
            }
        }
        surface.paint(size, lines, cursor);
        for &(x, y) in &self.match_cells {
            let cell = &mut surface.frame.rows[usize::from(y)].cells[usize::from(x)];
            cell.style.bold = true;
            cell.style.underline = 1;
        }
    }

    /// Apply retained semantic styling and caller-owned visible-ink annotations.
    /// Foreground spans preserve row selection backgrounds and matching attributes.
    pub fn apply_theme(
        &self,
        surface: &mut UiSurface,
        theme: &Theme,
        foregrounds: &[ForegroundSpan],
        backgrounds: &[BackgroundSpan],
    ) {
        surface.apply_theme_with_colors(theme, Role::Panel, &self.roles, foregrounds, backgrounds);
    }

    fn row_rect(&self, size: Size, visible: usize) -> Rect {
        let results = self.content_layout(size).results;
        Rect {
            y: results.y + visible as u16,
            height: 1,
            ..results
        }
    }

    fn capacity(&self, size: Size) -> usize {
        let results = self.content_layout(size).results;
        if results.width == 0 {
            0
        } else {
            usize::from(results.height)
        }
    }

    fn navigate(&mut self, delta: isize, size: Size) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.results.len().saturating_sub(1));
        self.reveal(self.capacity(size));
    }

    fn reveal(&mut self, capacity: usize) {
        // No visible result rows must not discard the logical selected identity.
        let capacity = capacity.max(1);
        self.scroll = self.scroll.min(self.results.len().saturating_sub(capacity));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + capacity {
            self.scroll = self.selected + 1 - capacity;
        }
    }

    fn changed(&mut self) {
        self.rank();
        self.selected = 0;
        self.scroll = 0;
    }

    fn rank(&mut self) {
        let terms: Vec<_> = self
            .input
            .value
            .split_whitespace()
            .map(|term| {
                Atom::new(
                    term,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                )
            })
            .collect();
        self.results.clear();
        'candidate: for (index, item) in self.items.iter().enumerate() {
            let mut score = 0_u32;
            let mut matches = Vec::new();
            for term in &terms {
                let Some(value) = term.indices(
                    Utf32Str::Unicode(&item.chars),
                    &mut self.matcher,
                    &mut matches,
                ) else {
                    continue 'candidate;
                };
                score = score.saturating_add(u32::from(value));
            }
            matches.sort_unstable();
            matches.dedup();
            self.results.push(ResultRow {
                item: index,
                score,
                matches,
            });
        }
        self.results.sort_unstable_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.item.cmp(&right.item))
        });
    }
}

fn display_width(text: &str) -> usize {
    crate::text_width(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> Size {
        Size {
            cols,
            rows,
            cell_width: 8,
            cell_height: 16,
        }
    }

    fn key(name: &str, modifiers: u16, text: &str) -> KeyEvent {
        KeyEvent {
            key: name.to_owned(),
            modifiers,
            text: text.to_owned(),
            action: KeyAction::Press,
            unshifted_codepoint: 0,
        }
    }

    fn list() -> SearchList<u8> {
        SearchList::new(
            [
                (0, "Ocean Dark".to_owned()),
                (1, "Forest Light".to_owned()),
                (2, "Ocean Light".to_owned()),
                (3, "Amber".to_owned()),
                (4, "Forest Dark".to_owned()),
            ],
            Some(&1),
        )
    }

    fn mouse(x: f32, y: f32, action: MouseAction, button: MouseButton) -> MouseEvent {
        MouseEvent {
            x,
            y,
            action,
            button,
            modifiers: 0,
        }
    }

    #[test]
    fn row_presentation_maps_emphasis_without_changing_search_identity() {
        let mut list = SearchList::new([(0, "abc".to_owned())], Some(&0));
        list.paste("b");
        list.set_row_presentations([("界 b".to_owned(), vec![None, None, Some(1)])]);
        let mut surface = UiSurface::new(size(12, 4));
        list.paint(&mut surface, size(12, 4));
        assert_eq!(list.selected(), Some(&0));
        assert_eq!(list.input.value, "b");
        assert_eq!(list.match_cells, vec![(3, 1)]);
        list.replace_items([(0, "abc".to_owned())]);
        list.paint(&mut surface, size(12, 4));
        assert_eq!(list.match_cells, vec![(1, 1)]);
    }

    #[test]
    fn cards_use_exact_painted_rows_and_ignore_borders_after_resize() {
        let mut list = list();
        list.set_title("Choose");
        list.paste("o");
        list.handle_key(&key("ArrowLeft", 0, ""), size(100, 24));
        let query = list.input.value.clone();
        let caret = list.input.cursor;
        let selected = list.selected().copied();
        let mut surface = UiSurface::new(size(100, 24));
        for presentation in [
            ListLayout::Cards { details: false },
            ListLayout::Cards { details: true },
            ListLayout::StackedCards,
        ] {
            list.set_layout(presentation);
            for dimensions in [
                size(100, 24),
                size(60, 18),
                size(8, 14),
                size(84, 14),
                size(39, 7),
                size(1, 1),
                size(0, 0),
            ] {
                list.paint(&mut surface, dimensions);
                assert_eq!(list.input.value, query);
                assert_eq!(list.input.cursor, caret);
                assert_eq!(list.selected().copied(), selected);
                let content = list.content_layout(dimensions);
                for (rect, _) in list.visible_items(dimensions) {
                    assert_eq!(rect.x, content.results.x);
                    assert!(rect.y >= content.results.y);
                    assert!(rect.y < content.results.y + content.results.height);
                    assert_eq!(rect.width, content.results.width);
                    assert!(
                        surface.frame.rows[usize::from(rect.y)].cells
                            [usize::from(rect.x)..usize::from(rect.x + rect.width)]
                            .iter()
                            .any(|cell| !cell.text.trim().is_empty())
                    );
                }
                let cards = match presentation {
                    ListLayout::Cards { details } => {
                        crate::picker::PickerLayout::new(dimensions, details)
                    }
                    ListLayout::StackedCards => crate::picker::PickerLayout::stacked(dimensions),
                    ListLayout::Inline => None,
                };
                if let Some(cards) = cards {
                    let border = cards.cards()[1].0;
                    assert_eq!(
                        list.handle_mouse(
                            &mouse(
                                f32::from(border.x),
                                f32::from(border.y),
                                MouseAction::Press,
                                MouseButton::Left
                            ),
                            dimensions,
                        ),
                        SearchListIntent::None
                    );
                    assert_eq!(list.selected().copied(), selected);
                }
            }
        }
    }

    #[test]
    fn heading_keeps_pointer_rows_and_tiny_query_consistent() {
        let mut list = list();
        list.set_title("Choose a palette");
        let mut surface = UiSurface::new(size(24, 4));
        list.paint(&mut surface, size(24, 4));
        assert_eq!(surface.frame.cursor.y, 1);
        assert_eq!(
            list.handle_mouse(
                &mouse(0.0, 1.0, MouseAction::Press, MouseButton::Left),
                size(24, 4),
            ),
            SearchListIntent::None
        );
        assert_eq!(list.selected(), Some(&1));
        assert_eq!(
            list.handle_mouse(
                &mouse(0.0, 2.0, MouseAction::Press, MouseButton::Left),
                size(24, 4),
            ),
            SearchListIntent::Accept
        );
        assert_eq!(list.selected(), Some(&0));
        list.paint(&mut surface, size(24, 1));
        assert_eq!(surface.frame.cursor.y, 0);
        assert!(surface.frame.cursor.visible);
        assert_eq!(list.selected(), Some(&0));
    }

    #[test]
    fn initial_identity_and_fuzzy_filter_are_stable() {
        let mut list = list();
        assert_eq!(list.selected(), Some(&1));
        list.paste("oc lt");
        assert_eq!(list.selected(), Some(&2));
        assert_eq!(list.results.len(), 1);
        assert!(!list.results[0].matches.is_empty());
        let mut surface = UiSurface::new(size(20, 5));
        list.paint(&mut surface, size(20, 5));
        assert!(
            surface.frame.rows[1]
                .cells
                .iter()
                .any(|cell| cell.style.underline == 1)
        );
        assert_eq!(
            list.handle_key(&key("Enter", 0, ""), size(20, 5)),
            SearchListIntent::Accept
        );
    }
    #[test]
    fn joined_grapheme_labels_keep_match_underlines_and_selection_padding_aligned() {
        let mut list = SearchList::new([(0, "👩‍💻x".to_owned())], Some(&0));
        list.paste("x");
        let mut surface = UiSurface::new(size(4, 2));
        list.paint(&mut surface, size(4, 2));
        let cells = &surface.frame.rows[1].cells;
        assert_eq!(cells[0].text, "👩‍💻");
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].text, "x");
        assert_eq!(cells[2].style.underline, 1);
        assert_eq!(cells[0].style.underline, 0);
        assert_eq!(cells[3].text, " ");
        assert!(cells.iter().all(|cell| cell.style.inverse));
        assert_eq!(surface.frame.cursor.x, 3);
    }

    #[test]
    fn result_paging_and_control_navigation_never_edit_query() {
        let mut list = list();
        list.handle_key(&key("PageDown", 0, ""), size(20, 3));
        assert_eq!(list.selected(), Some(&3));
        list.handle_key(&key("KeyP", 2, "p"), size(20, 3));
        assert_eq!(list.selected(), Some(&2));
        list.handle_key(&key("KeyN", 2, "n"), size(20, 3));
        assert_eq!(list.selected(), Some(&3));
        list.handle_key(&key("PageUp", 0, ""), size(20, 3));
        assert_eq!(list.selected(), Some(&1));
        assert!(list.input.value.is_empty());
    }

    #[test]
    fn no_match_cannot_accept_and_clear_restores_input_order() {
        let mut list = list();
        list.paste("unmatchable");
        assert_eq!(list.selected(), None);
        assert_eq!(
            list.handle_key(&key("Enter", 0, ""), size(20, 4)),
            SearchListIntent::None
        );
        let mut surface = UiSurface::new(size(20, 4));
        list.paint(&mut surface, size(20, 4));
        assert_eq!(surface.frame.rows[1].cells[0].text, "N");
        list.handle_key(&key("KeyU", 2, ""), size(20, 4));
        assert_eq!(list.selected(), Some(&0));
        assert_eq!(
            list.handle_key(&key("Escape", 0, ""), size(20, 4)),
            SearchListIntent::Cancel
        );
    }

    #[test]
    fn unicode_editing_paste_bound_and_input_cursor_are_safe() {
        let mut list = SearchList::new([(0, "界e\u{301}".to_owned())], Some(&0));
        list.paste("界e\u{301}");
        assert_eq!(list.selected(), Some(&0));
        list.handle_key(&key("Backspace", 0, ""), size(8, 3));
        assert_eq!(list.input.value, "界");
        list.handle_key(&key("Home", 0, ""), size(8, 3));
        let mut surface = UiSurface::new(size(8, 3));
        list.paint(&mut surface, size(8, 3));
        assert_eq!(surface.frame.cursor.x, 2);
        list.handle_key(&key("End", 0, ""), size(8, 3));
        list.paint(&mut surface, size(8, 3));
        assert_eq!(surface.frame.cursor.x, 4);
        list.paste(&"x".repeat(QUERY_LIMIT));
        assert_eq!(list.input.value, "界");
        assert_eq!(list.input.cursor, "界".len());
        assert!(list.input.error().is_some());
        list.handle_key(&key("KeyU", 2, ""), size(8, 3));
        list.paste("a\nb\t\u{1b}");
        assert_eq!(list.input.value, "a b  ");
    }

    #[test]
    fn tiny_resize_preserves_query_and_selection_with_bounded_cursor() {
        let mut list = list();
        list.paste("forest");
        list.handle_key(&key("ArrowDown", 0, ""), size(20, 4));
        let selected = list.selected().copied();
        let mut surface = UiSurface::new(size(20, 4));
        for dimensions in [
            size(0, 0),
            size(0, 1),
            size(1, 0),
            size(1, 1),
            size(2, 2),
            size(20, 4),
        ] {
            list.paint(&mut surface, dimensions);
            assert_eq!(list.selected().copied(), selected);
            assert_eq!(list.input.value, "forest");
            assert_eq!(
                surface.frame.cursor.visible,
                dimensions.cols > 0 && dimensions.rows > 0
            );
            if surface.frame.cursor.visible {
                assert!(surface.frame.cursor.x < dimensions.cols);
                assert_eq!(surface.frame.cursor.y, 0);
            }
        }
    }

    #[test]
    fn mouse_coordinates_are_content_local_and_clipped() {
        let mut list = list();
        let dimensions = size(12, 4);
        list.handle_mouse(
            &mouse(2.0, 3.0, MouseAction::Motion, MouseButton::None),
            dimensions,
        );
        assert_eq!(list.selected(), Some(&2));
        for (x, y) in [
            (0.0, 0.0),
            (-0.5, 2.0),
            (12.0, 1.0),
            (0.0, 4.0),
            (f32::NAN, 1.0),
        ] {
            assert_eq!(
                list.handle_mouse(
                    &mouse(x, y, MouseAction::Press, MouseButton::Left),
                    dimensions
                ),
                SearchListIntent::None
            );
            assert_eq!(list.selected(), Some(&2));
        }
        list.handle_mouse(
            &mouse(1.0, 1.0, MouseAction::Press, MouseButton::WheelDown),
            dimensions,
        );
        assert_eq!(list.selected(), Some(&3));
        assert_eq!(
            list.handle_mouse(
                &mouse(1.0, 2.0, MouseAction::Press, MouseButton::Left),
                dimensions
            ),
            SearchListIntent::Accept
        );
        assert_eq!(list.selected(), Some(&2));
    }

    #[test]
    fn retained_repaints_keep_and_clear_only_current_match_emphasis() {
        let mut list = SearchList::new([(0, "Ocean".to_owned())], Some(&0));
        let dimensions = size(12, 3);
        let mut surface = UiSurface::new(dimensions);
        let theme = Theme::default();
        list.paste("oc");
        for _ in 0..2 {
            list.paint(&mut surface, dimensions);
            list.apply_theme(&mut surface, &theme, &[], &[]);
            assert_eq!(surface.frame.rows[1].cells[0].style.underline, 1);
            assert_eq!(surface.frame.rows[1].cells[1].style.underline, 1);
            assert_eq!(surface.frame.rows[1].cells[2].style.underline, 0);
        }
        list.handle_key(&key("KeyU", 2, ""), dimensions);
        list.paint(&mut surface, dimensions);
        list.apply_theme(&mut surface, &theme, &[], &[]);
        assert!(
            surface.frame.rows[1]
                .cells
                .iter()
                .all(|cell| cell.style.underline == 0)
        );
    }

    #[test]
    fn empty_rows_and_key_releases_do_not_accept() {
        let mut list = SearchList::<u8>::new([], None);
        assert_eq!(list.selected(), None);
        assert_eq!(
            list.handle_mouse(
                &mouse(1.0, 1.0, MouseAction::Press, MouseButton::Left),
                size(10, 3)
            ),
            SearchListIntent::None
        );
        let mut event = key("Escape", 0, "");
        event.action = KeyAction::Release;
        assert_eq!(list.handle_key(&event, size(10, 3)), SearchListIntent::None);
        list.paste("界");
        let mut surface = UiSurface::new(size(1, 1));
        list.paint(&mut surface, size(1, 1));
        assert!(surface.frame.cursor.visible);
        assert_eq!(surface.frame.cursor.x, 0);
    }

    #[test]
    fn replacing_choices_keeps_query_and_exact_selection_across_reorder_and_rename() {
        let mut list = list();
        let dimensions = size(40, 6);
        list.paste("forest");
        list.handle_key(&key("ArrowDown", 0, ""), dimensions);
        assert_eq!(list.selected(), Some(&4));
        list.replace_items([
            (5, "Ocean new".into()),
            (4, "Forest Dark renamed".into()),
            (1, "Forest Light".into()),
        ]);
        let mut surface = UiSurface::new(dimensions);
        list.paint(&mut surface, dimensions);
        assert_eq!(list.selected(), Some(&4));
        assert!(list.visible_items(dimensions).all(|(_, value)| *value != 5));
        list.replace_items([(5, "Forest new".into()), (0, "Ocean".into())]);
        list.paint(&mut surface, dimensions);
        assert_eq!(list.selected(), Some(&5));
        assert_eq!(
            list.visible_items(dimensions)
                .map(|(_, value)| *value)
                .collect::<Vec<_>>(),
            [5]
        );
    }

    #[test]
    fn selection_only_pointer_rows_use_painted_geometry_but_enter_still_accepts() {
        let mut list = list();
        list.set_title("Recover");
        list.set_mouse_activation(MouseActivation::Select);
        list.set_select_on_hover(false);
        let dimensions = size(25, 4);
        let mut surface = UiSurface::new(dimensions);
        list.paint(&mut surface, dimensions);
        let (rect, value) = list.visible_items(dimensions).next().unwrap();
        let expected = *value;
        assert_eq!(rect.y, 2);
        assert_eq!(
            list.handle_mouse(
                &mouse(
                    f32::from(rect.x),
                    f32::from(rect.y),
                    MouseAction::Motion,
                    MouseButton::None
                ),
                dimensions
            ),
            SearchListIntent::None
        );
        assert_eq!(list.selected(), Some(&1));
        assert_eq!(
            list.handle_mouse(
                &mouse(
                    f32::from(rect.x),
                    f32::from(rect.y),
                    MouseAction::Press,
                    MouseButton::Left
                ),
                dimensions
            ),
            SearchListIntent::None
        );
        assert_eq!(list.selected(), Some(&expected));
        assert_eq!(
            list.handle_key(&key("Enter", 0, ""), dimensions),
            SearchListIntent::Accept
        );
    }
}
