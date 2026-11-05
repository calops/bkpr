//! Retained fuzzy single-list content, independent of application identities and palettes.
//! The caller owns the outer window, title, placement, and cancellation rollback.

use crate::{EditAction, Rect, Role, RoleSpan, TextInput, Theme, UiSurface, window};
use bkpr_protocol::{
    Cursor, CursorShape, KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent, Size,
};
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Queries are bounded in UTF-8 bytes; oversized insertions are rejected atomically.
const QUERY_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchListIntent {
    None,
    Cancel,
    Accept,
}

struct Item<T> {
    value: T,
    label: String,
    // Nucleo indexes graphemes, rather than UTF-8 bytes or terminal columns.
    chars: Vec<char>,
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
}

impl<T: Clone + Eq> SearchList<T> {
    pub fn new(items: impl IntoIterator<Item = (T, String)>, initial: &T) -> Self {
        let items: Vec<_> = items
            .into_iter()
            .map(|(value, label)| {
                let label = window::clean(&label);
                let chars = label
                    .graphemes(true)
                    .filter_map(|grapheme| grapheme.chars().next())
                    .collect();
                Item {
                    value,
                    label,
                    chars,
                }
            })
            .collect();
        let selected = items
            .iter()
            .position(|item| &item.value == initial)
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
        };
        list.rank();
        list
    }

    /// A content heading, visible with either graphical or cell-based frames.
    /// The query keeps the only row when a viewport is too short for both.
    pub fn set_title(&mut self, title: &str) {
        if self.title.as_deref() != Some(title) {
            self.title = (!title.is_empty()).then(|| window::clean(title));
        }
    }

    pub fn selected(&self) -> Option<&T> {
        self.results
            .get(self.selected)
            .map(|result| &self.items[result.item].value)
    }

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
    /// Motion selects immediately; a left press selects and accepts a valid row.
    /// Wheel events navigate results only while the pointer is in the list area.
    pub fn handle_mouse(&mut self, event: &MouseEvent, size: Size) -> SearchListIntent {
        if !event.x.is_finite()
            || !event.y.is_finite()
            || event.x < 0.0
            || event.y < f32::from(self.list_start(size))
            || event.x >= f32::from(size.cols)
            || event.y >= f32::from(size.rows)
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
        let index = self.scroll + usize::from(event.y as u16 - self.list_start(size));
        if index < self.results.len()
            && (event.action == MouseAction::Motion
                || (event.action == MouseAction::Press && event.button == MouseButton::Left))
        {
            self.selected = index;
            if event.action == MouseAction::Press {
                return SearchListIntent::Accept;
            }
        }
        SearchListIntent::None
    }

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
        if size.cols > 0 && size.rows > 0 {
            let input_y = self.input_row(size);
            if input_y > 0 {
                let rect = Rect {
                    x: 0,
                    y: 0,
                    width: size.cols,
                    height: 1,
                };
                lines.push((rect, self.title.as_ref().unwrap().clone(), false));
                self.roles.push(RoleSpan {
                    rect,
                    role: Role::Accent,
                });
            }
            let prefix = if size.cols >= 3 { "> " } else { "" };
            let (visible, caret) = self.input.visible(usize::from(size.cols) - prefix.len());
            let input_rect = Rect {
                x: 0,
                y: input_y,
                width: size.cols,
                height: 1,
            };
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
                x: (prefix.len() + caret).min(usize::from(size.cols - 1)) as u16,
                y: input_y,
                visible: true,
                blinking: true,
                shape: CursorShape::Bar,
                color: None,
            });
            if let Some(error) = self.input.error().filter(|_| size.rows > input_y + 1) {
                let rect = Rect {
                    y: input_y + 1,
                    ..input_rect
                };
                lines.push((rect, error.to_owned(), false));
                self.roles.push(RoleSpan {
                    rect,
                    role: Role::Error,
                });
            }
            if self.results.is_empty() && self.capacity(size) > 0 {
                let rect = Rect {
                    y: self.list_start(size),
                    ..input_rect
                };
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
                let y = self.list_start(size) + visible as u16;
                let item = &self.items[result.item];
                let mut label = window::fit(&item.label, size.cols);
                let used = display_width(&label);
                // Pad selection through the full row even without an RGB theme.
                label.extend(std::iter::repeat_n(' ', usize::from(size.cols) - used));
                lines.push((
                    Rect {
                        x: 0,
                        y,
                        width: size.cols,
                        height: 1,
                    },
                    label,
                    self.scroll + visible == self.selected,
                ));
                let mut x = 0;
                for (index, grapheme) in item.label.graphemes(true).enumerate() {
                    let width = display_width(grapheme);
                    if x + width > usize::from(size.cols) {
                        break;
                    }
                    if result.matches.binary_search(&(index as u32)).is_ok() {
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

    pub fn apply_theme(&self, surface: &mut UiSurface, theme: &Theme) {
        surface.apply_theme(theme, Role::Panel, &self.roles);
    }

    fn input_row(&self, size: Size) -> u16 {
        u16::from(self.title.is_some() && size.rows > 1)
    }

    fn list_start(&self, size: Size) -> u16 {
        self.input_row(size) + 1 + u16::from(self.input.error().is_some())
    }

    fn capacity(&self, size: Size) -> usize {
        usize::from(size.rows.saturating_sub(self.list_start(size)))
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
    // Match the scalar-width UiSurface renderer, including wide continuation cells.
    text.chars()
        .map(|character| UnicodeWidthChar::width(character).unwrap_or(0))
        .sum()
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
            &1,
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
        let mut list = SearchList::new([(0, "界e\u{301}".to_owned())], &0);
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
        let mut list = SearchList::new([(0, "Ocean".to_owned())], &0);
        let dimensions = size(12, 3);
        let mut surface = UiSurface::new(dimensions);
        let theme = Theme::default();
        list.paste("oc");
        for _ in 0..2 {
            list.paint(&mut surface, dimensions);
            list.apply_theme(&mut surface, &theme);
            assert_eq!(surface.frame.rows[1].cells[0].style.underline, 1);
            assert_eq!(surface.frame.rows[1].cells[1].style.underline, 1);
            assert_eq!(surface.frame.rows[1].cells[2].style.underline, 0);
        }
        list.handle_key(&key("KeyU", 2, ""), dimensions);
        list.paint(&mut surface, dimensions);
        list.apply_theme(&mut surface, &theme);
        assert!(
            surface.frame.rows[1]
                .cells
                .iter()
                .all(|cell| cell.style.underline == 0)
        );
    }

    #[test]
    fn empty_rows_and_key_releases_do_not_accept() {
        let mut list = SearchList::<u8>::new([], &0);
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
}
