//! Bounded single-line, grapheme-safe editor; applications own key bindings and byte limits.
//! Oversized insertions are rejected atomically, never truncated.

use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;
/// Grapheme editing or caret movement within a single-line value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Moves to the preceding grapheme boundary.
    Left,
    /// Moves to the following grapheme boundary.
    Right,
    /// Moves to the start of the value.
    Home,
    /// Moves to the end of the value.
    End,
    /// Removes the preceding grapheme.
    Backspace,
    /// Removes the following grapheme.
    Delete,
    /// Removes all content and clears any insertion error.
    Clear,
}

/// Single-line editor with a UTF-8 byte bound and grapheme-aligned caret.
#[derive(Clone, Debug)]
pub struct TextInput {
    /// Edited content; direct writes bypass sanitization and the byte bound.
    pub value: String,
    /// UTF-8 byte offset at a grapheme boundary, not a display column.
    pub cursor: usize,
    limit: usize,
    error: Option<String>,
}
impl TextInput {
    /// Creates an editor with the caret at the end, replacing controls with spaces.
    /// If sanitized content exceeds `limit` bytes, starts empty with an error.
    pub fn new(value: String, limit: usize) -> Self {
        if value.len() <= limit && !value.chars().any(char::is_control) {
            let cursor = value.len();
            return Self {
                value,
                cursor,
                limit,
                error: None,
            };
        }
        let mut input = Self {
            value: String::new(),
            cursor: 0,
            limit,
            error: None,
        };
        input.insert(&value);
        input
    }
    /// Returns the last rejected-insertion error, cleared by successful edits.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Replaces controls with spaces before checking the byte bound. On failure
    /// both value and cursor remain exactly unchanged. Returns content change.
    pub fn insert(&mut self, text: &str) -> bool {
        let Some(text) = printable(text, self.limit.saturating_sub(self.value.len())) else {
            self.error = Some(format!(
                "Input exceeds the {}-byte limit; insertion rejected",
                self.limit
            ));
            return false;
        };
        if text.is_empty() {
            return false;
        }
        self.normalize_cursor();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.joined_cursor();
        self.error = None;
        true
    }
    /// Returns content change only; navigation can move the cursor while false.
    pub fn edit(&mut self, action: EditAction) -> bool {
        self.normalize_cursor();
        let mut changed = false;
        match action {
            EditAction::Home => self.cursor = 0,
            EditAction::End => self.cursor = self.value.len(),
            EditAction::Left => self.cursor = self.previous_boundary(),
            EditAction::Right => self.cursor = self.next_boundary(),
            EditAction::Backspace if self.cursor > 0 => {
                let previous = self.previous_boundary();
                self.value.replace_range(previous..self.cursor, "");
                self.cursor = previous;
                changed = true;
            }
            EditAction::Delete if self.cursor < self.value.len() => {
                let next = self.next_boundary();
                self.value.replace_range(self.cursor..next, "");
                changed = true;
            }
            EditAction::Clear => {
                changed = !self.value.is_empty();
                self.value.clear();
                self.cursor = 0;
                self.error = None;
            }
            _ => {}
        }
        if changed {
            self.joined_cursor();
            self.error = None;
        }
        changed
    }
    fn previous_boundary(&self) -> usize {
        self.value[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next_boundary(&self) -> usize {
        self.value[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.value.len(), |g| self.cursor + g.len())
    }
    fn normalize_cursor(&mut self) {
        if self.cursor >= self.value.len() {
            self.cursor = self.value.len();
            return;
        }
        self.cursor = self
            .value
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|&i| i <= self.cursor)
            .last()
            .unwrap_or(0);
    }
    fn joined_cursor(&mut self) {
        self.cursor = self
            .value
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= self.cursor)
            .unwrap_or(self.value.len());
    }
    /// Returns a grapheme-aligned horizontal viewport and caret cell column.
    /// Reserves a cell for the caret, including at the end of a full-width value.
    pub fn visible(&self, columns: usize) -> (String, usize) {
        if columns == 0 {
            return (String::new(), 0);
        }
        let cursor = if self.cursor >= self.value.len() {
            self.value.len()
        } else {
            self.value
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .take_while(|&i| i <= self.cursor)
                .last()
                .unwrap_or(0)
        };
        let mut start = cursor;
        let mut caret = 0;
        for (offset, grapheme) in self.value[..cursor].grapheme_indices(true).rev() {
            let width = display_width(grapheme);
            if caret + width >= columns {
                break;
            }
            start = offset;
            caret += width;
        }
        let mut text = String::new();
        let mut used = 0;
        for grapheme in self.value[start..].graphemes(true) {
            let width = display_width(grapheme);
            if used + width > columns {
                break;
            }
            text.push_str(grapheme);
            used += width;
        }
        (text, caret)
    }
}
fn printable(text: &str, limit: usize) -> Option<Cow<'_, str>> {
    if text.chars().any(char::is_control) {
        // Reject before allocating a sanitized copy of an arbitrary-size paste.
        let mut bytes = 0;
        for c in text.chars() {
            bytes += if c.is_control() { 1 } else { c.len_utf8() };
            if bytes > limit {
                return None;
            }
        }
        Some(Cow::Owned(
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        ))
    } else {
        (text.len() <= limit).then_some(Cow::Borrowed(text))
    }
}
fn display_width(text: &str) -> usize {
    // UiSurface uses this scalar width model, not a competing terminal parser.
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    const LIMIT: usize = 64;
    #[test]
    fn oversized_insert_preserves_content_and_cursor_atomically() {
        let mut input = TextInput::new("existing界".into(), LIMIT);
        input.edit(EditAction::Left);
        let original = (input.value.clone(), input.cursor);
        assert!(!input.insert(&"界".repeat(LIMIT)));
        assert_eq!((&input.value, input.cursor), (&original.0, original.1));
        assert!(input.error().is_some());
        assert!(input.insert("!"));
        assert!(input.error().is_none());
    }
    #[test]
    fn unicode_editing_uses_graphemes_and_navigation_is_not_a_content_change() {
        let mut input = TextInput::new("e\u{301}👩‍💻界".into(), LIMIT);
        assert!(!input.edit(EditAction::Left));
        assert!(input.edit(EditAction::Backspace));
        assert_eq!(input.value, "e\u{301}界");
        assert!(input.edit(EditAction::Delete));
        assert_eq!(input.value, "e\u{301}");
        assert!(input.edit(EditAction::Backspace));
        assert_eq!(input.value, "");
        input.insert("a");
        input.edit(EditAction::Home);
        input.insert("e\u{301}");
        assert_eq!(input.cursor, "e\u{301}".len());
    }
    #[test]
    fn viewport_keeps_caret_visible_without_splitting_graphemes() {
        let input = TextInput::new("start/界e\u{301}👩‍💻end".into(), LIMIT);
        for columns in 0..12 {
            let (text, caret) = input.visible(columns);
            assert!(display_width(&text) <= columns);
            if columns > 0 {
                assert!(caret < columns);
            }
        }
        let mut input = TextInput::new("abcd".into(), LIMIT);
        input.edit(EditAction::Left);
        assert_eq!(input.visible(3), ("bcd".into(), 2));
    }
    #[test]
    fn controls_never_escape_and_exact_limit_is_accepted() {
        let mut input = TextInput::new("a\n\u{1b}b".into(), LIMIT);
        assert_eq!(input.value, "a  b");
        input.edit(EditAction::Clear);
        assert!(input.insert(&"x".repeat(LIMIT)));
        assert!(!input.insert("y"));
        assert_eq!(input.value.len(), LIMIT);
    }
}
