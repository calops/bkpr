//! Cached application-owned recovery columns over independent fuzzy search text.
use super::{Key, RecoveryCandidate, Role, SearchList, Size, status_label, status_role};
use bkpr_tui_framework::{ForegroundSpan, Rect, Theme, text_width};
use unicode_segmentation::UnicodeSegmentation;

struct Field {
    text: String,
    source: u32,
    role: Role,
}

struct Row {
    key: Key,
    search: String,
    icon: &'static str,
    fields: [Field; 4],
    spans: Vec<(u16, u16, Role)>,
}

impl Row {
    fn new(candidate: &RecoveryCandidate) -> Self {
        let mut search = String::new();
        let mut field = |text: String, role| {
            if !search.is_empty() {
                search.push(' ');
            }
            let source = search.graphemes(true).count() as u32;
            let text = crate::metadata::clean(&text);
            search.push_str(&text);
            Field { text, source, role }
        };
        let fields = [
            field(
                candidate.harness.kind.display_name().into(),
                crate::controller::harness_role(candidate.harness.kind),
            ),
            field(
                status_label(candidate.status).into(),
                status_role(candidate.status),
            ),
            field(candidate.harness.cwd.display().to_string(), Role::Panel),
            field(candidate.harness.conversation_id.clone(), Role::Muted),
        ];
        search.push_str(" Saved pane title: ");
        search.push_str(&crate::metadata::clean(&candidate.title));
        search.push_str(" Source pane: ");
        search.push_str(&crate::metadata::clean(&candidate.pane_id));
        Self {
            key: Key::from(candidate),
            search,
            icon: crate::controller::harness_icon(candidate.harness.kind),
            fields,
            spans: Vec::with_capacity(4),
        }
    }

    fn presentation(&mut self, width: u16, harness: u16) -> (String, Vec<Option<u32>>) {
        let mut text = String::new();
        let mut sources = Vec::new();
        self.spans.clear();
        let harness = harness.min(width);
        let available = width.saturating_sub(harness + 2);
        let status = available.min(11);
        let remainder = available.saturating_sub(status + 2);
        let (cwd, identity) = if remainder >= 26 {
            let identity = ((remainder - 2) / 2).max(12);
            (remainder - identity - 2, identity)
        } else {
            (0, remainder)
        };
        let columns = [(0, harness), (1, status), (2, cwd), (3, identity)];
        let mut x = 0_u16;
        for (index, cells) in columns {
            if cells == 0 {
                continue;
            }
            if x > 0 {
                text.push_str("  ");
                sources.extend([None, None]);
                x += 2;
            }
            let start = x;
            let field = &self.fields[index];
            let icon_width = text_width(self.icon) as u16;
            let prefix = if index == 0 && cells >= icon_width + 2 {
                text.push_str(self.icon);
                text.push(' ');
                sources.extend(std::iter::repeat_n(
                    None,
                    self.icon.graphemes(true).count() + 1,
                ));
                icon_width + 1
            } else {
                0
            };
            append_field(&mut text, &mut sources, field, cells - prefix, index == 2);
            x += cells;
            self.spans.push((start, cells, field.role));
        }
        (text, sources)
    }
}

fn append_field(
    text: &mut String,
    sources: &mut Vec<Option<u32>>,
    field: &Field,
    width: u16,
    keep_end: bool,
) {
    let clipped = text_width(&field.text) > usize::from(width);
    if clipped && keep_end && width > 0 {
        text.push('…');
        sources.push(None);
        let mut used = 1;
        let mut start = field.text.len();
        for (byte, grapheme) in field.text.grapheme_indices(true).rev() {
            let cells = text_width(grapheme);
            if used + cells > usize::from(width) {
                break;
            }
            start = byte;
            used += cells;
        }
        let first = field.text[..start].graphemes(true).count() as u32;
        for (index, grapheme) in field.text[start..].graphemes(true).enumerate() {
            text.push_str(grapheme);
            sources.push(Some(field.source + first + index as u32));
        }
        let padding = usize::from(width) - used;
        text.extend(std::iter::repeat_n(' ', padding));
        sources.extend(std::iter::repeat_n(None, padding));
        return;
    }
    let budget = usize::from(width.saturating_sub(u16::from(clipped)));
    let mut used = 0;
    for (index, grapheme) in field.text.graphemes(true).enumerate() {
        let cells = text_width(grapheme);
        if used + cells > budget {
            break;
        }
        if used == 0 && cells == 0 {
            continue;
        }
        text.push_str(grapheme);
        sources.push(Some(field.source + index as u32));
        used += cells;
    }
    if clipped && width > 0 {
        text.push('…');
        sources.push(None);
        used += 1;
    }
    let padding = usize::from(width) - used;
    text.extend(std::iter::repeat_n(' ', padding));
    sources.extend(std::iter::repeat_n(None, padding));
}

/// One retained presentation per candidate, invalidated only by facts or result width.
pub(super) struct Rows {
    rows: Vec<Row>,
    width: Option<u16>,
    harness_width: u16,
}

impl Rows {
    /// Cache sanitized searchable facts without treating a pane title as a conversation title.
    pub(super) fn new(candidates: &[RecoveryCandidate]) -> Self {
        let rows: Vec<_> = candidates.iter().map(Row::new).collect();
        let harness_width = rows
            .iter()
            .map(|row| text_width(row.icon) + 1 + text_width(&row.fields[0].text))
            .max()
            .unwrap_or(0)
            .min(usize::from(u16::MAX)) as u16;
        Self {
            rows,
            width: None,
            harness_width,
        }
    }

    /// Supply full searchable facts independently of the currently visible columns.
    pub(super) fn items(&self) -> impl Iterator<Item = (Key, String)> + '_ {
        self.rows
            .iter()
            .map(|row| (row.key.clone(), row.search.clone()))
    }

    /// Rebuild display strings and match mappings only when the result width changes.
    pub(super) fn prepare(&mut self, list: &mut SearchList<Key>, size: Size) {
        let width = list.content_layout(size).results.width;
        if self.width == Some(width) {
            return;
        }
        self.width = Some(width);
        list.set_row_presentations(
            self.rows
                .iter_mut()
                .map(|row| row.presentation(width, self.harness_width)),
        );
    }

    /// Reuse cached semantic column bounds while preserving selection and fuzzy emphasis.
    pub(super) fn foregrounds(
        &self,
        list: &SearchList<Key>,
        size: Size,
        theme: &Theme,
        panes: &std::collections::HashMap<String, usize>,
        output: &mut Vec<ForegroundSpan>,
    ) {
        output.clear();
        for (rect, key) in list.visible_items(size) {
            let Some(row) = panes
                .get(&key.pane_id)
                .and_then(|index| self.rows.get(*index))
            else {
                continue;
            };
            for &(x, width, role) in &row.spans {
                output.push(ForegroundSpan {
                    rect: Rect {
                        x: rect.x + x,
                        width,
                        ..rect
                    },
                    foreground: theme.ink(role, false),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{HarnessKind, HarnessRecovery, RecoveryStatus};

    fn candidate(kind: HarnessKind) -> RecoveryCandidate {
        RecoveryCandidate {
            pane_id: "source".into(),
            workspace_id: None,
            window_id: None,
            window_revision: None,
            title: "fish".into(),
            harness: HarnessRecovery {
                kind,
                conversation_id: "native-会話-e\u{301}-👩‍💻-exact".into(),
                cwd: "/work/日本語/👩‍💻-e\u{301}".into(),
            },
            status: RecoveryStatus::Recoverable,
            reason: None,
        }
    }

    #[test]
    fn shell_title_is_searchable_metadata_not_primary_identity() {
        let candidate = candidate(HarnessKind::Pi);
        let mut row = Row::new(&candidate);
        let (text, _) = row.presentation(160, 12);
        assert!(text.starts_with(crate::controller::harness_icon(HarnessKind::Pi)));
        assert!(text.contains("Pi"));
        assert!(text.contains("Recoverable"));
        assert!(text.contains(&candidate.harness.cwd.display().to_string()));
        assert!(text.contains(&candidate.harness.conversation_id));
        assert!(!text.contains("fish"));
        assert!(row.search.contains("Saved pane title: fish"));
    }

    #[test]
    fn columns_align_and_clipping_preserves_unicode_match_identity() {
        let mut rows = [
            Row::new(&candidate(HarnessKind::Pi)),
            Row::new(&candidate(HarnessKind::Claude)),
        ];
        for width in 0..=160 {
            let mut status_start = None;
            for row in &mut rows {
                let (text, sources) = row.presentation(width, 12);
                assert!(text_width(&text) <= usize::from(width), "{width}: {text}");
                assert_eq!(sources.len(), text.graphemes(true).count());
                let original: Vec<_> = row.search.graphemes(true).collect();
                for (grapheme, source) in text.graphemes(true).zip(sources) {
                    if let Some(source) = source {
                        assert_eq!(grapheme, original[source as usize], "{width}: {text}");
                    }
                }
                if row.spans.len() > 1 {
                    let start = row.spans[1].0;
                    assert_eq!(*status_start.get_or_insert(start), start);
                }
            }
        }
    }
}
