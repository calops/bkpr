//! Single-row pane entries, adapted from main 9ad93bd's MIT-licensed
//! finder_entry.rs. Field-aware excerpts use the current metadata-only index.
use super::{Document, Field, Term, Value, annotate, display_width, excerpt};
use bkpr_tui_framework::{Rect, Role, RoleSpan};
use nucleo_matcher::Matcher;
use unicode_segmentation::UnicodeSegmentation;

pub(super) enum Span {
    Role(RoleSpan),
    Project(Rect),
}

pub(super) struct Entry {
    pub text: String,
    pub spans: Vec<Span>,
    pub matches: Vec<usize>,
    width: usize,
    used: usize,
}
impl Entry {
    fn remaining(&self) -> usize {
        self.width.saturating_sub(self.used)
    }
    fn finish(mut self) -> Self {
        self.text.extend(std::iter::repeat_n(' ', self.remaining()));
        self
    }
    fn text(&mut self, text: &str, role: Role) {
        let start = self.used;
        for glyph in text.graphemes(true) {
            let width = display_width(glyph);
            if width > self.remaining() {
                break;
            }
            self.text.push_str(glyph);
            self.used += width;
        }
        if self.used > start {
            self.spans.push(Span::Role(RoleSpan {
                rect: Rect {
                    x: start as u16,
                    y: 0,
                    width: (self.used - start) as u16,
                    height: 1,
                },
                role,
            }));
        }
    }
    fn value(
        &mut self,
        value: &Value,
        terms: &[Term],
        matcher: &mut Matcher,
        positions: &mut Vec<u32>,
        budget: usize,
        role: Role,
    ) {
        positions.clear();
        annotate(value, terms, matcher, positions);
        let (text, marks) = excerpt(value, positions, budget.min(self.remaining()));
        let start = self.used;
        self.text(&text, if value.stale { Role::Warning } else { role });
        for column in marks.into_iter().map(|column| column + start) {
            self.matches.push(column);
            self.spans.push(Span::Role(RoleSpan {
                rect: Rect {
                    x: column as u16,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                role: Role::Info,
            }));
        }
    }
}

pub(super) fn render(
    document: &Document,
    terms: &[Term],
    matcher: &mut Matcher,
    positions: &mut Vec<u32>,
    selected: bool,
    width: u16,
) -> Entry {
    let mut row = Entry {
        text: String::new(),
        spans: Vec::new(),
        matches: Vec::new(),
        width: usize::from(width),
        used: 0,
    };
    let title = &document.values[0];
    let workspace = document.values.iter().find(|v| v.field == Field::Workspace);
    let branch = document.values.iter().find(|v| v.field == Field::Branch);
    // Matching metadata comes before decorative context, including workspace
    // and branch; otherwise a narrow row could hide its only actual match.
    let matched_context = document
        .values
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, value)| {
            let score = terms
                .iter()
                .filter(|term| term.field == Field::Text || term.field == value.field)
                .filter_map(|term| match &term.atom {
                    Some(atom) => {
                        if term.field == Field::Text && atom.score(title.view(), matcher).is_some()
                        {
                            None
                        } else {
                            atom.score(value.view(), matcher).map(u32::from)
                        }
                    }
                    None => Some(0),
                })
                .max()?;
            Some((score, index, value))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .map(|(_, _, value)| value);
    row.text(if selected { "› " } else { "  " }, Role::Panel);
    row.text(
        if document.values.iter().any(|v| v.field == Field::Project) {
            "● "
        } else {
            "○ "
        },
        Role::Muted,
    );
    if document
        .values
        .iter()
        .any(|value| value.field == Field::Project)
    {
        row.spans.push(Span::Project(Rect {
            x: 2,
            y: 0,
            width: 1,
            height: 1,
        }));
    }
    if let Some(value) = matched_context
        && row.remaining() < 10
    {
        let budget = row.remaining();
        row.value(value, terms, matcher, positions, budget, Role::Panel);
        return row.finish();
    }
    let context = matched_context.is_some() || workspace.is_some() || branch.is_some();
    let budget = if context {
        row.remaining().div_ceil(2)
    } else {
        row.remaining()
    };
    row.value(title, terms, matcher, positions, budget, Role::Panel);
    if let Some(value) = matched_context
        && row.remaining() > 3
    {
        row.text(" · ", Role::Muted);
        let label = format!("{}: ", value.label());
        // A matching excerpt is more important than an overlong label.
        if display_width(&label) + 2 < row.remaining() {
            row.text(&label, Role::Muted);
        }
        let budget = row.remaining();
        row.value(value, terms, matcher, positions, budget, Role::Panel);
    }
    let contexts = [workspace, branch];
    for (index, value) in contexts.into_iter().enumerate() {
        if let Some(value) = value
            && !matched_context.is_some_and(|matched| std::ptr::eq(value, matched))
            && row.remaining() > 3
        {
            row.text(" · ", Role::Muted);
            let budget = if index == 0 && branch.is_some() && row.remaining() >= 6 {
                row.remaining().div_ceil(2).saturating_sub(1)
            } else {
                row.remaining()
            };
            row.value(
                value,
                terms,
                matcher,
                positions,
                budget,
                if value.field == Field::Branch {
                    Role::Secondary
                } else {
                    Role::Muted
                },
            );
        }
    }
    row.finish()
}
