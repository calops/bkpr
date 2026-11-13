//! Prepared, application-independent keycap groups for shortcut guides.
//!
//! A member is one operation's alias alternatives. Members are separated by
//! commas; aliases by slashes. The caller retains the operations and supplies
//! styling for the returned terminal-cell ranges. Prepare groups when bindings
//! change, then wrap them when guide geometry changes, not during painting.

use std::ops::Range;

/// One actual chord, with modifiers separate from its base key's display label.
/// Labels are presentation metadata, never parsed to infer modifier semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord<'a> {
    /// Base key's display label; terminal controls and directional formatting are removed.
    pub key: &'a str,
    /// Ordered modifier labels, joined with hyphens and sanitized like `key`.
    pub modifiers: &'a [&'a str],
}

#[derive(Clone, Debug)]
struct Part {
    text: String,
    width: usize,
    separator: &'static str,
}

/// Owned presentation prepared from actual members, not a synthetic family key.
/// Uniform modifiers are factored with explicit parenthesized scope. Every
/// wrapped row repeats that scope, including rows containing alias continuations.
#[derive(Clone, Debug, Default)]
pub struct ChordGroup {
    parts: Vec<Part>,
    prefix: String,
    prefix_width: usize,
    factored: bool,
    width: usize,
    min_width: usize,
}

/// A physical row with keycap spans measured in terminal cells, not UTF-8 bytes.
/// Separators and scope parentheses are deliberately outside the keycap spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordLine {
    /// Sanitized row text, including separators and any repeated modifier scope.
    pub text: String,
    /// Display width of `text` in terminal cells.
    pub width: usize,
    /// Half-open terminal-cell ranges for modifier and key labels, excluding punctuation.
    pub keycaps: Vec<Range<usize>>,
}

impl ChordGroup {
    /// Members and their aliases retain input order. Empty members contribute
    /// nothing; no absent chord is synthesized to complete a family.
    pub fn new<'a>(members: impl IntoIterator<Item = impl IntoIterator<Item = Chord<'a>>>) -> Self {
        let mut chords = Vec::new();
        for aliases in members {
            for (alias, chord) in aliases.into_iter().enumerate() {
                let separator = if chords.is_empty() {
                    ""
                } else if alias == 0 {
                    ", "
                } else {
                    "/"
                };
                chords.push((chord, separator));
            }
        }
        let Some((first, _)) = chords.first() else {
            return Self::default();
        };
        let factored = chords.len() > 1
            && !first.modifiers.is_empty()
            && chords
                .iter()
                .all(|(chord, _)| chord.modifiers == first.modifiers);
        let prefix = if factored {
            let mut prefix = modifier_prefix(first.modifiers);
            prefix.push('(');
            prefix
        } else {
            String::new()
        };
        let prefix_width = crate::text_width(&prefix);
        let scope_width = prefix_width + usize::from(factored);
        let parts: Vec<_> = chords
            .into_iter()
            .map(|(chord, separator)| {
                let mut text = if factored {
                    String::new()
                } else {
                    modifier_prefix(chord.modifiers)
                };
                text.push_str(&crate::window::clean(chord.key));
                Part {
                    width: crate::text_width(&text),
                    text,
                    separator,
                }
            })
            .collect();
        let width = scope_width
            + parts
                .iter()
                .map(|part| part.separator.len() + part.width)
                .sum::<usize>();
        let min_width = scope_width
            + parts
                .iter()
                .map(|part| part.separator.len() + part.width)
                .max()
                .unwrap_or(0);
        Self {
            parts,
            prefix,
            prefix_width,
            factored,
            width,
            min_width,
        }
    }

    /// Width of the complete unwrapped presentation, in terminal cells.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Smallest width that can show every whole chord, including repeated
    /// modifier scopes and continuation separators. Chords are never truncated.
    pub fn min_width(&self) -> usize {
        self.min_width
    }

    /// Wrap only between complete chords. A leading slash or comma on a
    /// continuation preserves alias/member identity across physical rows.
    /// Returns `None` rather than silently dropping or splitting a chord.
    pub fn wrap(&self, columns: usize) -> Option<Vec<ChordLine>> {
        if self.parts.is_empty() {
            return Some(Vec::new());
        }
        if columns < self.min_width {
            return None;
        }
        let mut rows = Vec::new();
        let mut row = self.start_line("");
        for part in &self.parts {
            let mut separator = part.separator;
            let added = separator.len() + part.width;
            if row.width + added + usize::from(self.factored) > columns {
                self.finish_line(&mut row);
                rows.push(row);
                row = self.start_line(separator);
                separator = "";
            }
            row.text.push_str(separator);
            row.width += separator.len();
            row.keycaps.push(row.width..row.width + part.width);
            row.text.push_str(&part.text);
            row.width += part.width;
        }
        self.finish_line(&mut row);
        rows.push(row);
        Some(rows)
    }

    fn start_line(&self, separator: &str) -> ChordLine {
        let mut keycaps = Vec::new();
        if self.factored {
            // Continuation punctuation stays outside the repeated modifier scope.
            // The opening parenthesis is scope punctuation, not a keycap.
            let start = separator.len();
            keycaps.push(start..start + self.prefix_width - 1);
        }
        ChordLine {
            text: format!("{separator}{}", self.prefix),
            width: separator.len() + self.prefix_width,
            keycaps,
        }
    }

    fn finish_line(&self, row: &mut ChordLine) {
        if self.factored {
            row.text.push(')');
            row.width += 1;
        }
    }
}

fn modifier_prefix(modifiers: &[&str]) -> String {
    let mut prefix = String::new();
    for modifier in modifiers {
        prefix.push_str(&crate::window::clean(modifier));
        prefix.push('-');
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord<'a>(key: &'a str, modifiers: &'a [&'a str]) -> Chord<'a> {
        Chord { key, modifiers }
    }

    fn texts(group: &ChordGroup, columns: usize) -> Vec<String> {
        group
            .wrap(columns)
            .unwrap()
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    #[test]
    fn groups_non_directional_members_without_inventing_absent_keys() {
        let group = ChordGroup::new([vec![chord("F2", &[])], vec![], vec![chord("F7", &[])]]);
        assert_eq!(texts(&group, 40), ["F2, F7"]);
        assert_eq!(group.width(), 6);
        assert_eq!(group.min_width(), 4);
    }

    #[test]
    fn uniform_modifiers_have_explicit_repeated_scope() {
        let group = ChordGroup::new([
            [chord("a", &["Ctrl", "Shift"])],
            [chord("b", &["Ctrl", "Shift"])],
            [chord("c", &["Ctrl", "Shift"])],
        ]);
        assert_eq!(texts(&group, 40), ["Ctrl-Shift-(a, b, c)"]);
        assert_eq!(group.min_width(), 16);
        assert_eq!(
            texts(&group, group.min_width()),
            ["Ctrl-Shift-(a)", ", Ctrl-Shift-(b)", ", Ctrl-Shift-(c)"]
        );
    }

    #[test]
    fn mixed_modifiers_are_never_factored_or_inferred_from_labels() {
        let group = ChordGroup::new([
            [chord("A", &[])],
            [chord("b", &["Ctrl"])],
            [chord("!", &[])],
            [chord("c", &["Shift"])],
        ]);
        assert_eq!(texts(&group, 80), ["A, Ctrl-b, !, Shift-c"]);
        assert_eq!(texts(&group, 9), ["A, Ctrl-b", ", !", ", Shift-c"]);
    }

    #[test]
    fn aliases_and_members_remain_distinct_on_continuations() {
        let group = ChordGroup::new([
            vec![chord("h", &[]), chord("Home", &[])],
            vec![chord("q", &[]), chord("End", &[])],
        ]);
        assert_eq!(texts(&group, 80), ["h/Home, q/End"]);
        assert_eq!(texts(&group, 5), ["h", "/Home", ", q", "/End"]);
        assert_eq!(group.min_width(), 5);
        assert!(group.wrap(4).is_none());
    }

    #[test]
    fn continued_alias_separator_is_outside_scope_and_not_a_literal_slash_key() {
        let group = ChordGroup::new([[chord("x", &["Ctrl"]), chord("/", &["Ctrl"])]]);
        let lines = group.wrap(9).unwrap();
        assert_eq!(lines[0].text, "Ctrl-(x)");
        assert_eq!(lines[1].text, "/Ctrl-(/)");
        assert_eq!(lines[1].keycaps, [1..6, 7..8]);
        assert_eq!(lines[1].width, 9);
    }

    #[test]
    fn mixed_alias_modifiers_and_singletons_keep_complete_chords() {
        let aliases = ChordGroup::new([[chord("a", &["Ctrl"]), chord("a", &[])]]);
        assert_eq!(texts(&aliases, 20), ["Ctrl-a/a"]);
        let single = ChordGroup::new([[chord("F5", &["Alt"])]]);
        assert_eq!(texts(&single, 20), ["Alt-F5"]);
        assert_eq!(single.min_width(), 6);
        assert!(single.wrap(5).is_none());
    }

    #[test]
    fn unicode_measurement_and_keycaps_use_terminal_cells() {
        let group = ChordGroup::new([[chord("界", &["Alt"])], [chord("e\u{301}", &["Alt"])]]);
        let lines = group.wrap(10).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Alt-(界)");
        assert_eq!(lines[0].width, 8);
        assert_eq!(lines[0].keycaps, [0..4, 5..7]);
        assert_eq!(lines[1].text, ", Alt-(e\u{301})");
        assert_eq!(lines[1].width, 9);
        assert_eq!(lines[1].keycaps, [2..6, 7..8]);
        assert!(group.wrap(8).is_none());
        for line in lines {
            assert_eq!(line.width, crate::text_width(&line.text));
            assert!(line.keycaps.iter().all(|span| span.end <= line.width));
        }
    }

    #[test]
    fn joined_emoji_keycaps_fit_their_whole_grapheme_extent() {
        for glyph in ["👩‍💻", "✈\u{fe0f}", "🇫🇷"] {
            let group = ChordGroup::new([[chord(glyph, &[])]]);
            assert_eq!(group.width(), 2);
            assert_eq!(group.min_width(), 2);
            assert!(group.wrap(1).is_none());
            let lines = group.wrap(2).unwrap();
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text, glyph);
            assert_eq!(lines[0].width, 2);
            assert!(matches!(lines[0].keycaps.as_slice(), [range] if *range == (0..2)));
        }
    }

    #[test]
    fn separators_and_scope_punctuation_are_not_keycaps() {
        let group = ChordGroup::new([
            vec![chord("x", &["Ctrl"]), chord("X", &["Ctrl"])],
            vec![chord("y", &["Ctrl"])],
        ]);
        let lines = group.wrap(80).unwrap();
        assert_eq!(lines[0].text, "Ctrl-(x/X, y)");
        assert_eq!(lines[0].keycaps, [0..5, 6..7, 8..9, 11..12]);
    }

    #[test]
    fn measured_limits_match_whole_chord_layout_at_every_width() {
        let group = ChordGroup::new([
            vec![chord("界", &["Alt"]), chord("F12", &["Alt"])],
            vec![chord("e\u{301}", &["Alt"])],
        ]);
        for columns in 0..=group.width() + 1 {
            let lines = group.wrap(columns);
            assert_eq!(lines.is_some(), columns >= group.min_width());
            if let Some(lines) = lines {
                assert!(lines.iter().all(|line| line.width <= columns));
                assert!(
                    lines
                        .iter()
                        .all(|line| line.width == crate::text_width(&line.text))
                );
                // One repeated modifier span per row plus all three real keys.
                let spans: usize = lines.iter().map(|line| line.keycaps.len() - 1).sum();
                assert_eq!(spans, 3);
                assert_eq!(lines.len() == 1, columns >= group.width());
            }
        }
    }

    #[test]
    fn empty_and_zero_width_layouts_never_make_partial_chords() {
        let empty = ChordGroup::new(std::iter::empty::<[Chord<'static>; 0]>());
        assert_eq!(empty.width(), 0);
        assert_eq!(empty.min_width(), 0);
        assert_eq!(empty.wrap(0), Some(Vec::new()));
        let partial = ChordGroup::new([[chord("β", &[])]]);
        assert_eq!(texts(&partial, 1), ["β"]);
        assert!(partial.wrap(0).is_none());
    }

    #[test]
    fn control_labels_use_existing_window_text_sanitization() {
        let group = ChordGroup::new([[chord("a\n\u{202e}b", &["C\rt\u{2066}rl"])]]);
        assert_eq!(texts(&group, 40), ["Ctrl-ab"]);
    }
}
