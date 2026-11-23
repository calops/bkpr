//! Bounded grapheme-safe editor; applications own key bindings and byte limits.
//! Oversized insertions are rejected atomically, never truncated.

use std::borrow::Cow;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Grapheme editing or caret movement within a text value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Moves to the preceding grapheme boundary.
    Left,
    /// Moves to the following grapheme boundary.
    Right,
    /// Moves to the start of the current logical line.
    Home,
    /// Moves to the end of the current logical line.
    End,
    /// Moves backward over whitespace and one word.
    WordLeft,
    /// Moves forward over whitespace and one word.
    WordRight,
    /// Removes the preceding grapheme.
    Backspace,
    /// Removes the following grapheme.
    Delete,
    /// Removes whitespace and one word before the caret, staying on the current line.
    DeletePreviousWord,
    /// Removes text from the start of the current line to the caret.
    DeleteToLineStart,
    /// Removes text from the caret to the end of the current line.
    DeleteToLineEnd,
    /// Removes all content and clears any insertion error.
    Clear,
}

/// UTF-8 byte-bounded editor with a grapheme-aligned caret.
#[derive(Clone, Debug)]
pub struct TextInput {
    /// Edited content; direct writes bypass sanitization and the byte bound.
    pub value: String,
    /// UTF-8 byte offset at a grapheme boundary, not a display column.
    pub cursor: usize,
    limit: usize,
    multiline: bool,
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
                multiline: false,
                error: None,
            };
        }
        let mut input = Self {
            value: String::new(),
            cursor: 0,
            limit,
            multiline: false,
            error: None,
        };
        input.insert(&value);
        input
    }
    /// Creates an empty editor accepting line feeds in addition to printable text.
    /// Other controls are replaced with spaces, as in a single-line field.
    pub fn multiline(limit: usize) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            limit,
            multiline: true,
            error: None,
        }
    }
    /// Returns the last rejected-insertion error, cleared by successful edits.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Replaces controls with spaces before checking the byte bound. On failure
    /// both value and cursor remain exactly unchanged. Returns content change.
    pub fn insert(&mut self, text: &str) -> bool {
        self.insert_with_range(text).is_some()
    }

    /// Inserts at the caret and returns the exact inserted byte range in the new
    /// value. An empty or rejected insertion returns `None` and leaves the caret
    /// and value unchanged.
    pub fn insert_with_range(&mut self, text: &str) -> Option<Range<usize>> {
        let cursor = self.normalized_cursor();
        self.replace_range(cursor..cursor, text)
    }

    /// Replaces a UTF-8 byte range, applying the same sanitization and byte bound
    /// as insertion. Returns the new byte range before caret realignment, or
    /// `None` for a rejected or empty edit.
    ///
    /// # Panics
    ///
    /// Panics if an accepted replacement uses an invalid UTF-8 byte range.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) -> Option<Range<usize>> {
        let remaining = self
            .limit
            .saturating_sub(self.value.len().saturating_sub(range.len()));
        let Some(text) = printable(text, remaining, self.multiline) else {
            self.error = Some(format!(
                "Input exceeds the {}-byte limit; insertion rejected",
                self.limit
            ));
            return None;
        };
        if range.is_empty() && text.is_empty() {
            return None;
        }
        Some(self.replace_bytes(range, &text))
    }

    /// Returns content change only; navigation can move the cursor while false.
    pub fn edit(&mut self, action: EditAction) -> bool {
        self.edit_with_range(action).is_some()
    }

    /// Applies an action and returns the exact removed byte range in the old
    /// value. Navigation and empty deletions return `None`.
    pub fn edit_with_range(&mut self, action: EditAction) -> Option<Range<usize>> {
        self.cursor = self.normalized_cursor();
        let at = self.cursor;
        let mut removed = None;
        match action {
            EditAction::Home => self.cursor = self.line_start(),
            EditAction::End => self.cursor = self.line_end(),
            EditAction::Left => self.cursor = self.previous_boundary(),
            EditAction::Right => self.cursor = self.next_boundary(),
            EditAction::WordLeft => self.cursor = self.word_start(0),
            EditAction::WordRight => {
                self.cursor = word_edge(
                    self.value[at..]
                        .grapheme_indices(true)
                        .map(|(i, g)| (at + i, g)),
                    self.value.len(),
                );
            }
            EditAction::Backspace => removed = Some(self.previous_boundary()..at),
            EditAction::Delete => removed = Some(at..self.next_boundary()),
            EditAction::DeletePreviousWord => {
                removed = Some(self.word_start(self.line_start())..at);
            }
            EditAction::DeleteToLineStart => removed = Some(self.line_start()..at),
            EditAction::DeleteToLineEnd => removed = Some(at..self.line_end()),
            EditAction::Clear => {
                self.error = None;
                removed = Some(0..self.value.len());
            }
        }
        let removed = removed.filter(|range| !range.is_empty())?;
        self.replace_bytes(removed.clone(), "");
        Some(removed)
    }

    fn replace_bytes(&mut self, range: Range<usize>, text: &str) -> Range<usize> {
        let inserted = range.start..range.start + text.len();
        self.value.replace_range(range, text);
        self.cursor = inserted.end;
        self.joined_cursor();
        self.error = None;
        inserted
    }

    fn line_start(&self) -> usize {
        self.value[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |i| self.cursor + i)
    }

    fn word_start(&self, first: usize) -> usize {
        word_edge(
            self.value[first..self.cursor]
                .grapheme_indices(true)
                .rev()
                .map(|(i, g)| (first + i + g.len(), g)),
            first,
        )
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
    fn normalized_cursor(&self) -> usize {
        if self.cursor >= self.value.len() {
            return self.value.len();
        }
        self.value
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|&i| i <= self.cursor)
            .last()
            .unwrap_or(0)
    }
    fn joined_cursor(&mut self) {
        if self.cursor == self.value.len() {
            return;
        }
        self.cursor = self
            .value
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= self.cursor)
            .unwrap_or(self.value.len());
    }
    /// Returns a single-line grapheme-aligned horizontal viewport and caret cell column.
    /// Reserves a cell for the caret, including at the end of a full-width value.
    pub fn visible(&self, columns: usize) -> (String, usize) {
        if columns == 0 {
            return (String::new(), 0);
        }
        let cursor = self.normalized_cursor();
        let mut start = cursor;
        let mut caret = 0;
        for (offset, grapheme) in self.value[..cursor].grapheme_indices(true).rev() {
            let width = unicode_width::UnicodeWidthStr::width(grapheme);
            if caret + width >= columns {
                break;
            }
            start = offset;
            caret += width;
        }
        let mut text = String::new();
        let mut used = 0;
        for grapheme in self.value[start..].graphemes(true) {
            let width = unicode_width::UnicodeWidthStr::width(grapheme);
            if used + width > columns {
                break;
            }
            text.push_str(grapheme);
            used += width;
        }
        (text, caret)
    }
}

fn word_edge<'a>(graphemes: impl Iterator<Item = (usize, &'a str)>, fallback: usize) -> usize {
    let mut seen_word = false;
    for (boundary, grapheme) in graphemes {
        if grapheme.starts_with(char::is_whitespace) {
            if seen_word {
                return boundary;
            }
        } else {
            seen_word = true;
        }
    }
    fallback
}

fn printable(text: &str, limit: usize, multiline: bool) -> Option<Cow<'_, str>> {
    let replace = |c: char| c.is_control() && !(multiline && c == '\n');
    if text.chars().any(replace) {
        // Reject before allocating a sanitized copy of an arbitrary-size paste.
        let mut bytes = 0;
        for c in text.chars() {
            bytes += if replace(c) { 1 } else { c.len_utf8() };
            if bytes > limit {
                return None;
            }
        }
        Some(Cow::Owned(
            text.chars()
                .map(|c| if replace(c) { ' ' } else { c })
                .collect(),
        ))
    } else {
        (text.len() <= limit).then_some(Cow::Borrowed(text))
    }
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
            assert!(crate::text_width(&text) <= columns);
            if columns > 0 {
                assert!(caret < columns);
            }
        }
        let mut input = TextInput::new("abcd".into(), LIMIT);
        input.edit(EditAction::Left);
        assert_eq!(input.visible(3), ("bcd".into(), 2));
    }
    #[test]
    fn caret_and_scroll_reserve_whole_grapheme_cell_widths() {
        for (glyph, width) in [
            ("👩‍💻", 2),
            ("✈\u{fe0f}", 2),
            ("🇫🇷", 2),
            ("e\u{301}", 1),
            ("界", 2),
            ("\u{17d8}", 3),
        ] {
            let mut input = TextInput::new(glyph.into(), LIMIT);
            assert_eq!(input.visible(width + 1), (glyph.into(), width));
            assert_eq!(input.visible(width), (String::new(), 0));
            assert!(input.insert("x"));
            assert_eq!(input.visible(width + 2), (format!("{glyph}x"), width + 1));
            input.edit(EditAction::Left);
            assert_eq!(input.visible(width + 1), (format!("{glyph}x"), width));
            assert_eq!(input.visible(width), ("x".into(), 0));
            input.edit(EditAction::Backspace);
            assert_eq!(input.value, "x");
            assert_eq!(input.cursor, 0);
        }
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

    #[test]
    fn multiline_commands_use_logical_lines_and_report_exact_removed_bytes() {
        let mut input = TextInput::multiline(LIMIT);
        let before = "first\n";
        let prefix = "界 ";
        let word = "👩‍💻 \u{2003}";
        let start = before.len();
        let word_start = start + prefix.len();
        let word_end = word_start + word.len();
        assert!(input.insert(&format!("{before}{prefix}{word}tail\nlast")));

        input.cursor = word_end;
        assert_eq!(
            input.edit_with_range(EditAction::DeletePreviousWord),
            Some(word_start..word_end)
        );
        assert_eq!(input.value, "first\n界 tail\nlast");
        assert_eq!(input.cursor, word_start);
        assert_eq!(
            input.edit_with_range(EditAction::DeleteToLineStart),
            Some(start..word_start)
        );
        assert_eq!(input.cursor, start);
        assert_eq!(input.edit_with_range(EditAction::DeletePreviousWord), None);
        assert_eq!(input.edit_with_range(EditAction::End), None);
        assert_eq!(input.cursor, start + "tail".len());
        assert_eq!(input.edit_with_range(EditAction::DeleteToLineEnd), None);
        assert_eq!(input.edit_with_range(EditAction::Home), None);
        assert_eq!(input.cursor, start);
        assert_eq!(
            input.edit_with_range(EditAction::DeleteToLineEnd),
            Some(start..start + "tail".len())
        );
        assert_eq!(input.value, "first\n\nlast");
    }

    #[test]
    fn word_motion_and_inserted_ranges_distinguish_bytes_from_graphemes() {
        let mut input = TextInput::multiline(LIMIT);
        assert!(input.insert("👩‍💻\n\u{301}字"));
        input.edit(EditAction::Home);
        let at = "👩‍💻\n".len();
        assert_eq!(input.insert_with_range("e"), Some(at..at + 1));
        assert_eq!(input.cursor, at + "e\u{301}".len());
        assert!(!input.edit(EditAction::WordLeft));
        assert_eq!(input.cursor, at);
        assert!(!input.edit(EditAction::WordLeft));
        assert_eq!(input.cursor, 0);
        assert!(!input.edit(EditAction::WordRight));
        assert_eq!(input.cursor, "👩‍💻".len());
        assert!(!input.edit(EditAction::WordRight));
        assert_eq!(input.cursor, input.value.len());
    }
}
