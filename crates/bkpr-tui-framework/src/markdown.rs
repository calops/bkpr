//! Parse CommonMark into inert, styled text for the shared scene compositor.
use pulldown_cmark::{CowStr, Event, LinkType, Parser, Tag, TagEnd};
use std::{fmt::Write as _, ops::Range};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// Semantic flags that a caller can project into its own theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Strong emphasis or a heading.
    pub bold: bool,
    /// Inline emphasis.
    pub italic: bool,
    /// Inline code span.
    pub inline_code: bool,
    /// Block code, fenced or indented.
    pub fenced_code: bool,
    /// Text within a block quote, including its marker.
    pub quote: bool,
    /// A link label or its inert, displayed destination.
    pub link: bool,
}

/// Part of a row with one semantic style and, where available, exact source bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Run<'a> {
    /// Printable text borrowed from the document or its fixed layout markers.
    pub text: &'a str,
    /// Semantic styling, independent of terminal capabilities.
    pub style: Style,
    /// UTF-8 byte range in the original input; when present its bytes equal `text`.
    /// Generated text, transformations and discontiguous graphemes have no range.
    pub source: Option<Range<usize>>,
}

/// One display row, in left-to-right paint order.
#[derive(Debug, PartialEq, Eq)]
pub struct Row<'a> {
    /// Printable text runs, already laid out in order without splitting graphemes.
    pub runs: Vec<Run<'a>>,
    /// Cell width of a repeated layout prefix on a continuation row.
    pub continuation_prefix: usize,
    /// Spaces hidden by the soft wrap to the next row of this logical line.
    /// `None` marks the end of the logical line.
    pub join_spaces: Option<usize>,
}

/// Width-specific row ranges. Build once and borrow runs only for requested rows.
#[derive(Debug, Default)]
pub struct Layout {
    columns: usize,
    rows: Vec<RowRange>,
}

impl Layout {
    /// Number of display rows, including empty logical lines.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the layout has no display rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
struct RowRange {
    line: usize,
    start: usize,
    end: usize,
}

impl RowRange {
    fn join_spaces(self, next: usize) -> usize {
        // Replacements have an empty byte range, but consume a whole grapheme.
        if self.start == self.end {
            0
        } else {
            next - self.end
        }
    }
}

/// One row borrowing its text and semantic styles from the parsed document.
pub struct RowView<'a> {
    document: &'a Document,
    range: RowRange,
    prefix: [&'a str; 3],
    /// Cell width of a repeated layout prefix on a continuation row.
    pub continuation_prefix: usize,
    /// Spaces hidden by the soft wrap; `None` ends the logical line.
    pub join_spaces: Option<usize>,
}

impl<'a> RowView<'a> {
    /// Printable runs in display order without copying text or allocating run storage.
    pub fn runs(self) -> impl Iterator<Item = Run<'a>> {
        let line = &self.document.lines[self.range.line];
        let prefix_style = Style {
            quote: line.prefix.quote,
            ..Style::default()
        };
        let spans = &self.document.spans[line.spans.clone()];
        let start = self.range.start;
        let spans = &spans[spans.partition_point(|span| span.text.end <= start)..];
        let replacement = (start == self.range.end && start < line.text.end).then(|| Run {
            text: "�",
            style: spans[0].style,
            source: None,
        });
        self.prefix
            .into_iter()
            .filter(|text| !text.is_empty())
            .map(move |text| Run {
                text,
                style: prefix_style,
                source: None,
            })
            .chain(replacement)
            .chain(self.document.row_runs(start..self.range.end, spans))
    }
}

/// Immutable parsed content, reusable at any positive text width.
#[derive(Debug, Default)]
pub struct Document {
    text: String,
    markers: String,
    lines: Vec<Line>,
    spans: Vec<Span>,
}

impl Document {
    /// Parse bounded UTF-8 input, retaining line breaks and spacing between blocks.
    /// Raw HTML is omitted, images are alt-text labels, and links are plain text.
    /// Escape terminal controls and directional formatting before reflow.
    pub fn parse(source: &str) -> Self {
        let mut builder = Builder::default();
        for (event, range) in Parser::new(source).into_offset_iter() {
            match event {
                Event::Start(tag) => builder.start(tag, range),
                Event::End(tag) => builder.end(tag, source),
                Event::Text(text) => {
                    let offset = source_offset(source, &text, range);
                    if builder.code_block {
                        let mut offset = offset;
                        for part in text.split_inclusive('\n') {
                            if let Some(line) = part.strip_suffix('\n') {
                                builder.safe(
                                    line.strip_suffix('\r').unwrap_or(line),
                                    offset,
                                    false,
                                );
                                builder.finish_line(true);
                            } else {
                                builder.safe(part, offset, false);
                            }
                            offset = offset.map(|start| start + part.len());
                        }
                    } else {
                        builder.safe(&text, offset, false);
                    }
                }
                Event::Code(text) => {
                    let offset = source_offset(source, &text, range);
                    builder.safe(&text, offset, true);
                }
                Event::SoftBreak | Event::HardBreak => builder.finish_line(true),
                Event::Rule => {
                    builder.literal("─");
                    builder.end_block();
                }
                _ => (),
            }
        }
        builder.finish_line(false);
        builder.doc
    }

    /// Reflow to at most `columns` per row, measured like [`crate::text_width`],
    /// without copying the parsed text. At zero columns there are no rows.
    /// An unpaintable grapheme gets a one-cell replacement; prefixes compact
    /// before they take the columns needed for body text.
    pub fn wrap(&self, columns: usize) -> Vec<Row<'_>> {
        let mut rows = Vec::with_capacity(if columns == 0 { 0 } else { self.lines.len() });
        self.reflow(columns, |range, join_spaces| {
            let row = self.row_view(columns, range, join_spaces);
            rows.push(Row {
                continuation_prefix: row.continuation_prefix,
                join_spaces: row.join_spaces,
                runs: row.runs().collect(),
            });
        });
        rows
    }

    /// Replace the layout with compact row ranges for this document and width.
    /// At zero columns the layout is empty.
    pub fn layout(&self, columns: usize, layout: &mut Layout) {
        layout.columns = columns;
        layout.rows.clear();
        if columns != 0 {
            layout.rows.reserve(self.lines.len());
        }
        self.reflow(columns, |range, _| layout.rows.push(range));
    }

    /// Borrow a row from a layout built for this document, or `None` out of bounds.
    pub fn row(&self, layout: &Layout, index: usize) -> Option<RowView<'_>> {
        let range = *layout.rows.get(index)?;
        let join_spaces = layout
            .rows
            .get(index + 1)
            .filter(|next| next.line == range.line)
            .map(|next| range.join_spaces(next.start));
        Some(self.row_view(layout.columns, range, join_spaces))
    }

    fn reflow(&self, columns: usize, mut emit: impl FnMut(RowRange, Option<usize>)) {
        if columns == 0 {
            return;
        }
        for (line_index, line) in self.lines.iter().enumerate() {
            let prefix = self.prefix(&line.prefix, columns);
            let body_columns = columns - prefix.iter().map(|text| text.len()).sum::<usize>();
            let mut spans = &self.spans[line.spans.clone()];
            let mut start = line.text.start;
            loop {
                let rest = &self.text[start..line.text.end];
                if !rest.is_empty() {
                    span_at(&mut spans, start);
                }
                let cut = row_cut(rest, body_columns, line.verbatim, start, spans);
                let next = start + cut.next;
                let range = RowRange {
                    line: line_index,
                    start,
                    end: if cut.replace { start } else { start + cut.end },
                };
                emit(
                    range,
                    (next < line.text.end).then(|| range.join_spaces(next)),
                );
                start = next;
                if start == line.text.end {
                    break;
                }
            }
        }
    }

    fn row_view(&self, columns: usize, range: RowRange, join_spaces: Option<usize>) -> RowView<'_> {
        let line = &self.lines[range.line];
        let first = range.start == line.text.start;
        let mut prefix = self.prefix(&line.prefix, columns);
        if !first || !line.prefix.show_marker {
            prefix[2] = &SPACES[..prefix[2].len()];
        }
        RowView {
            document: self,
            range,
            prefix,
            continuation_prefix: if first {
                0
            } else {
                prefix.iter().map(|text| text.len()).sum()
            },
            join_spaces,
        }
    }

    fn prefix<'a>(&'a self, prefix: &Prefix, columns: usize) -> [&'a str; 3] {
        // Leave room for an ordinary double-width glyph even in nested blocks.
        let budget = columns.saturating_sub(2);
        let marker = prefix.item.as_ref().map_or("", |item| {
            let marker = match item {
                Marker::Bullet => "- ",
                Marker::Ordered(range) => &self.markers[range.clone()],
            };
            if marker.len() <= budget {
                marker
            } else if budget >= 2 {
                "- "
            } else {
                ""
            }
        });
        let quote = if prefix.quote && budget - marker.len() >= 2 {
            "> "
        } else {
            ""
        };
        let indent = prefix.indent.min(budget - marker.len() - quote.len());
        [quote, &SPACES[..indent], marker]
    }

    fn row_runs<'a>(
        &'a self,
        range: Range<usize>,
        mut spans: &'a [Span],
    ) -> impl Iterator<Item = Run<'a>> {
        let mut offset = range.start;
        let mut boundary = offset;
        let mut graphemes = self.text[range.clone()].graphemes(true);
        let mut cross_span = None;
        std::iter::from_fn(move || {
            if let Some(run) = cross_span.take() {
                return Some(run);
            }
            if offset == range.end {
                return None;
            }
            let start = offset;
            let span = span_at(&mut spans, start);
            let limit = span.text.end.min(range.end);
            if limit == range.end {
                offset = limit;
                return Some(self.run(span, start..limit));
            }
            let mut aligned = start;
            while boundary <= limit {
                aligned = boundary;
                boundary += graphemes.next().expect("span ends inside the row").len();
            }
            offset = aligned;
            if aligned < limit {
                // Markdown delimiters can separate a base and its marks. Keep
                // the complete displayed grapheme under the first span's style.
                let run = Run {
                    text: &self.text[aligned..boundary],
                    style: span.style,
                    source: None,
                };
                offset = boundary;
                if aligned == start {
                    return Some(run);
                }
                cross_span = Some(run);
            }
            Some(self.run(span, start..aligned))
        })
    }

    fn run(&self, span: &Span, text: Range<usize>) -> Run<'_> {
        let source = span.source.map(|source| {
            let start = source + text.start - span.text.start;
            start..start + text.len()
        });
        Run {
            text: &self.text[text],
            style: span.style,
            source,
        }
    }
}

// An ordered marker holds a u64 and ". "; continuation padding matches its width.
const SPACES: &str = "                        ";
const MAX_INDENT: usize = 6;

#[derive(Debug)]
struct Line {
    text: Range<usize>,
    spans: Range<usize>,
    prefix: Prefix,
    verbatim: bool,
}

#[derive(Debug)]
struct Prefix {
    quote: bool,
    indent: usize,
    item: Option<Marker>,
    show_marker: bool,
}

#[derive(Debug)]
struct Span {
    text: Range<usize>,
    style: Style,
    source: Option<usize>,
}

fn span_at<'a>(spans: &mut &'a [Span], offset: usize) -> &'a Span {
    let mut remaining = *spans;
    while remaining[0].text.end <= offset {
        remaining = &remaining[1..];
    }
    *spans = remaining;
    &remaining[0]
}

struct Cut {
    end: usize,
    next: usize,
    replace: bool,
}

fn row_cut(rest: &str, columns: usize, verbatim: bool, start: usize, mut spans: &[Span]) -> Cut {
    let breakable = |offset, glyph: &str, spans: &mut &[Span]| {
        !verbatim && glyph == " " && !span_at(spans, start + offset).style.inline_code
    };
    let mut used = 0;
    let mut cell_bytes = 0;
    let mut word_break: Option<(usize, usize)> = None;
    let mut in_space = false;
    let mut glyphs = rest.grapheme_indices(true);
    while let Some((offset, glyph)) = glyphs.next() {
        let cells = glyph.width();
        // A canonical cell accepts at most 8192 text bytes, including any
        // standalone zero-width text the scene painter appends to it.
        cell_bytes = glyph.len() + if cells == 0 { cell_bytes } else { 0 };
        let fits = cell_bytes <= 8192 && cells <= columns - used;
        if offset == 0 && (!fits || cells == 0) {
            return Cut {
                end: glyph.len(),
                next: glyph.len(),
                replace: true,
            };
        }
        let space = breakable(offset, glyph, &mut spans);
        if space {
            if in_space {
                if let Some((_, next)) = &mut word_break {
                    *next = offset + 1;
                }
            } else if offset != 0 {
                word_break = Some((offset, offset + 1));
            }
        }
        if !fits {
            let (end, mut next) = word_break.unwrap_or((offset, offset));
            if space && word_break.is_some() {
                for (offset, glyph) in
                    glyphs.take_while(|&(offset, glyph)| breakable(offset, glyph, &mut spans))
                {
                    next = offset + glyph.len();
                }
            }
            return Cut {
                end,
                next,
                replace: false,
            };
        }
        in_space = space;
        used += cells;
    }
    Cut {
        end: rest.len(),
        next: rest.len(),
        replace: false,
    }
}

#[derive(Default)]
struct Builder<'a> {
    doc: Document,
    open: Option<Line>,
    block_gap: bool,
    bold: usize,
    italic: usize,
    quotes: usize,
    code_block: bool,
    images: usize,
    lists: Vec<List>,
    links: Vec<Option<(CowStr<'a>, Range<usize>)>>,
}

struct List {
    number: Option<u64>,
    item: Option<Item>,
}

struct Item {
    marker: Marker,
    pending: bool,
}

#[derive(Clone, Debug)]
enum Marker {
    Bullet,
    Ordered(Range<usize>),
}

impl<'a> Builder<'a> {
    fn start(&mut self, tag: Tag<'a>, range: Range<usize>) {
        match tag {
            Tag::Strong | Tag::Heading { .. } => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::BlockQuote(_) => {
                self.finish_line(false);
                self.quotes += 1;
            }
            Tag::CodeBlock(_) => {
                self.finish_line(false);
                self.code_block = true;
            }
            Tag::List(number) => {
                self.finish_item_line();
                self.lists.push(List { number, item: None });
            }
            Tag::Item => {
                let list = self.lists.last_mut().expect("item in a list");
                let marker = if let Some(number) = &mut list.number {
                    let start = self.doc.markers.len();
                    write!(self.doc.markers, "{number}. ").expect("write to string");
                    *number += 1;
                    Marker::Ordered(start..self.doc.markers.len())
                } else {
                    Marker::Bullet
                };
                list.item = Some(Item {
                    marker,
                    pending: true,
                });
            }
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                let show_target = self.images == 0
                    && !dest_url.is_empty()
                    && !matches!(link_type, LinkType::Autolink | LinkType::Email);
                self.links.push(show_target.then_some((dest_url, range)));
            }
            Tag::Image { .. } => {
                self.images += 1;
                self.literal("[image: ");
            }
            _ => (),
        }
    }

    fn end(&mut self, tag: TagEnd, source: &str) {
        match tag {
            TagEnd::Heading(_) => {
                self.bold -= 1;
                self.end_block();
            }
            TagEnd::Strong => self.bold -= 1,
            TagEnd::Emphasis => self.italic -= 1,
            TagEnd::BlockQuote(_) => {
                self.end_block();
                self.quotes -= 1;
            }
            TagEnd::CodeBlock => {
                self.end_block();
                self.code_block = false;
            }
            TagEnd::Paragraph => self.end_block(),
            TagEnd::List(_) => {
                self.finish_line(false);
                self.lists.pop();
                self.block_gap |= self.lists.is_empty() && !self.doc.lines.is_empty();
            }
            TagEnd::Item => {
                self.finish_item_line();
                self.lists.last_mut().expect("item in a list").item = None;
            }
            TagEnd::Link => {
                if let Some((dest, range)) = self.links.last_mut().and_then(Option::take) {
                    let offset = source_offset(source, &dest, range);
                    self.literal(" (");
                    self.safe(&dest, offset, false);
                    self.literal(")");
                }
                self.links.pop();
            }
            TagEnd::Image => {
                self.literal("]");
                self.images -= 1;
            }
            _ => (),
        }
    }

    fn open_line(&mut self) {
        if self.open.is_some() {
            return;
        }
        let mut prefix = Prefix {
            quote: self.quotes != 0,
            indent: self.lists.len().saturating_sub(1).min(MAX_INDENT / 2) * 2,
            item: None,
            show_marker: false,
        };
        if let Some(item) = self.lists.last_mut().and_then(|list| list.item.as_mut()) {
            prefix.item = Some(item.marker.clone());
            prefix.show_marker = std::mem::take(&mut item.pending);
        }
        let start = self.doc.text.len();
        let spans = self.doc.spans.len();
        if std::mem::take(&mut self.block_gap) {
            self.doc.lines.push(Line {
                text: start..start,
                spans: spans..spans,
                prefix: Prefix {
                    quote: false,
                    indent: 0,
                    item: None,
                    show_marker: false,
                },
                verbatim: false,
            });
        }
        self.open = Some(Line {
            text: start..start,
            spans: spans..spans,
            prefix,
            verbatim: self.code_block,
        });
    }

    fn end_block(&mut self) {
        self.finish_line(false);
        self.block_gap = !self.doc.lines.is_empty();
    }

    fn finish_line(&mut self, force: bool) {
        if force {
            self.open_line();
        }
        if let Some(mut line) = self.open.take() {
            line.text.end = self.doc.text.len();
            line.spans.end = self.doc.spans.len();
            self.doc.lines.push(line);
        }
    }

    fn finish_item_line(&mut self) {
        let pending = self
            .lists
            .last()
            .and_then(|list| list.item.as_ref())
            .is_some_and(|item| item.pending);
        self.finish_line(pending);
    }

    fn style(&self, inline_code: bool) -> Style {
        Style {
            bold: self.bold != 0,
            italic: self.italic != 0,
            inline_code,
            fenced_code: self.code_block,
            quote: self.quotes != 0,
            link: !self.links.is_empty(),
        }
    }

    fn literal(&mut self, value: &str) {
        self.append(value, self.style(false), None);
    }

    fn safe(&mut self, value: &str, source: Option<usize>, inline_code: bool) {
        let style = self.style(inline_code);
        let mut start = 0;
        for (offset, escaped) in value.char_indices().filter(|&(_, c)| unsafe_for_display(c)) {
            self.append(&value[start..offset], style, source.map(|s| s + start));
            self.open_line();
            let from = self.doc.text.len();
            self.doc.text.extend(escaped.escape_unicode());
            self.record_span(from, style, None);
            start = offset + escaped.len_utf8();
        }
        self.append(&value[start..], style, source.map(|s| s + start));
    }

    fn append(&mut self, value: &str, style: Style, source: Option<usize>) {
        if value.is_empty() {
            return;
        }
        self.open_line();
        let start = self.doc.text.len();
        self.doc.text.push_str(value);
        self.record_span(start, style, source);
    }

    fn record_span(&mut self, start: usize, style: Style, source: Option<usize>) {
        let end = self.doc.text.len();
        let first = self
            .open
            .as_ref()
            .expect("span in an open line")
            .spans
            .start;
        if let Some(previous) = self.doc.spans[first..].last_mut()
            && previous.style == style
            && previous.source.map(|s| s + previous.text.len()) == source
        {
            previous.text.end = end;
        } else {
            self.doc.spans.push(Span {
                text: start..end,
                style,
                source,
            });
        }
    }
}

fn source_offset(source: &str, value: &CowStr<'_>, range: Range<usize>) -> Option<usize> {
    // Borrowed parser strings may be just the content inside the event, e.g.
    // an inline code span without its backticks. Only map the actual subslice.
    if let CowStr::Borrowed(value) = value
        && let Some(start) = value.as_ptr().addr().checked_sub(source.as_ptr().addr())
        && (range.start..=range.end).contains(&start)
        && value.len() <= range.end - start
    {
        return Some(start);
    }
    (source.get(range.clone()) == Some(value.as_ref())).then_some(range.start)
}

fn unsafe_for_display(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{10eeee}' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_width;

    fn text(row: &Row<'_>) -> String {
        row.runs.iter().map(|run| run.text).collect()
    }

    #[test]
    fn styled_source_ranges_follow_reflow_and_transformation() {
        let source = "> 3. **ping *équipe*** `@agent` &amp; [run](https://example.test)\n> 4. next\n\n```rs\n  code\n\n  more\n```";
        let doc = Document::parse(source);
        let rows = doc.wrap(80);
        assert_eq!(
            text(&rows[0]),
            "> 3. ping équipe @agent & run (https://example.test)"
        );
        assert_eq!(text(&rows[1]), "> 4. next");
        assert_eq!(
            rows[2..].iter().map(text).collect::<Vec<_>>(),
            ["", "  code", "", "  more"]
        );
        for (label, style) in [
            (
                "équipe",
                Style {
                    bold: true,
                    italic: true,
                    quote: true,
                    ..Style::default()
                },
            ),
            (
                "@agent",
                Style {
                    inline_code: true,
                    quote: true,
                    ..Style::default()
                },
            ),
            (
                "run",
                Style {
                    link: true,
                    quote: true,
                    ..Style::default()
                },
            ),
            (
                "  code",
                Style {
                    fenced_code: true,
                    ..Style::default()
                },
            ),
        ] {
            let run = rows
                .iter()
                .flat_map(|row| &row.runs)
                .find(|r| r.text == label)
                .unwrap();
            assert_eq!(run.style, style);
            assert_eq!(&source[run.source.clone().unwrap()], label);
        }
        for columns in [1, 4, 8, 17] {
            for row in doc.wrap(columns) {
                assert!(row.runs.iter().map(|r| text_width(r.text)).sum::<usize>() <= columns);
                for run in row.runs {
                    if let Some(range) = run.source {
                        assert_eq!(&source[range], run.text);
                    }
                }
            }
        }
    }

    #[test]
    fn block_spacing_keeps_tight_lists_and_literal_code_rows() {
        let doc = Document::parse(concat!(
            "\n\n# Report\n\nFirst paragraph\nwrapped in source.\n\nSecond paragraph\n\n",
            "- tight one\n- tight two\n\n> quoted one\n>\n> quoted two\n\n",
            "```\ncode one\n\n\ncode two\n```\n\n",
            "1. loose one\n\n2. loose two\n\nTail\n\n",
        ));
        assert_eq!(
            doc.wrap(80).iter().map(text).collect::<Vec<_>>(),
            [
                "Report",
                "",
                "First paragraph",
                "wrapped in source.",
                "",
                "Second paragraph",
                "",
                "- tight one",
                "- tight two",
                "",
                "> quoted one",
                "",
                "> quoted two",
                "",
                "code one",
                "",
                "",
                "code two",
                "",
                "1. loose one",
                "",
                "2. loose two",
                "",
                "Tail",
            ],
        );
    }

    #[test]
    fn markdown_keeps_host_controls_and_remote_images_inert() {
        let doc = Document::parse(
            "<img src=\"bad\">\n\nA &#27;\u{10eeee}\u{202e} ![alt](https://hidden.test) [link](<javascript:&#7;hi>)",
        );
        let rows = doc.wrap(100);
        assert_eq!(rows.len(), 1);
        let row = text(&rows[0]);
        assert!(row.contains(r"A \u{1b}\u{10eeee}\u{202e} [image: alt] link (javascript:\u{7}hi)"));
        assert!(!row.contains("hidden.test"));
        assert!(row.chars().all(|c| !unsafe_for_display(c)));
    }

    #[test]
    fn reflow_keeps_cross_span_graphemes_and_narrow_nested_body() {
        let source = format!("{}**e**\u{301}界x", "> ".repeat(512));
        let doc = Document::parse(&source);
        let wide = doc.wrap(12);
        assert_eq!(wide[0].runs[1].text, "e\u{301}");
        assert!(wide[0].runs[1].style.bold);
        assert_eq!(wide[0].runs[1].source, None);
        let narrow = doc.wrap(1);
        assert_eq!(
            narrow.iter().map(text).collect::<Vec<_>>(),
            ["e\u{301}", "�", "x"]
        );
        assert_eq!(
            doc.wrap(2).iter().map(text).collect::<Vec<_>>(),
            ["e\u{301}", "界", "x"]
        );
    }

    #[test]
    fn a_narrow_layout_serves_tail_rows_without_materializing_offscreen_runs() {
        let prefix = "x".repeat(4096);
        let source = format!("{prefix}\n\n> **e**\u{301}界y");
        let doc = Document::parse(&source);
        let mut layout = Layout::default();
        doc.layout(1, &mut layout);
        assert_eq!(layout.len(), prefix.len() + 4);

        let row = doc.row(&layout, prefix.len() - 1).unwrap();
        assert_eq!(row.join_spaces, None);
        assert_eq!(row.runs().next().unwrap().source, Some(4095..4096));
        let row = doc.row(&layout, prefix.len()).unwrap();
        assert!(row.runs().next().is_none());
        let row = doc.row(&layout, prefix.len() + 1).unwrap();
        assert_eq!(row.join_spaces, Some(0));
        let run = row.runs().next().unwrap();
        assert_eq!(run.text, "e\u{301}");
        assert!(run.style.bold && run.style.quote && run.source.is_none());
        let replacement = doc
            .row(&layout, prefix.len() + 2)
            .unwrap()
            .runs()
            .next()
            .unwrap();
        assert_eq!(replacement.text, "�");
        assert!(replacement.source.is_none());

        doc.layout(0, &mut layout);
        assert!(layout.is_empty() && doc.row(&layout, 0).is_none());
    }

    #[test]
    fn word_breaks_never_trim_code_spaces_or_cut_a_grapheme() {
        let doc = Document::parse("`a  b`\n\nx  \u{301}z");
        let rows = doc.wrap(1);
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            ["a", " ", " ", "b", "", "x", " \u{301}", "z"]
        );
    }
}
