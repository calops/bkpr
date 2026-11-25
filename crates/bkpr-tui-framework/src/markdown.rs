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
        if columns == 0 {
            return Vec::new();
        }
        let mut rows = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let mut spans = &self.spans[line.spans.clone()];
            let mut start = line.text.start;
            loop {
                let mut runs = Vec::new();
                let first = start == line.text.start;
                let prefix = self.prefix(&line.prefix, first, columns, &mut runs);
                let rest = &self.text[start..line.text.end];
                let cut = row_cut(rest, columns - prefix, line.verbatim, start, spans);
                if cut.replace {
                    runs.push(Run {
                        text: "�",
                        style: span_at(&mut spans, start).style,
                        source: None,
                    });
                } else {
                    self.row_runs(start..start + cut.end, &mut spans, &mut runs);
                }
                rows.push(Row {
                    runs,
                    continuation_prefix: if first { 0 } else { prefix },
                    join_spaces: (cut.next < rest.len()).then_some(cut.next - cut.end),
                });
                start += cut.next;
                if start == line.text.end {
                    break;
                }
            }
        }
        rows
    }

    fn prefix<'a>(
        &'a self,
        prefix: &Prefix,
        first: bool,
        columns: usize,
        runs: &mut Vec<Run<'a>>,
    ) -> usize {
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
        let style = Style {
            quote: prefix.quote,
            ..Style::default()
        };
        for text in [
            quote,
            &SPACES[..indent],
            if first && prefix.show_marker {
                marker
            } else {
                &SPACES[..marker.len()]
            },
        ] {
            if !text.is_empty() {
                runs.push(Run {
                    text,
                    style,
                    source: None,
                });
            }
        }
        quote.len() + indent + marker.len()
    }

    fn row_runs<'a>(&'a self, range: Range<usize>, spans: &mut &[Span], runs: &mut Vec<Run<'a>>) {
        let mut offset = range.start;
        let mut boundary = offset;
        let mut graphemes = self.text[range.clone()].graphemes(true);
        while offset < range.end {
            let span = span_at(spans, offset);
            let limit = span.text.end.min(range.end);
            if limit == range.end {
                runs.push(self.run(span, offset..limit));
                break;
            }
            let mut aligned = offset;
            while boundary <= limit {
                aligned = boundary;
                boundary += graphemes.next().expect("span ends inside the row").len();
            }
            if aligned > offset {
                runs.push(self.run(span, offset..aligned));
            }
            offset = aligned;
            if aligned < limit {
                // Markdown delimiters can separate a base and its marks. Keep
                // the complete displayed grapheme under the first span's style.
                runs.push(Run {
                    text: &self.text[aligned..boundary],
                    style: span.style,
                    source: None,
                });
                offset = boundary;
            }
        }
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
    fn word_breaks_never_trim_code_spaces_or_cut_a_grapheme() {
        let doc = Document::parse("`a  b`\n\nx  \u{301}z");
        let rows = doc.wrap(1);
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            ["a", " ", " ", "b", "", "x", " \u{301}", "z"]
        );
    }
}
