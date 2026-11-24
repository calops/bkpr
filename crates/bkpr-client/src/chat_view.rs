use super::{Anchor, Chat, Draft, Focus, Send, chat_images};
use bkpr_protocol::{
    ColorSource, Cursor, CursorShape, Frame, SelectionSpan, Size, Style, channels::*,
};
use bkpr_tui_framework::{
    Rect, Role, Theme, UiSurface, clear, graphics_frame, markdown, safe_text, text, text_width,
    window,
};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

const MAX_NICK: u16 = 16;
const LIST_MIN_WIDTH: u16 = 55;

pub(in crate::app) struct Entry {
    pub(super) message: ChannelMessage,
    document: markdown::Document,
    width: u16,
    rows: Vec<WrappedRow>,
}

struct Run {
    text: String,
    style: markdown::Style,
}

struct WrappedRow {
    runs: Vec<Run>,
    continuation_prefix: u16,
    join_spaces: Option<usize>,
}

impl WrappedRow {
    fn glyphs(&self, x: u16) -> impl Iterator<Item = (u16, u16, &str)> {
        self.runs
            .iter()
            .flat_map(|run| run.text.graphemes(true))
            .scan(x, |col, glyph| {
                let start = *col;
                *col += glyph.width() as u16;
                Some((start, *col, glyph))
            })
    }
}

impl Entry {
    pub(super) fn new(message: ChannelMessage) -> Self {
        let document = markdown::Document::parse(&message.text);
        Self {
            message,
            document,
            width: 0,
            rows: Vec::new(),
        }
    }

    pub(super) fn sequence(&self) -> u64 {
        self.message.message.sequence
    }

    fn wrap(&mut self, width: u16) {
        let width = width.max(1);
        if self.width == width {
            return;
        }
        self.width = width;
        self.rows = self
            .document
            .wrap(width.into())
            .into_iter()
            .map(|row| WrappedRow {
                runs: row
                    .runs
                    .into_iter()
                    .map(|run| Run {
                        text: run.text.to_owned(),
                        style: run.style,
                    })
                    .collect(),
                continuation_prefix: row.continuation_prefix as u16,
                join_spaces: row.join_spaces,
            })
            .collect();
    }

    fn meta_rows(&self) -> usize {
        let meta = &self.message.message;
        usize::from(meta.intent != PostIntent::Note)
            + usize::from(meta.reply_to.is_some())
            + usize::from(!meta.mentions.is_empty())
    }

    fn len(&self, compact: bool, graphics: bool) -> usize {
        usize::from(compact)
            + self.meta_rows()
            + self.rows.len().max(1)
            + self.message.attachments.len()
                * (1 + if graphics {
                    chat_images::PREVIEW_ROWS as usize
                } else {
                    0
                })
    }
}

#[derive(Clone, Copy)]
pub(super) enum Hit {
    Channel(usize),
    List,
    History(Option<usize>),
    Composer,
    Mention(usize),
    Latest,
    Create,
    Manage,
}

struct Range {
    sequence: u64,
    start: usize,
    len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HistoryRow {
    sequence: u64,
    offset: usize,
    y: u16,
    x: u16,
    prefix: u16,
    end: u16,
}

impl HistoryRow {
    fn text(self, entries: &VecDeque<Entry>) -> Option<&WrappedRow> {
        let entry = entries
            .binary_search_by_key(&self.sequence, Entry::sequence)
            .ok()?;
        entries[entry].rows.get(self.offset)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    row: usize,
    start: u16,
    end: u16,
}

struct HistorySelection {
    anchor: Point,
    head: Point,
    dragging: bool,
}

impl HistorySelection {
    fn bounds(&self) -> (Point, Point) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

pub(super) struct Paint {
    pub(super) surface: UiSurface,
    pub(super) rendered: Vec<u64>,
    hits: Vec<(Rect, Hit)>,
    hovered_channel: Option<ChannelId>,
    inlays: Vec<(Rect, Rect, Option<u8>)>,
    inlay_mask: Option<u8>,
    outer: Rect,
    images: Frame,
    history: Rect,
    composer_width: u16,
    ranges: Vec<Range>,
    start: usize,
    tail: usize,
    key: Option<PaintKey>,
    rows: Vec<HistoryRow>,
    previous_rows: Vec<HistoryRow>,
    selection: Option<HistorySelection>,
    selection_spans: Vec<SelectionSpan>,
    copy_selection: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PaintKey {
    outer: Rect,
    size: Size,
    theme: u64,
    read_only: bool,
    graphics: bool,
    focused: bool,
    backgrounds: [ColorSource; 3],
}

impl Default for Paint {
    fn default() -> Self {
        let initial = Size {
            cols: 1,
            rows: 1,
            cell_width: 1,
            cell_height: 1,
        };
        Self {
            surface: UiSurface::new(initial),
            rendered: Vec::new(),
            hits: Vec::new(),
            hovered_channel: None,
            inlays: Vec::with_capacity(2),
            inlay_mask: None,
            outer: Rect::default(),
            images: graphics_frame(initial),
            history: Rect::default(),
            composer_width: 1,
            ranges: Vec::new(),
            start: 0,
            tail: 0,
            key: None,
            rows: Vec::new(),
            previous_rows: Vec::new(),
            selection: None,
            selection_spans: Vec::new(),
            copy_selection: false,
        }
    }
}

impl Paint {
    pub(super) fn invalidate_hits(&mut self) {
        self.hits.clear();
        self.hovered_channel = None;
        self.rendered.clear();
        self.ranges.clear();
        self.rows.clear();
        self.clear_selection();
        self.key = None;
    }

    pub(super) fn hit(&self, x: u16, y: u16) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, hit)| *hit)
    }

    pub(super) fn page_rows(&self) -> u16 {
        self.history.height.saturating_sub(1)
    }
    pub(super) fn composer_width(&self) -> u16 {
        self.composer_width
    }

    pub(super) fn scroll(&mut self, delta: isize) -> Option<(Anchor, bool)> {
        let start = self.start.saturating_add_signed(delta).min(self.tail);
        let index = self
            .ranges
            .partition_point(|range| range.start <= start)
            .checked_sub(1)?;
        let range = &self.ranges[index];
        let anchor = Anchor {
            sequence: range.sequence,
            line: start - range.start,
        };
        let boundary = if delta < 0 {
            start == 0
        } else {
            start == self.tail
        };
        if start != self.start {
            self.clear_selection();
        }
        self.start = start;
        Some((anchor, boundary))
    }

    fn begin(&mut self, key: PaintKey, theme: &Theme) {
        std::mem::swap(&mut self.rows, &mut self.previous_rows);
        self.rows.clear();
        self.key = Some(key);
        self.outer = key.outer;
        self.history = Rect::default();
        self.composer_width = 1;
        self.ranges.clear();
        self.rendered.clear();
        self.hits.clear();
        self.inlays.clear();
        self.inlay_mask = None;
        self.images.images.clear();
        self.images.placements.clear();
        self.start = 0;
        self.tail = 0;
        self.surface.paint(key.size, Vec::new(), None);
        let frame = &mut self.surface.frame;
        clear(
            frame,
            Rect {
                x: 0,
                y: 0,
                width: key.size.cols,
                height: key.size.rows,
            },
        );
        let (fg, bg) = theme.defaults();
        frame.foreground = fg;
        frame.background = bg;
        frame.palette.clear();
        let style = card_style(theme, Role::Base, key.backgrounds[2]);
        for cell in frame.rows.iter_mut().flat_map(|row| &mut row.cells) {
            cell.style.clone_from(&style);
        }
    }

    fn line(&mut self, theme: &Theme, rect: Rect, value: &str, role: Role, selected: bool) {
        self.styled_line(rect, value, role_style(theme, role, selected), selected);
    }

    fn card_line(
        &mut self,
        theme: &Theme,
        rect: Rect,
        value: &str,
        role: Role,
        selected: bool,
        background: ColorSource,
    ) {
        if selected {
            self.styled_line(rect, value, theme.selected_style(role), true);
        } else {
            self.styled_line(rect, value, &card_style(theme, role, background), false);
        }
    }

    fn styled_line(&mut self, rect: Rect, value: &str, style: &Style, selected: bool) {
        if rect.height > 0 && rect.width > 0 {
            let frame = &mut self.surface.frame;
            let value = window::fit(value, rect.width);
            text(frame, rect.x, rect.y, &value, selected);
            style_cells(
                frame,
                rect.x,
                rect.y,
                text_width(&value) as u16,
                style,
                None,
            );
        }
    }

    fn fill_card(&mut self, theme: &Theme, body: Rect, background: ColorSource) {
        let style = card_style(theme, Role::Base, background);
        for y in body.y..body.y + body.height {
            style_cells(&mut self.surface.frame, body.x, y, body.width, &style, None);
        }
    }

    fn inlay(
        &mut self,
        theme: &Theme,
        outer: Rect,
        background: ColorSource,
        accent: Option<u8>,
    ) -> Rect {
        let mut body = window::content(outer.size(self.surface.frame.size));
        body.x += outer.x;
        body.y += outer.y;
        if body != outer {
            self.inlays.push((outer, body, accent));
        }
        self.fill_card(theme, body, background);
        body
    }

    fn channel_controls(
        &mut self,
        theme: &Theme,
        body: Rect,
        background: ColorSource,
        settings: bool,
        create: bool,
    ) {
        let (settings, create) = match (settings, create, body.width) {
            (true, true, 18..) => ("[Settings]", "[+ New]"),
            (true, true, 13..) => ("Settings", "+New"),
            (true, true, 7..) => ("[S]", "[+]"),
            (true, true, _) => ("S", "+"),
            (true, false, 10..) => ("[Settings]", ""),
            (true, false, _) => ("S", ""),
            (false, true, 7..) => ("", "[+ New]"),
            (false, true, _) => ("", "+"),
            (false, false, _) => ("", ""),
        };
        let mut remaining = body.width;
        for (label, hit) in [(create, Hit::Create), (settings, Hit::Manage)] {
            let width = (label.len() as u16).min(remaining);
            if width > 0 {
                remaining -= width;
                let bounds = row(body.x + remaining, body.y, width);
                self.card_line(theme, bounds, label, Role::Info, false, background);
                self.target(bounds, hit);
                if remaining > settings.len() as u16 || matches!(hit, Hit::Manage) {
                    remaining = remaining.saturating_sub(1);
                }
            }
        }
        if remaining >= 8 {
            self.card_line(
                theme,
                row(body.x, body.y, remaining),
                "Channels",
                Role::Title,
                false,
                background,
            );
        }
    }

    fn channels_button(
        &mut self,
        theme: &Theme,
        bounds: Rect,
        label: &str,
        background: Option<ColorSource>,
    ) {
        if let Some(background) = background {
            self.card_line(theme, bounds, label, Role::Info, false, background);
        } else {
            self.line(theme, bounds, label, Role::Info, false);
        }
        self.target(bounds, Hit::List);
    }

    fn target(&mut self, rect: Rect, hit: Hit) {
        if rect.height > 0 && rect.width > 0 {
            self.hits.push((rect, hit));
        }
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_spans.clear();
        self.copy_selection = false;
    }

    fn selection_point(
        &self,
        entries: &VecDeque<Entry>,
        x: u16,
        y: u16,
        begin: bool,
    ) -> Option<Point> {
        let index = match self.rows.binary_search_by_key(&y, |row| row.y) {
            Ok(index) => index,
            Err(_) if begin => return None,
            Err(index) => {
                match (
                    index.checked_sub(1).and_then(|i| self.rows.get(i)),
                    self.rows.get(index),
                ) {
                    (Some(before), Some(after)) if y - before.y > after.y - y => index,
                    (Some(_), _) => index - 1,
                    (None, Some(_)) => index,
                    (None, None) => return None,
                }
            }
        };
        let row = self.rows[index];
        let left = row.x + row.prefix;
        if begin && (!self.history.contains(x, y) || x < left) {
            return None;
        }
        if left == row.end {
            return Some(Point {
                row: index,
                start: left,
                end: left,
            });
        }
        let x = x.clamp(left, row.end - 1);
        let (start, end, _) = row
            .text(entries)?
            .glyphs(row.x)
            .find(|&(_, end, _)| x < end)?;
        Some(Point {
            row: index,
            start,
            end,
        })
    }

    fn update_selection_spans(&mut self) {
        self.selection_spans.clear();
        let Some(selection) = self.selection.as_ref() else {
            return;
        };
        let (lo, hi) = selection.bounds();
        if lo == hi {
            return;
        }
        for (index, row) in self.rows.iter().enumerate().take(hi.row + 1).skip(lo.row) {
            let (start, end) = row.selected_columns(index, lo, hi);
            if start < end {
                self.selection_spans.push(SelectionSpan {
                    row: row.y,
                    start,
                    end,
                });
            }
        }
    }
}

impl HistoryRow {
    fn selected_columns(self, index: usize, lo: Point, hi: Point) -> (u16, u16) {
        let start = if lo.row == index {
            lo.start
        } else {
            self.x + self.prefix
        };
        let end = if hi.row == index { hi.end } else { self.end };
        (start, end)
    }
}

impl Chat {
    pub(in crate::app) fn inlays(&self) -> &[(Rect, Rect, Option<u8>)] {
        &self.paint.inlays
    }

    pub(in crate::app) fn set_graphical_inlays(&mut self, mask: u8, theme: &Theme) {
        let paint = &mut self.paint;
        if paint.inlay_mask == Some(mask) {
            return;
        }
        paint.inlay_mask = Some(mask);
        let parent = paint.key.expect("painted chat").backgrounds[2];
        let mut border_rows = Vec::new();
        let mut plain = [Rect::default(); 3];
        let mut count = 0;
        for (index, &(outer, _, _)) in paint.inlays.iter().enumerate() {
            if mask & (1 << index) == 0 {
                plain[count] = outer;
                count += 1;
            }
            let bottom = outer.y + outer.height - 1;
            let right = outer.x + outer.width - 1;
            border_rows.extend([
                row(outer.x, outer.y, outer.width),
                row(outer.x, bottom, outer.width),
            ]);
            for y in outer.y + 1..bottom {
                border_rows.extend([row(outer.x, y, 1), row(right, y, 1)]);
            }
        }
        for rect in border_rows {
            clear(&mut paint.surface.frame, rect);
            paint.fill_card(theme, rect, parent);
        }
        let mut lines = Vec::new();
        window::tiled_border_rows(paint.surface.frame.size, &plain[..count], None, &mut lines);
        for (rect, label, _) in lines {
            paint.card_line(theme, rect, &label, Role::Border, false, parent);
        }
    }
    pub(in crate::app) fn redraw(&mut self) {
        self.paint.key = None;
    }

    pub(in crate::app) fn hover_channel(&mut self, pointer: Option<(u16, u16)>) {
        let hovered = pointer.and_then(|(x, y)| {
            let outer = self.paint.outer;
            if outer.contains(x, y)
                && self.editor.is_none()
                && let Some(Hit::Channel(index)) = self.paint.hit(x - outer.x, y - outer.y)
            {
                self.ids.get(index)
            } else {
                None
            }
        });
        if self.paint.hovered_channel.as_ref() != hovered {
            self.paint.hovered_channel = hovered.cloned();
            self.redraw();
        }
    }

    pub(in crate::app) fn geometry(&self) -> Rect {
        self.paint.outer
    }

    pub(in crate::app) fn image_surface(&self) -> Option<(Rect, &Frame)> {
        if self.paint.images.placements.is_empty() {
            return None;
        }
        let history = self.paint.history;
        Some((
            Rect {
                x: self.paint.outer.x + history.x,
                y: self.paint.outer.y + history.y,
                ..history
            },
            &self.paint.images,
        ))
    }

    /// Hit only text in the current painted channel history, leaving typing to the composer.
    pub(in crate::app) fn begin_history_selection(
        &mut self,
        outer: Rect,
        screen: Size,
        x: u16,
        y: u16,
    ) -> bool {
        self.clear_history_selection();
        let Some(anchor) = self.history_point(outer, screen, x, y, true) else {
            return false;
        };
        self.paint.selection = Some(HistorySelection {
            anchor,
            head: anchor,
            dragging: true,
        });
        if self.focus != Some(Focus::Composer) {
            self.focus = Some(Focus::Composer);
            self.redraw();
        }
        true
    }

    /// Continue a captured gesture; a nonempty release authorizes automatic copy.
    pub(in crate::app) fn drag_history_selection(
        &mut self,
        outer: Rect,
        screen: Size,
        x: u16,
        y: u16,
        release: bool,
    ) -> bool {
        if !self
            .paint
            .selection
            .as_ref()
            .is_some_and(|selection| selection.dragging)
        {
            return false;
        }
        let Some(head) = self.history_point(outer, screen, x, y, false) else {
            self.clear_history_selection();
            return false;
        };
        let selection = self.paint.selection.as_mut().expect("captured selection");
        selection.dragging = !release;
        if selection.head != head {
            selection.head = head;
            self.paint.update_selection_spans();
        }
        release && self.can_copy_history()
    }

    fn history_point(
        &self,
        outer: Rect,
        screen: Size,
        x: u16,
        y: u16,
        begin: bool,
    ) -> Option<Point> {
        let paint = &self.paint;
        if self.editor.is_some()
            || paint.outer != outer
            || paint.surface.frame.size != outer.size(screen)
        {
            return None;
        }
        paint.selection_point(
            &self.active.as_ref()?.history.entries,
            x.saturating_sub(outer.x),
            y.saturating_sub(outer.y),
            begin,
        )
    }

    /// The compositor applies the same selection annotation as for native panes.
    pub(in crate::app) fn history_selection_spans(&self) -> &[SelectionSpan] {
        &self.paint.selection_spans
    }

    /// Discard the range and any export not yet written by the terminal loop.
    pub(in crate::app) fn clear_history_selection(&mut self) {
        self.paint.clear_selection();
    }

    /// A completed and still visible nonempty body range can be copied again.
    pub(in crate::app) fn can_copy_history(&self) -> bool {
        self.is_attending()
            && !self.paint.selection_spans.is_empty()
            && self
                .paint
                .selection
                .as_ref()
                .is_some_and(|selection| !selection.dragging)
    }

    /// Retain one local copy request until the current projection has been painted.
    pub(in crate::app) fn request_history_copy(&mut self) {
        self.paint.copy_selection = self.can_copy_history();
    }

    /// Drop the queued write without changing the retained selection.
    pub(in crate::app) fn cancel_history_copy(&mut self) {
        self.paint.copy_selection = false;
    }

    /// Take the rendered text, preserving logical row joins without copying gutters.
    pub(in crate::app) fn take_history_copy(
        &mut self,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        if !std::mem::take(&mut self.paint.copy_selection) || !self.can_copy_history() {
            return Ok(None);
        }
        let Some(entries) = self.active.as_ref().map(|active| &active.history.entries) else {
            return Ok(None);
        };
        let (lo, hi) = self
            .paint
            .selection
            .as_ref()
            .expect("completed selection")
            .bounds();
        let mut result = Vec::new();
        let mut previous: Option<(HistoryRow, Option<usize>)> = None;
        for (index, &row) in self
            .paint
            .rows
            .iter()
            .enumerate()
            .take(hi.row + 1)
            .skip(lo.row)
        {
            let Some(text) = row.text(entries) else {
                return Ok(None);
            };
            if let Some((prior, join_spaces)) = previous {
                let spaces = join_spaces
                    .filter(|_| prior.sequence == row.sequence && prior.offset + 1 == row.offset);
                let count = spaces.unwrap_or(1);
                check_copy_size(result.len() + count, limit)?;
                result.resize(
                    result.len() + count,
                    if spaces.is_some() { b' ' } else { b'\n' },
                );
            }
            let (start, end) = row.selected_columns(index, lo, hi);
            for (left, right, glyph) in text.glyphs(row.x) {
                if right > end {
                    break;
                }
                if left >= start && (left < right || left > start) {
                    check_copy_size(result.len() + glyph.len(), limit)?;
                    result.extend_from_slice(glyph.as_bytes());
                }
            }
            previous = Some((row, text.join_spaces));
        }
        Ok(Some(result))
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn paint(
        &mut self,
        outer: Rect,
        screen: Size,
        theme: &Theme,
        read_only: bool,
        graphics: bool,
        focused: bool,
        backgrounds: [ColorSource; 3],
    ) {
        let size = outer.size(screen);
        let key = PaintKey {
            outer,
            size,
            theme: theme.revision(),
            read_only,
            graphics,
            focused,
            backgrounds,
        };
        if self.paint.key == Some(key) {
            return;
        }
        let geometry_changed = self.paint.outer != outer || self.paint.surface.frame.size != size;
        if geometry_changed {
            self.paint.hovered_channel = None;
        }
        let old_history = self.paint.history;
        self.paint.begin(key, theme);
        self.paint_contents(screen, key, theme);
        if geometry_changed
            || self.paint.history != old_history
            || self.paint.rows != self.paint.previous_rows
        {
            self.paint.clear_selection();
        }
    }

    fn paint_contents(&mut self, screen: Size, key: PaintKey, theme: &Theme) {
        let PaintKey {
            outer,
            size,
            read_only,
            graphics,
            focused,
            backgrounds: [list_background, composer_background, _],
            ..
        } = key;
        self.images.start_frame();
        self.images_wanted.clear();
        let paint = &mut self.paint;
        if let Some(editor) = &mut self.editor {
            editor
                .form
                .paint(&mut paint.surface, size, theme, !read_only);
            return;
        }
        if let Some(active) = &mut self.active {
            active.history.visible_through = 0;
        }
        if outer.width == 0 || outer.height == 0 {
            return;
        }
        let full = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        let split = (size.cols >= LIST_MIN_WIDTH)
            .then(|| {
                crate::tiling::partition_at(
                    full,
                    bkpr_protocol::Axis::Horizontal,
                    (size.cols / 4).clamp(15, 24) - 1,
                )
            })
            .flatten();
        let channels_wide = split.is_some();
        let list_only = !channels_wide && self.focus == Some(Focus::Channels);
        let (list, thread_column) = split
            .map_or((list_only.then_some(full), full), |(list, _, right)| {
                (Some(list), right)
            });
        let summary = self
            .active
            .as_ref()
            .and_then(|active| self.catalog.get(&active.id));
        let writing = !read_only && summary.is_some_and(|summary| !summary.direct);
        let can_create = !read_only && self.catalog_ready && self.catalog_error.is_none();
        if let Some(outer) = list {
            let body = paint.inlay(theme, outer, list_background, None);
            paint.target(outer, Hit::List);
            paint.channel_controls(theme, body, list_background, writing, can_create);
            let list_height = body.height.saturating_sub(1);
            let selected = self.active.as_ref().map(|active| &active.id);
            let offset = self
                .list_cursor
                .saturating_sub(list_height.saturating_sub(1) as usize);
            for (line, (index, id)) in self
                .ids
                .iter()
                .enumerate()
                .skip(offset)
                .take(list_height.into())
                .enumerate()
            {
                let summary = &self.catalog[id];
                let lead = match (selected == Some(id), summary.unread_attention) {
                    (true, true) => "•!",
                    (true, false) => "• ",
                    (false, true) => "! ",
                    (false, false) => "  ",
                };
                let name = channel_name(summary);
                let full = format!("{lead}{name}");
                let bounds = row(body.x, body.y + line as u16 + 1, body.width);
                let chosen = self.focus == Some(Focus::Channels) && self.list_cursor == index;
                let role = if summary.unread_attention {
                    Role::Warning
                } else if selected == Some(id) {
                    Role::Accent
                } else {
                    Role::Base
                };
                if paint.hovered_channel.as_ref() == Some(id) {
                    let hover = crate::appearance::hover_background(theme, list_background);
                    paint.fill_card(theme, bounds, hover);
                    paint.card_line(theme, bounds, &full, role, false, hover);
                } else {
                    paint.card_line(theme, bounds, &full, role, chosen, list_background);
                }
                paint.target(bounds, Hit::Channel(index));
            }
        }
        if list_only {
            return;
        }
        let left = thread_column.x;
        let columns = thread_column.width;
        let (Some(active), Some(summary)) = (&mut self.active, summary) else {
            let thread = paint.inlay(
                theme,
                Rect {
                    x: left,
                    y: 0,
                    width: columns,
                    height: size.rows,
                },
                list_background,
                None,
            );
            let reserve = if channels_wide {
                0
            } else {
                let (label, reserve) = narrow_channels_button(thread.width, 8);
                paint.channels_button(
                    theme,
                    row(thread.x, thread.y, text_width(label) as u16),
                    label,
                    Some(list_background),
                );
                reserve
            };
            let (label, role) = if let Some(error) = &self.catalog_error {
                (error.as_str(), Role::Error)
            } else if self.catalog_ready {
                ("No channels", Role::Muted)
            } else {
                ("Loading channels…", Role::Muted)
            };
            paint.card_line(
                theme,
                row(thread.x + reserve, thread.y, thread.width - reserve),
                label,
                role,
                false,
                list_background,
            );
            return;
        };
        let draft = self.drafts.get(&active.id);
        let history = &mut active.history;
        let new = summary
            .last_sequence
            .saturating_sub(history.paused_at.unwrap_or_else(|| history.latest()));
        let status = if let Some(send) = self.sends.get(&active.id) {
            let (role, label) = match send {
                Send::Pending(_) => (Role::Info, "Sending…"),
                Send::Unknown(_) => (
                    Role::Error,
                    "Send outcome unknown · Ctrl-r retries the same post",
                ),
            };
            Some((role, Cow::Borrowed(label), None))
        } else if let Some(error) =
            draft.and_then(|draft| draft.error.as_deref().or(draft.input.error()))
        {
            Some((Role::Error, Cow::Borrowed(error), None))
        } else if let Some(error) = &history.error {
            Some((Role::Error, Cow::Borrowed(error.as_str()), None))
        } else if let Some((role, notice)) = &self.notice {
            Some((*role, Cow::Borrowed(notice.as_str()), None))
        } else if history.anchor.is_some() || new > 0 && history.loaded {
            Some((
                Role::Info,
                if new > 0 {
                    Cow::Owned(format!("{new} new · [Latest]"))
                } else {
                    Cow::Borrowed("Reading history · [Latest]")
                },
                Some(Hit::Latest),
            ))
        } else {
            None
        };
        let status_rows = u16::from(status.is_some() && size.rows > 1);
        let has_composer = writing && size.rows > 1 + status_rows;
        let inset = u16::from(has_composer && columns >= 5 && size.rows >= 6 + status_rows);
        let history_inset =
            u16::from(columns >= 3 && size.rows >= 3 && (!has_composer || inset != 0));
        let composer_columns = columns.saturating_sub(2 * inset);
        let composer_padding = u16::from(composer_columns >= 6);
        let (button, button_width) = if channels_wide {
            ("", 0)
        } else if has_composer {
            narrow_channels_button(composer_columns - composer_padding, 8)
        } else {
            narrow_channels_button(columns.saturating_sub(2 * history_inset), 34)
        };
        let history_indent = if has_composer { 0 } else { button_width };
        let (draft_lines, caret_line) = if has_composer {
            paint.composer_width = composer_columns
                .saturating_sub(button_width + composer_padding + 3)
                .max(1);
            draft.map_or_else(
                || (Vec::new(), 0),
                |draft| draft.lines(usize::from(paint.composer_width)),
            )
        } else {
            (Vec::new(), 0)
        };
        let composer_rows = if has_composer {
            (draft_lines.len().max(1) as u16)
                .min(4)
                .min(size.rows - 1 - status_rows - 2 * inset - history_inset)
        } else {
            0
        };
        let composer_at = size.rows - composer_rows - 2 * inset;
        let choices = if active.roster_ready {
            draft.map_or(0, |draft| draft.choices(&active.roster).count())
        } else {
            0
        };
        let completion_rows =
            if has_composer && draft.is_some_and(|draft| draft.completion.is_some()) {
                (choices.max(1) as u16)
                    .min(composer_at.saturating_sub(1 + status_rows + 2 * history_inset))
            } else {
                0
            };
        let (thread_outer, composer_outer) = if inset != 0 {
            let (thread, _, composer) = crate::tiling::partition_at(
                thread_column,
                bkpr_protocol::Axis::Vertical,
                composer_at,
            )
            .expect("representable composer split");
            (thread, composer)
        } else {
            (
                Rect {
                    height: composer_at,
                    ..thread_column
                },
                Rect {
                    y: composer_at,
                    height: composer_rows,
                    ..thread_column
                },
            )
        };
        let thread = if history_inset != 0 {
            paint.inlay(theme, thread_outer, list_background, None)
        } else {
            paint.fill_card(theme, thread_outer, list_background);
            thread_outer
        };
        let completion_at = thread.y + thread.height - completion_rows;
        let status_at = completion_at - status_rows;
        let history_rect = Rect {
            x: thread.x + history_indent,
            y: thread.y,
            width: thread.width - history_indent,
            height: status_at - thread.y,
        };
        if history_indent != 0 {
            paint.channels_button(
                theme,
                row(thread.x, thread.y, text_width(button) as u16),
                button,
                Some(list_background),
            );
        }
        paint.history = history_rect;
        paint.images.size = history_rect.size(screen);
        paint.target(history_rect, Hit::History(None));
        paint_history(
            paint,
            &mut history.entries,
            &mut self.images,
            &mut self.images_wanted,
            &active.id,
            history.anchor,
            theme,
            graphics,
        );
        history.visible_through = paint.rendered.last().copied().unwrap_or(0);
        if history.entries.is_empty() && history.error.is_none() && history_rect.height != 0 {
            let label = if history.loaded {
                "No messages yet"
            } else {
                "Loading history…"
            };
            let width = text_width(label) as u16;
            let x = thread.x + thread.width.saturating_sub(width) / 2;
            let y = (thread.y + (thread.height - 1) / 2).min(status_at - 1);
            let x = if y == thread.y {
                x.max(history_rect.x)
            } else {
                x
            };
            paint.card_line(
                theme,
                row(x, y, thread.x + thread.width - x),
                label,
                Role::Muted,
                false,
                list_background,
            );
        }
        if status_rows != 0
            && let Some((role, label, hit)) = status
        {
            let bounds = row(history_rect.x, status_at, history_rect.width);
            paint.card_line(theme, bounds, &label, role, false, list_background);
            if let Some(hit) = hit {
                paint.target(bounds, hit);
            }
        }
        if completion_rows > 0
            && let Some(draft) = draft
        {
            if choices == 0 {
                let label = active
                    .roster_error
                    .as_deref()
                    .unwrap_or(if active.roster_ready {
                        "No matching eligible agents · Esc cancels"
                    } else {
                        "Loading eligible agents…"
                    });
                paint.card_line(
                    theme,
                    row(thread.x, completion_at, thread.width),
                    label,
                    Role::Muted,
                    false,
                    list_background,
                );
            } else {
                for (i, (index, entry, chosen)) in draft
                    .choices(&active.roster)
                    .take(completion_rows.into())
                    .enumerate()
                {
                    let bounds = row(thread.x, completion_at + i as u16, thread.width);
                    let mut label = Draft::selection_label(entry, index);
                    if chosen
                        && let Some(scope) =
                            draft.completion_wake_preview(summary.settings.user_wake)
                        && text_width(&label) + text_width(scope) + 3 <= usize::from(thread.width)
                    {
                        label.push_str(" · ");
                        label.push_str(scope);
                    }
                    paint.card_line(
                        theme,
                        bounds,
                        &label,
                        Role::Secondary,
                        chosen,
                        list_background,
                    );
                    paint.target(bounds, Hit::Mention(index));
                }
            }
        }
        let background = card_style(theme, Role::Base, list_background);
        for y in history_rect.y..status_at {
            let cells = &mut paint.surface.frame.rows[usize::from(y)].cells;
            let x = usize::from(history_rect.x);
            for cell in &mut cells[x..x + usize::from(history_rect.width)] {
                cell.style.background_source = background.background_source;
                cell.style.background = background.background;
            }
        }
        if composer_rows > 0 {
            let outer = composer_outer;
            let body = if inset != 0 {
                paint.inlay(theme, outer, composer_background, Some(6))
            } else {
                paint.fill_card(theme, outer, composer_background);
                outer
            };
            let left = body.x + button_width + composer_padding;
            let columns = body.width - button_width - composer_padding;
            let composer = body.y;
            let enabled = !self.sends.contains_key(&active.id);
            let has_composer_focus = self.focus == Some(Focus::Composer) && focused && enabled;
            let visible_from = caret_line.saturating_sub(composer_rows.saturating_sub(1) as usize);
            paint.target(
                Rect {
                    x: left,
                    width: columns,
                    ..body
                },
                Hit::Composer,
            );
            if button_width > 0 {
                paint.channels_button(
                    theme,
                    row(body.x, composer, text_width(button) as u16),
                    button,
                    Some(composer_background),
                );
            }
            let reply = draft.and_then(|draft| draft.reply_to.as_ref());
            let (prompt, fallback) = if reply.is_some() {
                ("↪", ">")
            } else {
                ("›", ">")
            };
            let lead = if columns > 1 && text_width(prompt) == 1 {
                prompt
            } else {
                fallback
            };
            paint.card_line(
                theme,
                row(left, composer, 1.min(columns)),
                lead,
                Role::Accent,
                false,
                composer_background,
            );
            if let Some(draft) = draft {
                let mention_style = card_style(theme, Role::Secondary, composer_background);
                for (i, line) in draft_lines
                    .iter()
                    .enumerate()
                    .skip(visible_from)
                    .take(composer_rows.into())
                {
                    let y = composer + (i - visible_from) as u16;
                    let bounds = row(left.saturating_add(2), y, columns.saturating_sub(2));
                    paint.card_line(
                        theme,
                        bounds,
                        &draft.input.value[line.clone()],
                        Role::Base,
                        false,
                        composer_background,
                    );
                    for span in draft.highlighted(line) {
                        let x = bounds.x
                            + (text_width(&draft.input.value[line.start..span.start]) as u16)
                                .min(bounds.width);
                        let width = (text_width(&draft.input.value[span]) as u16)
                            .min(bounds.x.saturating_add(bounds.width).saturating_sub(x));
                        style_cells(&mut paint.surface.frame, x, y, width, &mention_style, None);
                    }
                }
            }
            if has_composer_focus && columns > 2 {
                let col = draft
                    .and_then(|draft| {
                        draft_lines.get(caret_line).map(|line| {
                            text_width(&draft.input.value[line.start..draft.input.cursor])
                        })
                    })
                    .unwrap_or(0);
                paint.surface.frame.cursor = Cursor {
                    x: left + 2 + col.min(usize::from(columns - 3)) as u16,
                    y: composer + (caret_line - visible_from) as u16,
                    visible: true,
                    blinking: false,
                    shape: CursorShape::Bar,
                    color: None,
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_history(
    paint: &mut Paint,
    entries: &mut VecDeque<Entry>,
    images: &mut chat_images::Cache,
    wanted: &mut Vec<AttachmentId>,
    channel: &ChannelId,
    anchor: Option<Anchor>,
    theme: &Theme,
    graphics: bool,
) {
    let area = paint.history;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (nick, gutter) = history_columns(area.width);
    let compact = gutter == 0;
    let width = area.width - gutter;
    let mut total = 0;
    let mut duplicates = HashMap::<&str, Option<&ParticipantId>>::new();
    let entry_count = entries.len();
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.wrap(width);
        let len = entry.len(compact, graphics) + usize::from(index + 1 < entry_count);
        paint.ranges.push(Range {
            sequence: entry.sequence(),
            start: total,
            len,
        });
        total += len;
    }
    for entry in entries.iter() {
        let author = &entry.message.message.author;
        duplicates
            .entry(&author.display_name)
            .and_modify(|known| {
                if *known != Some(&author.id) {
                    *known = None;
                }
            })
            .or_insert(Some(&author.id));
    }
    paint.tail = total.saturating_sub(area.height as usize);
    let start = anchor
        .and_then(|anchor| {
            paint
                .ranges
                .iter()
                .find(|range| range.sequence == anchor.sequence)
                .map(|range| range.start + anchor.line.min(range.len.saturating_sub(1)))
        })
        .unwrap_or(paint.tail)
        .min(paint.tail);
    paint.start = start;
    let end = start + area.height as usize;
    let first = paint
        .ranges
        .partition_point(|range| range.start + range.len <= start);
    for (index, entry) in entries.iter().enumerate().skip(first) {
        let Range {
            sequence,
            start: at,
            len,
        } = paint.ranges[index];
        if at >= end {
            break;
        }
        let upper = start.max(at);
        let message_end = at + len - usize::from(index + 1 < entry_count);
        let lower = end.min(message_end);
        if upper >= lower {
            continue;
        }
        let meta = &entry.message.message;
        let display_name = safe_text(&meta.author.display_name);
        let duplicate = duplicates.get(meta.author.display_name.as_str()) == Some(&None);
        let name = if meta.author.kind == ParticipantKind::User {
            "You".into()
        } else if duplicate {
            let short: String = meta
                .author
                .id
                .0
                .chars()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let max = if compact {
                area.width.saturating_sub(17)
            } else {
                nick.saturating_sub(7)
            };
            format!("@{}·{}", window::fit(&display_name, max), safe_text(&short))
        } else {
            format!("@{display_name}")
        };
        let minute = (meta.posted_at_ms / 60_000) % 1_440;
        let role = sender_role(&meta.author);
        let x = area.x + gutter;
        if upper == at {
            let y = area.y + (at - start) as u16;
            let time = format!("{:02}:{:02}", minute / 60, minute % 60);
            paint.line(
                theme,
                row(area.x, y, area.width.min(5)),
                &time,
                Role::Muted,
                false,
            );
            let label = window::fit(
                &name,
                if compact {
                    area.width.saturating_sub(6)
                } else {
                    nick
                },
            );
            let aligned = if compact {
                0
            } else {
                nick - text_width(&label) as u16
            };
            draw(
                &mut paint.surface.frame,
                theme,
                area.x + 6 + aligned,
                y,
                &label,
                role,
                false,
                (meta.author.kind == ParticipantKind::User).then_some(markdown::Style {
                    bold: true,
                    ..markdown::Style::default()
                }),
            );
        }
        let relative =
            |line: usize| (line >= upper && line < lower).then(|| area.y + (line - start) as u16);
        let mut line = at + usize::from(compact);
        if meta.intent != PostIntent::Note {
            let (label, role) = if meta.intent == PostIntent::Question {
                ("? Question", Role::Warning)
            } else {
                ("◆ Report", Role::Success)
            };
            if let Some(y) = relative(line) {
                paint.line(theme, row(x, y, width), label, role, false);
            }
            line += 1;
        }
        if let Some(reply) = &meta.reply_to {
            if let Some(y) = relative(line) {
                let author = entries
                    .iter()
                    .find(|source| source.message.message.id == *reply)
                    .map(|source| source.message.message.author.display_name.as_str());
                let label = match author {
                    Some(author) => format!("↪ Reply to {}", safe_text(author)),
                    None => format!("↪ Reply to {}", safe_text(reply.as_ref())),
                };
                paint.line(theme, row(x, y, width), &label, Role::Info, false);
            }
            line += 1;
        }
        if !meta.mentions.is_empty() {
            if let Some(y) = relative(line) {
                let mut label = String::from("@ Addressed: ");
                for (n, id) in meta.mentions.iter().enumerate() {
                    if n != 0 {
                        label.push_str(", ");
                    }
                    let name = entries
                        .iter()
                        .find(|entry| entry.message.message.author.id == *id)
                        .map_or(id.as_ref(), |entry| {
                            entry.message.message.author.display_name.as_str()
                        });
                    label.push_str(&safe_text(name));
                    if text_width(&label) > usize::from(width) {
                        break;
                    }
                }
                paint.line(theme, row(x, y, width), &label, Role::Secondary, false);
            }
            line += 1;
        }
        for (offset, wrapped) in entry
            .rows
            .iter()
            .enumerate()
            .skip(upper.saturating_sub(line))
            .take(lower.saturating_sub(line.max(upper)))
        {
            let Some(y) = relative(line + offset) else {
                continue;
            };
            let mut col = x;
            for run in &wrapped.runs {
                let style = run.style;
                let role = if style.link {
                    Role::Info
                } else if style.inline_code || style.fenced_code {
                    Role::Secondary
                } else if style.quote {
                    Role::Muted
                } else {
                    Role::Base
                };
                col += draw(
                    &mut paint.surface.frame,
                    theme,
                    col,
                    y,
                    &run.text,
                    role,
                    false,
                    Some(style),
                );
            }
            paint.rows.push(HistoryRow {
                sequence,
                offset,
                y,
                x,
                prefix: wrapped.continuation_prefix,
                end: col,
            });
        }
        line += entry.rows.len().max(1);
        for attachment in &entry.message.attachments {
            if let Some(y) = relative(line) {
                let alt = attachment
                    .alt_text
                    .as_deref()
                    .filter(|alt| !alt.trim().is_empty())
                    .unwrap_or("PNG image");
                let prefix = if graphics && images.unavailable(channel, &attachment.id) {
                    "Image unavailable"
                } else {
                    "Image"
                };
                paint.line(
                    theme,
                    row(x, y, width),
                    &format!("[{prefix}: {}]", safe_text(alt)),
                    Role::Info,
                    false,
                );
            }
            line += 1;
            if graphics {
                let preview = usize::from(chat_images::PREVIEW_ROWS);
                if line < end && line + preview > start {
                    wanted.push(attachment.id.clone());
                    images.append(
                        &mut paint.images,
                        channel,
                        &attachment.id,
                        gutter,
                        line as i32 - start as i32,
                        width,
                    );
                }
                line += preview;
            }
        }
        paint.target(
            Rect {
                x: area.x,
                y: area.y + (upper - start) as u16,
                width: area.width,
                height: (lower - upper) as u16,
            },
            Hit::History(Some(index)),
        );
        if lower == message_end {
            paint.rendered.push(sequence);
        }
    }
}

fn history_columns(width: u16) -> (u16, u16) {
    if width < 34 {
        (0, 0)
    } else {
        let nick = MAX_NICK.min(width / 4);
        (nick, nick + 9)
    }
}

fn row(x: u16, y: u16, width: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height: 1,
    }
}

fn narrow_channels_button(width: u16, minimum_remaining: u16) -> (&'static str, u16) {
    if width >= minimum_remaining + 11 {
        ("[Channels]", 11)
    } else if width > 0 {
        ("≡", 1)
    } else {
        ("", 0)
    }
}

fn check_copy_size(bytes: usize, limit: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes <= limit,
        "Selection exceeds clipboard payload limit; nothing sent"
    );
    Ok(())
}

fn channel_name(summary: &ChannelSummary) -> String {
    let name = safe_text(&summary.name);
    if summary.direct || name.starts_with('#') {
        name
    } else {
        format!("#{name}")
    }
}

fn sender_role(participant: &Participant) -> Role {
    if participant.kind == ParticipantKind::User {
        return Role::Accent;
    }
    let mut hash = DefaultHasher::new();
    participant.id.hash(&mut hash);
    [
        Role::Accent,
        Role::Info,
        Role::Secondary,
        Role::Success,
        Role::BrightInfo,
        Role::BrightAccent,
    ][(hash.finish() % 6) as usize]
}

fn role_style(theme: &Theme, role: Role, selected: bool) -> &Style {
    if selected {
        theme.selected_style(role)
    } else {
        theme.style(role)
    }
}

fn card_style(theme: &Theme, role: Role, background: ColorSource) -> Style {
    let mut style = theme.panel_style(role).clone();
    style.background = match background {
        ColorSource::Rgb(color) => color,
        _ => theme.defaults().1,
    };
    style.background_source = background;
    style
}

#[allow(clippy::too_many_arguments)]
fn draw(
    frame: &mut Frame,
    theme: &Theme,
    x: u16,
    y: u16,
    value: &str,
    role: Role,
    selected: bool,
    rich: Option<markdown::Style>,
) -> u16 {
    if x >= frame.size.cols || y >= frame.size.rows {
        return 0;
    }
    text(frame, x, y, value, selected);
    let width = text_width(value).min(usize::from(frame.size.cols - x)) as u16;
    style_cells(frame, x, y, width, role_style(theme, role, selected), rich);
    width
}

fn style_cells(
    frame: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    style: &Style,
    rich: Option<markdown::Style>,
) {
    let Some(row) = frame.rows.get_mut(usize::from(y)) else {
        return;
    };
    let end = x.saturating_add(width).min(frame.size.cols);
    for cell in &mut row.cells[usize::from(x.min(end))..usize::from(end)] {
        cell.style.clone_from(style);
        if let Some(rich) = rich {
            cell.style.bold |= rich.bold;
            cell.style.italic |= rich.italic;
            if rich.link {
                cell.style.underline = 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        App,
        chat::Completion,
        tests::{attach_fixture, fixture, key, size},
    };
    use bkpr_tui_framework::{
        ClientEvent, HitMap, ReplyDemux, Scene, Surface, SurfaceContent, decode_input,
    };
    use termwiz::input::InputParser;

    const BACKGROUNDS: [ColorSource; 3] = [
        ColorSource::Rgb([14, 15, 16]),
        ColorSource::Rgb([15, 22, 24]),
        ColorSource::Rgb([8, 9, 10]),
    ];

    fn open_channel(chat: &mut Chat, last_sequence: u64) {
        let id = ChannelId("fixture-channel".into());
        chat.open("workspace".into());
        chat.replace_catalog(HashMap::from([(
            id.clone(),
            ChannelSummary {
                id,
                workspace_id: "workspace".into(),
                name: "#desk".into(),
                membership: MembershipMode::WorkspaceWide,
                settings: ChannelSettings::default(),
                last_sequence,
                read_through: 0,
                unread_attention: false,
                direct: false,
            },
        )]));
    }

    fn pointer(
        app: &mut App,
        size: Size,
        hits: &HitMap,
        (x, y): (u16, u16),
        code: u8,
        event: char,
    ) {
        let bytes = format!("\x1b[<{code};{};{}{event}", x + 1, y + 1);
        let parts = ReplyDemux::default().feed(bytes.as_bytes());
        for event in decode_input(&mut InputParser::new(), parts, false) {
            let ClientEvent::Mouse {
                event,
                button,
                action,
            } = event
            else {
                panic!("expected SGR mouse event");
            };
            app.mouse(event, Some((button, action)), size, &[], &[], hits)
                .unwrap();
        }
    }

    #[test]
    fn three_aligned_channel_cards_keep_controls_reachable() {
        let mut chat = Chat::default();
        open_channel(&mut chat, 0);
        let theme = Theme::default();
        let wide = size();
        let area = |size: Size| Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        chat.paint(area(wide), wide, &theme, false, false, true, BACKGROUNDS);
        let &[
            (list, list_body, None),
            (thread, thread_body, None),
            (composer, composer_body, Some(6)),
        ] = chat.inlays()
        else {
            panic!("expected neutral list and thread cards plus an accented composer");
        };
        assert_eq!(chat.paint.history, thread_body);
        chat.active.as_mut().unwrap().history.loaded = true;
        chat.redraw();
        chat.paint(area(wide), wide, &theme, false, false, true, BACKGROUNDS);
        let label = "No messages yet";
        let centered_x = thread_body.x + (thread_body.width - text_width(label) as u16) / 2;
        let centered_y = thread_body.y + (thread_body.height - 1) / 2;
        let frame = &chat.surface().frame;
        assert_eq!(
            frame.rows[usize::from(centered_y)].cells
                [usize::from(centered_x)..usize::from(centered_x) + label.len()]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>(),
            label
        );
        assert_eq!(
            frame.rows[usize::from(composer_body.y)].cells[usize::from(composer_body.x + 1)].text,
            "›"
        );
        assert_eq!(frame.cursor.x, composer_body.x + 3);
        assert!(matches!(
            chat.paint.hit(frame.cursor.x, frame.cursor.y),
            Some(Hit::Composer)
        ));
        assert_eq!(list.y, thread.y);
        assert_eq!((composer.x, composer.width), (thread.x, thread.width));
        assert_eq!(composer.y + 1, thread.y + thread.height);
        assert_eq!(thread.x + 1, list.x + list.width);
        assert_eq!(composer.y + composer.height, list.y + list.height);
        chat.set_graphical_inlays(0, &theme);
        assert_eq!(
            chat.surface().frame.rows[usize::from(composer.y)].cells[usize::from(thread.x)].text,
            "├"
        );
        let frame = &chat.surface().frame;
        for (body, background) in [
            (list_body, BACKGROUNDS[0]),
            (thread_body, BACKGROUNDS[0]),
            (composer_body, BACKGROUNDS[1]),
        ] {
            assert_eq!(
                frame.rows[usize::from(body.y)].cells[usize::from(body.x)]
                    .style
                    .background_source,
                background
            );
        }
        let has_action = |chat: &Chat, predicate: fn(Hit) -> bool| {
            chat.paint.hits.iter().any(|(rect, hit)| {
                predicate(*hit) && chat.paint.hit(rect.x, rect.y).is_some_and(predicate)
            })
        };
        assert!(has_action(&chat, |hit| matches!(hit, Hit::Manage)));
        assert!(has_action(&chat, |hit| matches!(hit, Hit::Create)));

        chat.active.as_mut().unwrap().history.anchor = Some(Anchor {
            sequence: 1,
            line: 0,
        });
        chat.redraw();
        chat.paint(area(wide), wide, &theme, false, false, true, BACKGROUNDS);
        assert_eq!(chat.paint.history.height + 1, thread_body.height);
        assert_eq!(chat.inlays()[2].0, composer);
        assert!(matches!(
            chat.paint
                .hit(thread_body.x, thread_body.y + thread_body.height - 1),
            Some(Hit::Latest)
        ));
        chat.live_tail();
        chat.redraw();

        let narrow = Size { cols: 40, ..wide };
        chat.paint(
            area(narrow),
            narrow,
            &theme,
            false,
            false,
            true,
            BACKGROUNDS,
        );
        let &[(thread, thread_body, None), (composer, body, Some(6))] = chat.inlays() else {
            panic!("expected stacked thread and composer on a narrow terminal");
        };
        assert_eq!(chat.paint.history, thread_body);
        assert_eq!(thread.y + thread.height, composer.y + 1);
        assert!(matches!(chat.paint.hit(body.x, body.y), Some(Hit::List)));
        assert_eq!(chat.surface().frame.cursor.y, body.y);

        chat.focus = Some(Focus::Channels);
        chat.redraw();
        chat.paint(
            area(narrow),
            narrow,
            &theme,
            false,
            false,
            true,
            BACKGROUNDS,
        );
        assert!(has_action(&chat, |hit| matches!(hit, Hit::Manage)));
        assert!(has_action(&chat, |hit| matches!(hit, Hit::Create)));
        let [(_, body, None)] = chat.inlays() else {
            panic!("expected only the list card");
        };
        assert_eq!(
            chat.surface().frame.rows[usize::from(body.y + 1)].cells[usize::from(body.x)]
                .style
                .background_source,
            theme.selected_style(Role::Accent).background_source
        );
    }

    #[test]
    fn channel_drag_copies_logical_text_and_retires_at_geometry_change() {
        let (mut app, input) = fixture();
        attach_fixture(&mut app);
        app.pane_padding = crate::appearance::FANCY_PANE_PADDING;
        open_channel(&mut app.chat, 2);
        let job = app.chat.next_job(false).unwrap().0;
        let job = app.chat.track(job);
        let entries = [
            "**界**  one two three  four five six seven eight\nlast",
            "終 tail",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            Entry::new(ChannelMessage {
                message: MessageSummary {
                    id: MessageId(format!("message-{index}")),
                    sequence: index as u64 + 1,
                    author: Participant {
                        id: ParticipantId("workspace-user".into()),
                        kind: ParticipantKind::User,
                        display_name: "User".into(),
                    },
                    posted_at_ms: 0,
                    intent: PostIntent::Note,
                    mentions: Vec::new(),
                    reply_to: None,
                    attachments: Vec::new(),
                },
                text: text.into(),
                attachments: Vec::new(),
            })
        })
        .collect();
        assert!(
            app.chat
                .complete(
                    &job,
                    Ok(Completion::History {
                        entries,
                        older: false
                    }),
                    true,
                )
                .is_none()
        );
        let size = size();
        let theme = Theme::default();
        let outer = app.chat_content(app.screen_geometry(size).1);
        app.chat
            .paint(outer, size, &theme, false, false, true, BACKGROUNDS);
        let frame = &app.chat.surface().frame;
        let hits = Scene {
            size,
            surfaces: vec![Surface {
                palette: None,
                id: "ui:chat",
                content: SurfaceContent::Cells,
                rect: outer,
                frame,
                selection: &[],
                search: &[],
                focused: true,
                faint: false,
            }],
        }
        .hit_map();
        let locate = |glyph: &str| {
            frame
                .rows
                .iter()
                .enumerate()
                .find_map(|(y, row)| {
                    let x = row.cells.iter().position(|cell| cell.text == glyph)?;
                    Some((x, y))
                })
                .unwrap()
        };
        let (first_x, first_y) = locate("界");
        let (last_x, last_y) = locate("終");
        let to = (outer.x + first_x as u16 + 1, outer.y + first_y as u16);
        let from = (outer.x + last_x as u16 + 1, outer.y + last_y as u16);
        let blank = &frame.rows[last_y - 1].cells;
        assert_eq!(blank[first_x - 2].text, " ");
        assert!(
            blank[first_x..first_x + 4]
                .iter()
                .all(|cell| cell.text == " ")
        );
        assert!(
            frame.rows[first_y].cells[..first_x]
                .iter()
                .any(|cell| cell.text == "Y" && cell.style.bold)
        );
        let cursor = (frame.cursor.x, frame.cursor.y);
        assert!(frame.cursor.visible);

        pointer(&mut app, size, &hits, from, 0, 'M');
        pointer(&mut app, size, &hits, to, 32, 'M');
        let spans = app.chat.history_selection_spans();
        assert_eq!(spans[0].start, first_x as u16);
        assert_eq!(spans.last().unwrap().end, last_x as u16 + 2);
        assert!(!spans.iter().any(|span| span.row as usize == last_y - 1));
        pointer(&mut app, size, &hits, to, 0, 'm');
        let mut out = Vec::new();
        app.history_clipboard(&mut out).unwrap();
        let expected = b"\x1b]52;c;55WMICBvbmUgdHdvIHRocmVlICBmb3VyIGZpdmUgc2l4IHNldmVuIGVpZ2h0Cmxhc3QK57WC\x1b\\";
        assert_eq!(out, expected);
        out.clear();
        let mut prefix = key("KeyB");
        prefix.modifiers = 2;
        let mut copy = key("KeyY");
        copy.text = "y".into();
        app.key(prefix, &mut out, size, &[]).unwrap();
        app.key(copy, &mut out, size, &[]).unwrap();
        app.history_clipboard(&mut out).unwrap();
        assert_eq!(out, expected);
        out.clear();

        let mut typed = key("KeyX");
        typed.text = "x".into();
        app.key(typed, &mut out, size, &[]).unwrap();
        app.chat
            .paint(outer, size, &theme, false, false, true, BACKGROUNDS);
        let frame = &app.chat.surface().frame;
        assert_eq!(
            frame.rows[cursor.1 as usize].cells[cursor.0 as usize].text,
            "x"
        );
        assert!(frame.cursor.visible);

        pointer(&mut app, size, &hits, from, 0, 'M');
        pointer(&mut app, size, &hits, to, 32, 'M');
        let resized = Size {
            cols: size.cols - 4,
            ..size
        };
        let outer = app.chat_content(app.screen_geometry(resized).1);
        app.chat
            .paint(outer, resized, &theme, false, false, true, BACKGROUNDS);
        pointer(&mut app, resized, &hits, to, 0, 'm');
        app.history_clipboard(&mut out).unwrap();
        assert!(out.is_empty());
        assert!(app.chat.history_selection_spans().is_empty());
        assert!(app.pointer_capture.is_none());
        assert!(input.try_recv().is_err());
    }
}
