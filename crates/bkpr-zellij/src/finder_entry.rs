//! Width-fitted finder rows with field-aware, grapheme-aligned fuzzy emphasis.

use bkpr::Snapshot;
use unicode_segmentation::UnicodeSegmentation;

use crate::pane_search::{Annotations, Field, Index, Value};
use crate::ui;

const TEXT: &str = ui::TEXT;
const MATCH: &str = "\u{1b}[4m";
const UNDERLINE_OFF: &str = "\u{1b}[24m";

/// Complete entry content, exactly `columns` terminal cells. No background SGRs
/// are emitted: the caller owns selection/background and the surrounding frame.
pub fn render(
    index: &mut Index,
    snapshot: &Snapshot,
    match_index: usize,
    selected: bool,
    columns: usize,
    theme: &ui::Theme,
) -> String {
    let Some(result) = index.results().get(match_index) else {
        return " ".repeat(columns);
    };
    let pane_index = result.pane;
    let Some(pane) = snapshot.panes.get(pane_index) else {
        return " ".repeat(columns);
    };
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == pane.workspace_id)
        .map(|workspace| workspace.name.as_str())
        .unwrap_or(&pane.workspace_id);
    let name = index.value(pane_index, Field::All, &pane.name);
    let workspace = index.value(pane_index, Field::Workspace, workspace);
    let branch = pane
        .branch
        .as_deref()
        .and_then(|branch| index.value(pane_index, Field::Branch, branch));
    let mut visible = [Value::new(Field::All, 0); 3];
    let mut count = 0;
    for value in [name, workspace, branch].into_iter().flatten() {
        visible[count] = value;
        count += 1;
    }
    let visible = &visible[..count];
    let annotations = index.annotate(pane_index, visible);
    let has_context = annotations.contexts(visible).next().is_some();
    let mut row = Row::new(columns, theme);
    row.text(if selected { "› " } else { "  " }, TEXT);
    row.text("● ", ui::project_color(pane.project.as_ref(), theme));
    if let Some(name) = name {
        let budget = if has_context {
            row.remaining().div_ceil(2)
        } else {
            row.remaining()
        };
        row.value(&annotations, name, budget, TEXT);
    }
    // Put a truthful matching excerpt before ordinary context when the query
    // only matched metadata that is not otherwise displayed in this row.
    for value in annotations.contexts(visible) {
        if row.remaining() <= 5 {
            break;
        }
        row.text(" · ", &theme.muted);
        row.text(value.field.label(), &theme.muted);
        row.text(": ", &theme.muted);
        row.value(&annotations, value, row.remaining(), &theme.bright);
    }
    for value in [workspace, branch].into_iter().flatten() {
        if row.remaining() <= 3 {
            break;
        }
        row.text(" · ", &theme.muted);
        row.value(&annotations, value, row.remaining(), &theme.bright);
    }
    row.finish()
}

struct Row<'a> {
    output: String,
    columns: usize,
    cells: usize,
    last: Option<(usize, usize)>,
    matched: bool,
    color: &'a str,
    match_color: &'a str,
    clipped: bool,
}

// Preserve readable word boundaries in multiline metadata, without controls.
fn clean_glyph(glyph: &str) -> std::borrow::Cow<'_, str> {
    if matches!(glyph, "\n" | "\r" | "\r\n" | "\t") {
        std::borrow::Cow::Borrowed(" ")
    } else {
        ui::clean(glyph)
    }
}

impl<'a> Row<'a> {
    fn new(columns: usize, theme: &'a ui::Theme) -> Self {
        Self {
            output: String::with_capacity(columns.saturating_add(128)),
            columns,
            cells: 0,
            last: None,
            matched: false,
            color: "",
            match_color: &theme.accents[4],
            clipped: false,
        }
    }

    fn remaining(&self) -> usize {
        self.columns.saturating_sub(self.cells)
    }

    fn style(&mut self, matched: bool, color: &'a str) {
        if matched != self.matched || (!matched && color != self.color) {
            if matched {
                self.output.push_str(MATCH);
                self.output.push_str(self.match_color);
            } else {
                self.output.push_str(UNDERLINE_OFF);
                self.output.push_str(color);
            }
        }
        self.matched = matched;
        self.color = color;
    }

    fn glyph(&mut self, glyph: &str, matched: bool, color: &'a str) -> bool {
        let clean = clean_glyph(glyph);
        let width = ui::width(&clean);
        self.push_clean(&clean, width, matched, color)
    }

    fn push_clean(&mut self, clean: &str, width: usize, matched: bool, color: &'a str) -> bool {
        if self.clipped {
            return false;
        }
        if clean.is_empty() {
            return true;
        }
        if width > self.remaining() {
            self.clip();
            return false;
        }
        if width > 0 {
            self.last = Some((self.output.len(), self.cells));
        }
        self.style(matched, color);
        self.output.push_str(clean);
        self.cells += width;
        true
    }

    fn text(&mut self, text: &str, color: &'a str) {
        for glyph in text.graphemes(true) {
            if !self.glyph(glyph, false, color) {
                break;
            }
        }
    }

    fn clip(&mut self) {
        if self.columns > 0 {
            if self.remaining() == 0
                && let Some((bytes, cells)) = self.last
            {
                self.output.truncate(bytes);
                self.cells = cells;
                // Truncation may have removed a style transition as well.
                self.output.push_str(UNDERLINE_OFF);
                self.output.push_str(TEXT);
                self.matched = false;
                self.color = TEXT;
            }
            self.style(false, TEXT);
            self.output.push('…');
            self.cells += 1;
        }
        self.clipped = true;
    }

    fn value(
        &mut self,
        annotations: &Annotations<'_>,
        value: Value,
        budget: usize,
        color: &'a str,
    ) {
        let mut budget = budget.min(self.remaining());
        if budget == 0 || self.clipped {
            return;
        }
        let len = annotations.len(value);
        let mut start = 0;
        if let Some((first, last)) = annotations.match_bounds(value) {
            // Seek when the complete matched span would be clipped, not just
            // its first glyph. The scan stops at the visible cell budget.
            let mut prefix_width = 0;
            let distant = (0..=last).any(|position| {
                prefix_width += ui::width(&clean_glyph(annotations.glyph(value, position)));
                prefix_width >= budget
            });
            if distant {
                // Cached grapheme offsets let us jump into a long PR body
                // without scanning all preceding metadata.
                let mut span_width = 0;
                for position in first..=last {
                    span_width += ui::width(&clean_glyph(annotations.glyph(value, position)));
                    if span_width >= budget {
                        break;
                    }
                }
                let context_budget = budget.saturating_sub(span_width + 2).min(budget / 3);
                let mut before = 0;
                start = first;
                while start > 0 {
                    let width = ui::width(&clean_glyph(annotations.glyph(value, start - 1)));
                    if before + width > context_budget {
                        break;
                    }
                    before += width;
                    start -= 1;
                }
            }
        }
        if start > 0 {
            self.glyph("…", false, color);
            budget -= 1;
        }
        for position in start..len {
            let glyph = annotations.glyph(value, position);
            let clean = clean_glyph(glyph);
            let width = ui::width(&clean);
            let tail = usize::from(position + 1 < len);
            if width + tail > budget {
                if budget > 0 {
                    self.glyph("…", false, color);
                }
                break;
            }
            if !self.push_clean(&clean, width, annotations.matched(value, position), color) {
                break;
            }
            budget -= width;
        }
    }

    fn finish(mut self) -> String {
        self.style(false, TEXT);
        let padding = self.remaining();
        self.output.extend(std::iter::repeat_n(' ', padding));
        self.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr::{Pane, ProjectIdentity, Status, Workspace};

    fn snapshot(name: &str, description: &str) -> Snapshot {
        Snapshot {
            protocol: bkpr::PROTOCOL_VERSION,
            instance_id: "test".into(),
            revision: 1,
            session: "test".into(),
            connected: true,
            error: None,
            focused_pane_id: None,
            workspaces: vec![Workspace {
                id: "workspace".into(),
                name: "Engine room".into(),
                active_tab_id: "tab".into(),
                projects: vec![],
                status: Status::Unknown,
                attention: None,
            }],
            panes: vec![Pane {
                id: "pane".into(),
                terminal_id: "terminal".into(),
                workspace_id: "workspace".into(),
                tab_id: "tab".into(),
                name: name.into(),
                description: description.into(),
                harness: None,
                agent_id: None,
                status: Status::Unknown,
                attention: None,
                cwd: Some("/repo".into()),
                branch: Some("feature/queues".into()),
                branch_changes: None,
                project: Some(ProjectIdentity {
                    id: "git:engine".into(),
                    name: "Engine".into(),
                    repository: true,
                    color_slot: Some(0),
                }),
                ci: None,
            }],
        }
    }

    fn render_query(snapshot: &Snapshot, query: &str, columns: usize) -> String {
        let mut index = Index::default();
        index.rebuild(snapshot);
        assert_eq!(index.search(query).len(), 1);
        render(
            &mut index,
            snapshot,
            0,
            true,
            columns,
            &ui::Theme::default(),
        )
    }

    // Decode the rendered behavior, not the implementation's escape spelling.
    fn glyphs(row: &str) -> Vec<(String, bool)> {
        let mut output = Vec::new();
        let mut remaining = row;
        let mut underlined = false;
        while !remaining.is_empty() {
            if let Some(sgr) = remaining.strip_prefix("\u{1b}[") {
                let end = sgr.find('m').expect("complete SGR");
                for parameter in sgr[..end].split(';') {
                    match parameter {
                        "4" => underlined = true,
                        "24" | "0" => underlined = false,
                        _ => {}
                    }
                }
                remaining = &sgr[end + 1..];
            } else {
                let glyph = remaining.graphemes(true).next().unwrap();
                output.push((glyph.into(), underlined));
                remaining = &remaining[glyph.len()..];
            }
        }
        output
    }

    fn plain(row: &str) -> String {
        glyphs(row).into_iter().map(|(glyph, _)| glyph).collect()
    }

    fn emphasized(row: &str) -> String {
        glyphs(row)
            .into_iter()
            .filter_map(|(glyph, matched)| matched.then_some(glyph))
            .collect()
    }

    #[test]
    fn discontiguous_matches_emphasize_only_aligned_glyphs() {
        let row = render_query(&snapshot("compiler", ""), "cmplr", 60);
        assert_eq!(emphasized(&row), "cmplr");
        assert!(plain(&row).starts_with("› ● compiler · Engine room"));
        assert_eq!(ui::width(&plain(&row)), 60);
    }

    #[test]
    fn field_filters_never_emphasize_coincidental_name_letters() {
        let row = render_query(&snapshot("feature worker", ""), "branch:feat", 70);
        let decoded = glyphs(&row);
        let name_end = plain(&row).find(" · ").unwrap();
        assert!(!decoded[..name_end].iter().any(|(_, matched)| *matched));
        assert_eq!(emphasized(&row), "feat");
    }

    #[test]
    fn hidden_metadata_gets_compact_honest_matching_context() {
        let row = render_query(
            &snapshot(
                "compiler",
                "Long unrelated introduction. Eliminate starvation in dispatcher",
            ),
            "starvation",
            64,
        );
        assert!(plain(&row).starts_with("› ● compiler · text: "));
        assert!(plain(&row).contains("starvation"));
        assert_eq!(emphasized(&row), "starvation");
        assert_eq!(ui::width(&plain(&row)), 64);
    }

    #[test]
    fn excerpt_keeps_the_match_when_leading_context_would_consume_its_budget() {
        let query = "abcdefghijklmnopq";
        let row = render_query(
            &snapshot("compiler", "aaaa abcdefghijklmnopq tail"),
            query,
            42,
        );
        assert_eq!(emphasized(&row), query);
        assert_eq!(ui::width(&plain(&row)), 42);
    }

    #[test]
    fn grapheme_matches_and_clipping_do_not_split_combining_or_emoji_sequences() {
        let state = snapshot("Cafe\u{301} 👩\u{200d}💻 東京", "");
        let row = render_query(&state, "cafe", 40);
        assert_eq!(emphasized(&row), "Cafe\u{301}");
        let full = plain(&render_query(&state, "", 40));
        for columns in 0..24 {
            let clipped = render_query(&state, "", columns);
            let text = plain(&clipped);
            assert_eq!(ui::width(&text), columns);
            assert!(!text.contains('\u{1b}'));
            let content = text.trim_end().trim_end_matches('…');
            assert!(full.starts_with(content));
            if content.contains('👩') {
                assert!(content.contains("👩\u{200d}💻"));
            }
        }
    }

    #[test]
    fn clipped_styled_runs_end_before_ellipsis_and_padding() {
        for columns in 5..16 {
            let row = render_query(&snapshot("compiler", ""), "cmplr", columns);
            assert_eq!(ui::width(&plain(&row)), columns);
            for (glyph, matched) in glyphs(&row) {
                if matches!(glyph.as_str(), "…" | " " | "›" | "●") {
                    assert!(!matched);
                }
            }
            assert!(!row.contains("\u{1b}[0m"));
            assert!(!row.contains("48;"));
        }
    }

    #[test]
    fn empty_and_presence_queries_keep_normal_visible_rows() {
        let state = snapshot("compiler", "");
        for query in ["", "project:", "branch:"] {
            let row = render_query(&state, query, 60);
            assert_eq!(emphasized(&row), "");
            assert!(plain(&row).starts_with("› ● compiler · Engine room · feature/queues"));
        }
        let mut local = state;
        local.panes[0].project.as_mut().unwrap().repository = false;
        let row = render_query(&local, "", 60);
        assert_eq!(emphasized(&row), "");
        assert!(plain(&row).starts_with("› ● compiler · Engine room · feature/queues"));
    }

    #[test]
    fn metadata_controls_cannot_inject_terminal_sequences() {
        let row = render_query(&snapshot("safe\u{1b}[31m name", ""), "safe", 50);
        assert!(!row.contains("\u{1b}[31m"));
        assert!(!plain(&row).contains('\u{1b}'));
        assert_eq!(ui::width(&plain(&row)), 50);
    }
}
