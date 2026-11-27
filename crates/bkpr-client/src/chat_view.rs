use super::super::roles_ui;
use super::{Active, Anchor, Chat, Draft, Send, chat_images};
use bkpr_protocol::{
    ColorSource, Cursor, CursorShape, Frame, SelectionSpan, Size, Style, channels::*,
};
use bkpr_tui_framework::{
    Rect, Role, Theme, UiSurface,
    button::{Button, State as ButtonState},
    clear, graphics_frame, markdown, safe_text, text, text_width, window,
};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

const MAX_NICK: u16 = 24;
const LIST_MIN_WIDTH: u16 = 55;
const MAX_COMPLETIONS: usize = 5;

pub(in crate::app) struct Entry {
    pub(super) message: ChannelMessage,
    document: markdown::Document,
    width: u16,
    layout: markdown::Layout,
}

impl Entry {
    pub(super) fn new(message: ChannelMessage) -> Self {
        let document = markdown::Document::parse(&message.text);
        Self {
            message,
            document,
            width: 0,
            layout: markdown::Layout::default(),
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
        self.document.layout(width.into(), &mut self.layout);
    }

    fn meta_rows(&self) -> usize {
        let meta = &self.message.message;
        usize::from(meta.intent != PostIntent::Note)
            + usize::from(meta.reply_to.is_some())
            + usize::from(!meta.mentions.is_empty())
            + usize::from(meta.role.is_some())
    }

    fn len(&self, compact: bool, graphics: bool) -> usize {
        usize::from(compact)
            + self.meta_rows()
            + self.layout.len().max(1)
            + self.message.attachments.len()
                * (1 + if graphics {
                    chat_images::PREVIEW_ROWS as usize
                } else {
                    0
                })
    }
}

fn mention_runs(
    run: markdown::Run<'_>,
    spans: &[ChannelResolvedMentionSpan],
    mut push: impl FnMut(&str, Option<Role>),
) {
    let Some(source) = run.source.filter(|_| {
        !run.style.inline_code && !run.style.fenced_code && !run.style.quote && !run.style.link
    }) else {
        push(run.text, None);
        return;
    };
    let first = spans.partition_point(|span| span.end as usize <= source.start);
    let spans = &spans[first..];
    if spans
        .first()
        .is_none_or(|span| span.start as usize >= source.end)
    {
        push(run.text, None);
        return;
    }

    let mut range_start = 0;
    let mut active_role = None;
    let span_role = |span: &ChannelResolvedMentionSpan| {
        (span.participant.kind == ParticipantKind::Agent).then(|| sender_role(&span.participant))
    };
    let mut spans = spans.iter();
    let mut span = spans.next();
    let mut ink = span.and_then(span_role);
    for (at, glyph) in run.text.grapheme_indices(true) {
        let start = source.start + at;
        let end = start + glyph.len();
        while span.is_some_and(|span| span.end as usize <= start) {
            span = spans.next();
            ink = span.and_then(span_role);
        }
        let role = ink.filter(|_| {
            span.is_some_and(|span| span.start as usize <= start && end <= span.end as usize)
        });
        if role != active_role {
            if at > range_start {
                push(&run.text[range_start..at], active_role);
            }
            range_start = at;
            active_role = role;
        }
    }
    push(&run.text[range_start..], active_role);
}

fn row_glyphs(row: markdown::RowView<'_>, x: u16) -> impl Iterator<Item = (u16, u16, &str)> {
    row.runs()
        .flat_map(|run| run.text.graphemes(true))
        .scan(x, |col, glyph| {
            let start = *col;
            *col += glyph.width() as u16;
            Some((start, *col, glyph))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hit {
    Channel(usize),
    List,
    OpenChannels,
    History(Option<usize>),
    Composer,
    Mention(usize),
    Latest,
    Create,
    Manage,
    Members,
    MemberBack,
    MemberRail,
    Divider(bkpr_protocol::Axis),
    Invite,
}

impl Paint {
    pub(super) fn has_member_rail(&self) -> bool {
        self.hits.iter().any(|(_, hit)| *hit == Hit::MemberRail)
    }
}

const CHANNEL_BUTTONS: [(Hit, &str); 5] = [
    (Hit::Create, "\u{f0415} New channel"),
    (Hit::Manage, "\u{f0493} Settings"),
    (Hit::Members, "Members"),
    (Hit::MemberBack, MEMBER_BACK),
    (Hit::Invite, "Invite member"),
];

const MEMBER_BACK: &str = "Back";

fn is_channel_button(hit: Hit) -> bool {
    matches!(
        hit,
        Hit::Create | Hit::Manage | Hit::Members | Hit::MemberBack | Hit::Invite
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum MemberGroup {
    Coordinator,
    Agents,
    Users,
}

pub(super) fn member_group(entry: &ChannelRosterEntry) -> MemberGroup {
    if let Some(attribution) = &entry.role {
        return match attribution.role {
            bkpr_protocol::roles::WorkspaceRole::Coordinator => MemberGroup::Coordinator,
        };
    }
    match entry.participant.kind {
        ParticipantKind::Agent => MemberGroup::Agents,
        ParticipantKind::User => MemberGroup::Users,
    }
}

impl MemberGroup {
    const ALL: [Self; 3] = [Self::Coordinator, Self::Agents, Self::Users];

    fn label(self) -> &'static str {
        match self {
            Self::Coordinator => roles_ui::label(bkpr_protocol::roles::WorkspaceRole::Coordinator),
            Self::Agents => "Agents",
            Self::Users => "Users",
        }
    }
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
    fn text(self, entries: &VecDeque<Entry>) -> Option<markdown::RowView<'_>> {
        let index = entries
            .binary_search_by_key(&self.sequence, Entry::sequence)
            .ok()?;
        let entry = &entries[index];
        entry.document.row(&entry.layout, self.offset)
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
    button_pointer: Option<(u16, u16)>,
    button_press: Option<(Rect, Hit)>,
    painted_buttons: [Option<(ButtonState, bool)>; CHANNEL_BUTTONS.len()],
    hovered_channel: Option<ChannelId>,
    row_highlights: [Option<Rect>; 2],
    inlays: Vec<(Rect, Rect, Option<u8>)>,
    inlay_mask: Option<u8>,
    outer: Rect,
    images: Frame,
    history: Rect,
    completion_capacity: usize,
    member_page_rows: usize,
    members_max_scroll: usize,
    composer_width: u16,
    composer: Option<ComposerPaint>,
    composer_dirty: bool,
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

#[derive(Clone, Copy)]
struct ComposerPaint {
    area: Rect,
    rows: u16,
    max_rows: u16,
    focused: bool,
    stable_thread: bool,
}

struct ComposerText {
    lines: Vec<std::ops::Range<usize>>,
    caret: usize,
}

impl ComposerText {
    fn new(draft: Option<&Draft>, columns: u16) -> Self {
        let (lines, caret) = draft.map_or_else(
            || (Vec::new(), 0),
            |draft| draft.lines(usize::from(columns)),
        );
        Self { lines, caret }
    }

    fn rows(&self, max: u16) -> u16 {
        self.lines.len().max(1).min(usize::from(max)) as u16
    }
}

fn draft_affects_thread(draft: Option<&Draft>) -> bool {
    draft.is_some_and(|draft| {
        draft.completion.is_some() || draft.error.is_some() || draft.input.error().is_some()
    })
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
            button_pointer: None,
            button_press: None,
            painted_buttons: [None; CHANNEL_BUTTONS.len()],
            hovered_channel: None,
            row_highlights: [None; 2],
            inlays: Vec::with_capacity(2),
            inlay_mask: None,
            outer: Rect::default(),
            images: graphics_frame(initial),
            history: Rect::default(),
            completion_capacity: 0,
            member_page_rows: 0,
            members_max_scroll: 0,
            composer_width: 1,
            composer: None,
            composer_dirty: false,
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
    fn button_at(&self, (x, y): (u16, u16)) -> Option<(Rect, Hit)> {
        self.hits.iter().rev().find_map(|&(rect, hit)| {
            (is_channel_button(hit) && rect.contains(x, y)).then_some((rect, hit))
        })
    }

    pub(super) fn invalidate_hits(&mut self) {
        self.hits.clear();
        self.button_press = None;
        self.button_pointer = None;
        self.rendered.clear();
        self.ranges.clear();
        self.rows.clear();
        self.completion_capacity = 0;
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
    pub(super) fn member_page_rows(&self) -> usize {
        self.member_page_rows
    }

    pub(super) fn members_max_scroll(&self) -> usize {
        self.members_max_scroll
    }

    pub(super) fn composer_width(&self) -> u16 {
        self.composer_width
    }

    pub(super) fn completion_capacity(&self) -> usize {
        self.completion_capacity
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
        self.completion_capacity = 0;
        self.member_page_rows = 0;
        self.members_max_scroll = 0;
        self.composer_width = 1;
        self.composer = None;
        self.composer_dirty = false;
        self.ranges.clear();
        self.rendered.clear();
        self.hits.clear();
        self.painted_buttons = [None; CHANNEL_BUTTONS.len()];
        self.row_highlights = [None; 2];
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
        self.target(bounds, Hit::OpenChannels);
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
        let (start, end, _) = row_glyphs(row.text(entries)?, row.x).find(|&(_, end, _)| x < end)?;
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
        let mut plain = [Rect::default(); 4];
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

    /// Repaint the draft without rebuilding the thread when its layout is unchanged.
    pub(super) fn redraw_composer(&mut self) {
        self.paint.composer_dirty = true;
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

    pub(in crate::app) fn update_button_pointer(&mut self, point: Option<(u16, u16)>) {
        let outer = self.paint.outer;
        self.paint.button_pointer = point
            .filter(|&(x, y)| outer.contains(x, y))
            .map(|(x, y)| (x - outer.x, y - outer.y));
    }

    pub(in crate::app) fn press_button(&mut self) -> bool {
        self.paint.button_press = self
            .paint
            .button_pointer
            .and_then(|point| self.paint.button_at(point));
        self.paint.button_press.is_some()
    }

    pub(in crate::app) fn release_button(&mut self, read_only: bool) {
        let pressed = self.paint.button_press.take();
        let current = self
            .paint
            .button_pointer
            .and_then(|point| self.paint.button_at(point));
        if let Some((_, hit)) = pressed.filter(|pressed| Some(*pressed) == current) {
            self.activate_button(hit, read_only);
        }
    }

    pub(in crate::app) fn cancel_button_press(&mut self) {
        self.paint.button_press = None;
        self.paint.button_pointer = None;
    }

    pub(in crate::app) fn paint_buttons(
        &mut self,
        effects: &mut Frame,
        theme: &Theme,
        mut place: impl FnMut(&mut Frame, Rect, ButtonState) -> bool,
    ) {
        let paint = &mut self.paint;
        effects.size = paint.surface.frame.size;
        effects.images.clear();
        effects.placements.clear();
        for (index, (hit, label)) in CHANNEL_BUTTONS.into_iter().enumerate() {
            let Some(&(rect, _)) = paint.hits.iter().find(|(_, candidate)| *candidate == hit)
            else {
                continue;
            };
            let state = ButtonState::at(
                rect,
                paint.button_pointer,
                paint.button_press == Some((rect, hit)),
            );
            let graphical = place(effects, rect, state);
            let presentation = Some((state, graphical));
            if paint.painted_buttons[index] != presentation {
                Button::new(label).paint(
                    &mut paint.surface.frame,
                    rect,
                    theme.style(Role::Accent),
                    state,
                    graphical,
                );
                paint.painted_buttons[index] = presentation;
            }
        }
    }

    pub(in crate::app) fn row_highlights(&self) -> [Option<Rect>; 2] {
        self.paint.row_highlights
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
            let join_spaces = text.join_spaces;
            for (left, right, glyph) in row_glyphs(text, row.x) {
                if right > end {
                    break;
                }
                if left >= start && (left < right || left > start) {
                    check_copy_size(result.len() + glyph.len(), limit)?;
                    result.extend_from_slice(glyph.as_bytes());
                }
            }
            previous = Some((row, join_spaces));
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
        let mut composer_text = None;
        if self.paint.key == Some(key) {
            if !self.paint.composer_dirty {
                return;
            }
            let draft = self
                .active
                .as_ref()
                .and_then(|active| self.drafts.get(&active.id));
            if let Some(composer) = self
                .paint
                .composer
                .filter(|composer| composer.stable_thread)
                && !draft_affects_thread(draft)
            {
                let text = ComposerText::new(draft, self.paint.composer_width);
                if text.rows(composer.max_rows) == composer.rows {
                    let paint = &mut self.paint;
                    clear(&mut paint.surface.frame, composer.area);
                    paint.fill_card(theme, composer.area, backgrounds[1]);
                    paint_composer(paint, theme, backgrounds[1], composer, draft, &text);
                    paint.composer_dirty = false;
                    return;
                }
                composer_text = Some(text);
            }
        }
        let geometry_changed = self.paint.outer != outer || self.paint.surface.frame.size != size;
        if geometry_changed {
            self.cancel_button_press();
            self.paint.hovered_channel = None;
        }
        let old_history = self.paint.history;
        self.paint.begin(key, theme);
        self.paint_contents(screen, key, theme, composer_text);
        if geometry_changed
            || self.paint.history != old_history
            || self.paint.rows != self.paint.previous_rows
        {
            self.paint.clear_selection();
        }
    }

    fn paint_contents(
        &mut self,
        screen: Size,
        key: PaintKey,
        theme: &Theme,
        composer_text: Option<ComposerText>,
    ) {
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
        let list_only = self.channel_chooser;
        if let Some(offset) = &mut self.members_scroll
            && let Some(active) = &self.active
            && let Some(summary) = self.catalog.get(&active.id)
        {
            let mut title = format!("Members · {}", channel_name(summary));
            if self.roles_enabled == Some(false) {
                title.push_str(" · Live updates unavailable");
            }
            paint_members(
                paint,
                theme,
                full,
                (
                    list_background,
                    true,
                    !read_only && invite_available(summary),
                ),
                &title,
                active,
                offset,
            );
            return;
        }
        let split = (!list_only && size.cols >= LIST_MIN_WIDTH)
            .then(|| {
                crate::tiling::partition_at(
                    full,
                    bkpr_protocol::Axis::Horizontal,
                    self.rail_width
                        .unwrap_or_else(|| (size.cols / 4).clamp(15, 24) - 1)
                        .clamp(12, size.cols.saturating_sub(24).max(12)),
                )
            })
            .flatten();
        let channels_wide = split.is_some();
        let (list, thread_column) = split
            .map_or((list_only.then_some(full), full), |(list, _, right)| {
                (Some(list), right)
            });
        let summary = self
            .active
            .as_ref()
            .and_then(|active| self.catalog.get(&active.id));
        let (list, member_rail) = match (list, summary) {
            (Some(rail), Some(channel))
                if !list_only
                    && rail.height >= 12
                    && Self::can_show_members(channel, self.roles_enabled) =>
            {
                let first = self
                    .channels_height
                    .unwrap_or(rail.height * 3 / 5)
                    .clamp(6, rail.height - 6);
                crate::tiling::partition_at(rail, bkpr_protocol::Axis::Vertical, first)
                    .map_or((Some(rail), None), |(channels, _, members)| {
                        (Some(channels), Some(members))
                    })
            }
            (rail, _) => (rail, None),
        };
        if let (Some(rail), Some(channel), Some(active)) =
            (member_rail, summary, self.active.as_ref())
        {
            let title = format!("Members · {}", channel_name(channel));
            paint.target(rail, Hit::MemberRail);
            paint_members(
                paint,
                theme,
                rail,
                (
                    list_background,
                    false,
                    !read_only && invite_available(channel),
                ),
                &title,
                active,
                &mut self.rail_members_scroll,
            );
        }
        let writing = !read_only && summary.is_some_and(Self::can_write);
        let can_create = !read_only && self.catalog_ready && self.catalog_error.is_none();
        if let Some(outer) = list {
            let body = paint.inlay(theme, outer, list_background, None);
            paint.target(outer, Hit::List);
            let reserved = 1 + u16::from(!self.ids.is_empty());
            let has_settings =
                !read_only && summary.is_some_and(Self::can_manage) && body.height > reserved;
            let has_members = member_rail.is_none()
                && summary
                    .is_some_and(|channel| Self::can_show_members(channel, self.roles_enabled))
                && body.height > reserved + u16::from(has_settings);
            let has_create = can_create
                && body.height >= reserved + 2 + u16::from(has_settings) + u16::from(has_members);
            let actions =
                u16::from(has_settings) + u16::from(has_members) + 2 * u16::from(has_create);
            let first_channel = 1 + u16::from(
                !list_only && !self.ids.is_empty() && body.height > reserved + actions,
            );
            let list_height = body.height.saturating_sub(first_channel + actions);
            let (title, indent) = if list_only {
                ("Choose a channel", 0)
            } else {
                let title = "Channels";
                (
                    title,
                    body.width.saturating_sub(text_width(title) as u16) / 2,
                )
            };
            paint.card_line(
                theme,
                row(body.x + indent, body.y, body.width - indent),
                title,
                Role::Title,
                false,
                list_background,
            );
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
                let lead = if summary.unread_attention { "! " } else { "  " };
                let name = if summary.role_thread.is_some_and(|thread| thread.archived) {
                    let short: String = id.as_ref().chars().take(6).collect();
                    format!("Archive {}", safe_text(&short))
                } else {
                    channel_name(summary)
                };
                let full = format!("{lead}{name}");
                let bounds = row(body.x, body.y + first_channel + line as u16, body.width);
                let chosen = list_only && self.list_cursor == index;
                let role = if summary.unread_attention {
                    Role::Warning
                } else if selected == Some(id) {
                    Role::Accent
                } else {
                    Role::Base
                };
                if paint.hovered_channel.as_ref() == Some(id) {
                    paint.row_highlights[0] = Some(bounds);
                    let hover = crate::appearance::hover_background(theme, list_background);
                    paint.fill_card(theme, bounds, hover);
                    paint.card_line(theme, bounds, &full, role, false, hover);
                } else {
                    if chosen {
                        paint.row_highlights[1] = Some(bounds);
                    }
                    paint.card_line(theme, bounds, &full, role, chosen, list_background);
                }
                paint.target(bounds, Hit::Channel(index));
            }
            if has_create {
                let visible = (self.ids.len().saturating_sub(offset)).min(usize::from(list_height));
                paint.target(
                    row(
                        body.x,
                        body.y + first_channel + visible as u16 + 1,
                        body.width,
                    )
                    .inset_horizontal(1),
                    Hit::Create,
                );
            }
            if has_settings {
                paint.target(
                    row(body.x, body.y + body.height - 1, body.width).inset_horizontal(1),
                    Hit::Manage,
                );
            }
            if has_members {
                paint.target(
                    row(
                        body.x,
                        body.y + body.height - 1 - u16::from(has_settings),
                        body.width,
                    )
                    .inset_horizontal(1),
                    Hit::Members,
                );
            }
        }
        if let Some((_, divider, _)) = split {
            paint.target(divider, Hit::Divider(bkpr_protocol::Axis::Horizontal));
        }
        if let Some(members) = member_rail {
            paint.target(
                row(members.x, members.y, members.width.saturating_sub(1)),
                Hit::Divider(bkpr_protocol::Axis::Vertical),
            );
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
                Send::Pending { .. } => (Role::Info, "Sending…"),
                Send::SettingRole => (Role::Info, "Updating Coordinator…"),
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
        let catalog_rows = u16::from(self.catalog_error.is_some());
        let detail_rows = u16::from(status.is_some() && size.rows > 1 + catalog_rows);
        let status_rows = catalog_rows + detail_rows;
        let has_composer = writing && size.rows > 1 + status_rows;
        let inset = u16::from(has_composer && columns >= 5 && size.rows >= 6 + status_rows);
        let history_inset = u16::from(
            columns >= 3 && size.rows >= 3.max(2 + status_rows) && (!has_composer || inset != 0),
        );
        let composer_columns = columns.saturating_sub(2 * inset);
        let composer_padding = u16::from(composer_columns >= 6);
        let (button, button_width) = if channels_wide {
            ("", 0)
        } else if has_composer {
            narrow_channels_button(composer_columns - composer_padding, 8)
        } else {
            let thread_columns = Rect {
                width: columns.saturating_sub(2 * history_inset),
                ..thread_column
            }
            .inset_horizontal(crate::appearance::FANCY_PANE_PADDING * history_inset)
            .width;
            narrow_channels_button(thread_columns, 34)
        };
        let history_indent = if has_composer { 0 } else { button_width };
        let max_composer_rows = if has_composer {
            paint.composer_width = composer_columns
                .saturating_sub(button_width + composer_padding + 3)
                .max(1);
            4.min(size.rows - 1 - status_rows - 2 * inset - history_inset)
        } else {
            0
        };
        let composer_text = composer_text.unwrap_or_else(|| {
            ComposerText::new(draft.filter(|_| has_composer), paint.composer_width)
        });
        let composer_rows = composer_text.rows(max_composer_rows);
        let composer_at = size.rows - composer_rows - 2 * inset;
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
            paint.inlay(theme, thread_outer, list_background, None);
            crate::controller::padded_content(thread_outer, crate::appearance::FANCY_PANE_PADDING)
        } else {
            paint.fill_card(theme, thread_outer, list_background);
            thread_outer
        };
        let completion_capacity = if has_composer
            && thread.width != 0
            && draft.is_some_and(|draft| draft.completion.is_some())
        {
            usize::from(thread.height.saturating_sub(1 + status_rows)).min(MAX_COMPLETIONS)
        } else {
            0
        };
        let mut choices = [None; MAX_COMPLETIONS];
        if completion_capacity != 0
            && active.roster_ready
            && let Some(draft) = draft
        {
            for (slot, choice) in choices
                .iter_mut()
                .zip(draft.choices(&active.roster, completion_capacity))
            {
                *slot = Some(choice);
            }
        }
        let choice_count = choices.iter().flatten().count();
        paint.completion_capacity = choice_count;
        let completion_rows = choice_count.max(1).min(completion_capacity) as u16;
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
        if let Some(error) = &self.catalog_error {
            paint.card_line(
                theme,
                row(history_rect.x, status_at, history_rect.width),
                &format!("Channel catalog stopped · {error}"),
                Role::Error,
                false,
                list_background,
            );
        }
        if detail_rows != 0
            && let Some((role, label, hit)) = status
        {
            let bounds = row(history_rect.x, status_at + catalog_rows, history_rect.width);
            paint.card_line(theme, bounds, &label, role, false, list_background);
            if let Some(hit) = hit {
                paint.target(bounds, hit);
            }
        }
        if completion_rows > 0
            && let Some(draft) = draft
        {
            if choice_count == 0 {
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
                for (i, (index, entry, chosen)) in choices.into_iter().flatten().enumerate() {
                    let bounds = row(thread.x, completion_at + i as u16, thread.width);
                    let (label, mut hint) = Draft::selection_label(entry, index);
                    let label_width = text_width(&label).min(usize::from(bounds.width)) as u16;
                    if chosen
                        && let Some(scope) =
                            draft.completion_wake_preview(summary.settings.user_wake)
                        && usize::from(label_width) + text_width(&hint) + text_width(scope) + 3
                            <= usize::from(bounds.width)
                    {
                        hint.push_str(" · ");
                        hint.push_str(scope);
                    }
                    let background = if chosen {
                        paint.row_highlights[1] = Some(bounds);
                        let background = theme.background(Role::Selected, false);
                        paint.fill_card(theme, bounds, background);
                        background
                    } else {
                        list_background
                    };
                    let mut style = card_style(theme, sender_role(&entry.participant), background);
                    style.bold |= chosen;
                    paint.styled_line(row(bounds.x, bounds.y, label_width), &label, &style, false);
                    paint.card_line(
                        theme,
                        row(bounds.x + label_width, bounds.y, bounds.width - label_width),
                        &hint,
                        Role::Muted,
                        false,
                        background,
                    );
                    paint.target(bounds, Hit::Mention(index));
                }
            }
        }
        let background = card_style(theme, Role::Base, list_background);
        let (_, gutter) = history_columns(history_rect.width);
        let divider = (gutter != 0 && !history.entries.is_empty()).then(|| {
            let mut style = card_style(theme, Role::Border, list_background);
            if let ColorSource::Rgb(fill) = list_background {
                style.foreground = crate::appearance::blend([0; 3], fill, 14);
                style.foreground_source = ColorSource::Rgb(style.foreground);
            }
            (history_rect.x + gutter - 2, style)
        });
        for y in history_rect.y..status_at {
            let cells = &mut paint.surface.frame.rows[usize::from(y)].cells;
            let x = usize::from(history_rect.x);
            for cell in &mut cells[x..x + usize::from(history_rect.width)] {
                cell.style.background_source = background.background_source;
                cell.style.background = background.background;
            }
            if let Some((column, style)) = &divider {
                text(&mut paint.surface.frame, *column, y, "│", false);
                style_cells(&mut paint.surface.frame, *column, y, 1, style, None);
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
            let composer = ComposerPaint {
                area: Rect {
                    x: body.x + button_width + composer_padding,
                    width: body.width - button_width - composer_padding,
                    ..body
                },
                rows: composer_rows,
                max_rows: max_composer_rows,
                focused: focused && !self.sends.contains_key(&active.id),
                stable_thread: !draft_affects_thread(draft),
            };
            paint.composer = Some(composer);
            paint.target(composer.area, Hit::Composer);
            if button_width > 0 {
                paint.channels_button(
                    theme,
                    row(body.x, body.y, text_width(button) as u16),
                    button,
                    Some(composer_background),
                );
            }
            paint_composer(
                paint,
                theme,
                composer_background,
                composer,
                draft,
                &composer_text,
            );
        }
    }
}

fn invite_available(channel: &ChannelSummary) -> bool {
    super::chat_controls::Editor::can_invite(channel)
}

fn paint_members(
    paint: &mut Paint,
    theme: &Theme,
    outer: Rect,
    (background, show_back, invite): (ColorSource, bool, bool),
    title: &str,
    active: &Active,
    offset: &mut usize,
) {
    let body = paint.inlay(theme, outer, background, None);
    if body.width == 0 || body.height == 0 {
        return;
    }
    let back_width = Button::new(MEMBER_BACK).width();
    let back = if show_back && body.width >= back_width + 8 {
        paint.target(
            row(body.x + body.width - back_width, body.y, back_width),
            Hit::MemberBack,
        );
        back_width + 1
    } else {
        0
    };
    paint.card_line(
        theme,
        row(body.x, body.y, body.width - back),
        title,
        Role::Title,
        false,
        background,
    );
    let invite = invite && body.height >= 3;
    if invite {
        paint.target(
            row(body.x, body.y + body.height - 1, body.width),
            Hit::Invite,
        );
    }
    let rows = usize::from(body.height - 1 - u16::from(invite));
    if rows == 0 {
        return;
    }
    let status = if active.workspace_roster {
        Some(("Loading members…", Role::Muted))
    } else if let Some(error) = &active.roster_error {
        Some((error.as_str(), Role::Error))
    } else if !active.roster_ready {
        Some(("Loading members…", Role::Muted))
    } else if active.roster.is_empty() {
        Some(("No members", Role::Muted))
    } else {
        None
    };
    if let Some((status, role)) = status {
        paint.card_line(
            theme,
            row(body.x, body.y + 1, body.width),
            status,
            role,
            false,
            background,
        );
        return;
    }
    let mut remaining = active.roster.as_slice();
    let groups = MemberGroup::ALL.map(|group| {
        let end = remaining.partition_point(|entry| member_group(entry) == group);
        let (entries, rest) = remaining.split_at(end);
        remaining = rest;
        (group, entries)
    });
    let headings = groups
        .iter()
        .filter(|(_, entries)| !entries.is_empty())
        .count();
    paint.member_page_rows = rows;
    paint.members_max_scroll = (active.roster.len() + headings).saturating_sub(rows);
    *offset = (*offset).min(paint.members_max_scroll);
    let mut y = body.y + 1;
    let mut skip = *offset;
    let end = body.y + body.height - u16::from(invite);
    for (group, entries) in groups
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
    {
        if skip == 0 {
            paint.card_line(
                theme,
                row(body.x, y, body.width),
                group.label(),
                Role::Title,
                false,
                background,
            );
            y += 1;
        } else {
            skip -= 1;
        }
        for entry in entries.iter().skip(skip).take(usize::from(end - y)) {
            let indent = u16::from(body.width > 1);
            paint.card_line(
                theme,
                row(body.x + indent, y, body.width - indent),
                &safe_text(&entry.participant.display_name),
                sender_role(&entry.participant),
                false,
                background,
            );
            y += 1;
        }
        if y == end {
            break;
        }
        skip = skip.saturating_sub(entries.len());
    }
}

fn paint_composer(
    paint: &mut Paint,
    theme: &Theme,
    background: ColorSource,
    composer: ComposerPaint,
    draft: Option<&Draft>,
    draft_text: &ComposerText,
) {
    let Rect {
        x: left,
        y: top,
        width: columns,
        ..
    } = composer.area;
    let caret = draft_text.caret;
    let visible_from = caret.saturating_sub(usize::from(composer.rows - 1));
    let prompt = if draft.is_some_and(|draft| draft.reply_to.is_some()) {
        "↪"
    } else {
        "›"
    };
    let lead = if columns > 1 && text_width(prompt) == 1 {
        prompt
    } else {
        ">"
    };
    paint.card_line(
        theme,
        row(left, top, 1.min(columns)),
        lead,
        Role::Accent,
        false,
        background,
    );
    if let Some(draft) = draft {
        let mention_style = card_style(theme, Role::Secondary, background);
        for (i, line) in draft_text
            .lines
            .iter()
            .enumerate()
            .skip(visible_from)
            .take(composer.rows.into())
        {
            let y = top + (i - visible_from) as u16;
            let bounds = row(left.saturating_add(2), y, columns.saturating_sub(2));
            paint.card_line(
                theme,
                bounds,
                &draft.input.value[line.clone()],
                Role::Base,
                false,
                background,
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
    if composer.focused && columns > 2 {
        let col = draft
            .and_then(|draft| {
                draft_text
                    .lines
                    .get(caret)
                    .map(|line| text_width(&draft.input.value[line.start..draft.input.cursor]))
            })
            .unwrap_or(0);
        paint.surface.frame.cursor = Cursor {
            x: left + 2 + col.min(usize::from(columns - 3)) as u16,
            y: top + (caret - visible_from) as u16,
            visible: true,
            blinking: false,
            shape: CursorShape::Bar,
            color: None,
        };
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
        if let Some(attribution) = &meta.role {
            if let Some(y) = relative(line) {
                let label = format!("Posted as {}", roles_ui::label(attribution.role));
                paint.line(theme, row(x, y, width), &label, Role::Muted, false);
            }
            line += 1;
        }
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
        let first = upper.saturating_sub(line);
        let last = lower.saturating_sub(line).min(entry.layout.len());
        for offset in first..last {
            let Some(y) = relative(line + offset) else {
                continue;
            };
            let mut col = x;
            let wrapped = entry
                .document
                .row(&entry.layout, offset)
                .expect("visible row");
            let prefix = wrapped.continuation_prefix as u16;
            for run in wrapped.runs() {
                let style = run.style;
                mention_runs(run, &meta.mention_spans, |value, mention_role| {
                    let role = if let Some(mention) = mention_role {
                        mention
                    } else if style.link {
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
                        value,
                        role,
                        false,
                        Some(style),
                    );
                });
            }
            paint.rows.push(HistoryRow {
                sequence,
                offset,
                y,
                x,
                prefix,
                end: col,
            });
        }
        line += entry.layout.len().max(1);
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
    let mut name = safe_text(&summary.name);
    if !summary.direct && !name.starts_with('#') {
        name.insert(0, '#');
    }
    if summary.role_thread.is_some_and(|thread| thread.archived) {
        name.push_str(" (archived)");
    }
    name
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
        chat::{Completion, Effect},
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
                role_thread: None,
            },
        )]));
    }

    fn paint_thread(chat: &mut Chat, size: Size, theme: &Theme) {
        chat.paint(
            Rect {
                width: size.cols,
                height: size.rows,
                ..Rect::default()
            },
            size,
            theme,
            false,
            false,
            true,
            BACKGROUNDS,
        );
    }

    #[test]
    fn only_painted_mention_rows_offer_keyboard_acceptance() {
        let mut chat = Chat::default();
        open_channel(&mut chat, 0);
        let active = chat.active.as_mut().unwrap();
        let channel = active.id.clone();
        active.roster = ["First", "Second"]
            .into_iter()
            .map(|name| ChannelRosterEntry {
                participant: Participant {
                    id: ParticipantId(name.into()),
                    kind: ParticipantKind::Agent,
                    display_name: name.into(),
                },
                binding: Some(ChannelMentionBinding {
                    participant_id: ParticipantId(name.into()),
                    launch_id: name.into(),
                    binding_generation: 1,
                }),
                role: None,
            })
            .collect();
        active.roster_ready = true;
        assert!(
            chat.drafts
                .entry(channel.clone())
                .or_default()
                .insert("@", true)
        );
        let theme = Theme::default();
        let tiny = Size {
            cols: 40,
            rows: 2,
            ..size()
        };
        paint_thread(&mut chat, tiny, &theme);
        assert!(matches!(chat.key(&key("Enter"), false), Effect::None));
        assert_eq!(chat.drafts[&channel].input.value, "@");
        assert!(chat.drafts[&channel].completion.is_some());

        let short = Size { rows: 3, ..tiny };
        paint_thread(&mut chat, short, &theme);
        chat.key(&key("ArrowDown"), false);
        paint_thread(&mut chat, short, &theme);
        let chosen = chat.row_highlights()[1].unwrap();
        assert_eq!(chat.paint.hit(chosen.x, chosen.y), Some(Hit::Mention(1)));
        chat.key(&key("Enter"), false);
        assert_eq!(chat.drafts[&channel].input.value, "@Second ");
    }

    #[test]
    fn an_open_thread_shows_a_stopped_catalog_alongside_its_history_error() {
        let mut chat = Chat::default();
        open_channel(&mut chat, 0);
        chat.active.as_mut().unwrap().history.error = Some("History unavailable".into());
        chat.catalog_error(&anyhow::anyhow!("stream closed"));
        paint_thread(&mut chat, size(), &Theme::default());
        let frame = &chat.surface().frame;
        for label in [
            "Channel catalog stopped · stream closed",
            "History unavailable",
        ] {
            assert!(frame.rows.iter().any(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains(label)
            }));
        }
        assert!(frame.cursor.visible);
    }

    #[test]
    fn same_height_typing_retains_thread_paint_but_a_new_line_reflows_it() {
        let mut chat = Chat::default();
        open_channel(&mut chat, 0);
        let theme = Theme::default();
        paint_thread(&mut chat, size(), &theme);
        let area = chat.paint.history;
        let old_cursor = &chat.surface().frame.cursor;
        let (x, y) = (old_cursor.x, old_cursor.y);
        chat.set_graphical_inlays(0, &theme);

        let mut event = key("KeyE");
        event.text = "é".into();
        chat.key(&event, false);
        paint_thread(&mut chat, size(), &theme);
        let frame = &chat.surface().frame;
        assert_eq!(frame.rows[usize::from(y)].cells[usize::from(x)].text, "é");
        assert_eq!(frame.cursor.x, x + 1);
        assert_eq!(chat.paint.inlay_mask, Some(0));
        assert_eq!(chat.paint.history, area);

        chat.draft().unwrap().insert("\n", false);
        chat.redraw_composer();
        paint_thread(&mut chat, size(), &theme);
        assert_eq!(chat.paint.history.height + 1, area.height);
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
    fn exact_mentions_keep_identity_across_markdown_reflow_without_changing_text() {
        let first = Participant {
            id: ParticipantId("agent-one".into()),
            kind: ParticipantKind::Agent,
            display_name: "Équipe 界".into(),
        };
        let second = (0..32)
            .map(|index| Participant {
                id: ParticipantId(format!("other-{index}")),
                ..first.clone()
            })
            .find(|participant| sender_role(participant) != sender_role(&first))
            .unwrap();
        let text = "例 @Équipe 界 **@Équipe 界** `@Équipe 界` @Équipe 界";
        let marked = text
            .match_indices("@Équipe 界")
            .zip([&first, &second, &first])
            .map(|((start, label), participant)| ChannelResolvedMentionSpan {
                start: start as u32,
                end: (start + label.len()) as u32,
                participant: participant.clone(),
            })
            .collect::<Vec<_>>();
        let document = markdown::Document::parse(text);
        for width in [8, 80] {
            let mut colored = [String::new(), String::new()];
            let mut layout = markdown::Layout::default();
            document.layout(width, &mut layout);
            for index in 0..layout.len() {
                let row = document.row(&layout, index).unwrap();
                let original: String = row.runs().map(|run| run.text).collect();
                let mut painted = String::new();
                let row = document.row(&layout, index).unwrap();
                for run in row.runs() {
                    let style = run.style;
                    mention_runs(run, &marked, |text, role| {
                        painted.push_str(text);
                        if let Some(role) = role {
                            assert!(!style.inline_code);
                            assert_eq!(style.bold, role == sender_role(&second));
                            colored[usize::from(role == sender_role(&second))].push_str(text);
                        }
                    });
                }
                assert_eq!(painted, original);
            }
            let token = if width == 8 {
                "@Équipe界"
            } else {
                "@Équipe 界"
            };
            assert_eq!(colored, [token, token]);
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
            (members, _, None),
            (list, list_body, None),
            (thread, thread_body, None),
            (composer, composer_body, Some(6)),
        ] = chat.inlays()
        else {
            panic!("expected neutral list and thread cards plus an accented composer");
        };
        assert_eq!(members.x, list.x);
        assert!(members.y >= list.y + list.height - 1);
        assert_eq!(
            chat.paint.history,
            crate::controller::padded_content(thread, crate::appearance::FANCY_PANE_PADDING)
        );
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
        assert_eq!(composer.y + composer.height, members.y + members.height);
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
        for (hit, y) in [
            (Hit::Create, list_body.y + 4),
            (Hit::Manage, list_body.y + list_body.height - 1),
        ] {
            let button = row(list_body.x, y, list_body.width).inset_horizontal(1);
            assert_eq!(chat.paint.button_at((button.x, y)), Some((button, hit)));
            assert_eq!(
                chat.paint.button_at((button.x + button.width - 1, y)),
                Some((button, hit))
            );
            assert_eq!(chat.paint.button_at((list_body.x, y)), None);
            assert_eq!(
                chat.paint.button_at((list_body.x + list_body.width - 1, y)),
                None
            );
        }
        assert!(matches!(
            chat.paint.hit(list_body.x, list_body.y + 1),
            Some(Hit::List)
        ));
        assert!(matches!(
            chat.paint.hit(list_body.x, list_body.y + 2),
            Some(Hit::Channel(0))
        ));
        assert!(matches!(
            chat.paint.hit(list_body.x, list_body.y + 3),
            Some(Hit::List)
        ));

        chat.active.as_mut().unwrap().history.anchor = Some(Anchor {
            sequence: 1,
            line: 0,
        });
        chat.redraw();
        chat.paint(area(wide), wide, &theme, false, false, true, BACKGROUNDS);
        assert_eq!(chat.paint.history.height + 1, thread_body.height);
        assert_eq!(chat.inlays().last().map(|card| card.0), Some(composer));
        assert!(matches!(
            chat.paint
                .hit(chat.paint.history.x, thread_body.y + thread_body.height - 1),
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
        let &[(thread, _, None), (composer, body, Some(6))] = chat.inlays() else {
            panic!("expected stacked thread and composer on a narrow terminal");
        };
        assert_eq!(
            chat.paint.history,
            crate::controller::padded_content(thread, crate::appearance::FANCY_PANE_PADDING)
        );
        assert_eq!(thread.y + thread.height, composer.y + 1);
        assert!(matches!(
            chat.paint.hit(body.x, body.y),
            Some(Hit::OpenChannels)
        ));
        assert_eq!(chat.surface().frame.cursor.y, body.y);

        chat.set_channel_chooser(true);
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

        chat.set_channel_chooser(false);
        chat.catalog.values_mut().next().unwrap().direct = true;
        chat.paint(area(wide), wide, &theme, false, false, true, BACKGROUNDS);
        assert!(!chat.surface().frame.cursor.visible);
        assert!(!has_action(&chat, |hit| matches!(hit, Hit::Composer)));
    }

    #[test]
    fn alt_click_picks_the_full_chat_image_and_does_not_send_a_pane_click() {
        use bkpr_protocol::{Image, ImageLayer, PixelFormat, Placement};
        use std::sync::Arc;

        let (mut app, input) = fixture();
        attach_fixture(&mut app);
        let query = app.colors.graphics.begin();
        let query = std::str::from_utf8(&query).unwrap();
        let id = query.split("i=").nth(1).unwrap().split(',').next().unwrap();
        app.colors.feed(format!("graphics;Gi={id};OK").as_bytes());
        open_channel(&mut app.chat, 0);
        let size = size();
        let theme = Theme::default();
        let outer = app.chat_content(app.screen_geometry(size).1);
        app.chat
            .paint(outer, size, &theme, false, true, true, BACKGROUNDS);
        let image = Arc::new(Image {
            id: 7,
            generation: 19,
            width: 6,
            height: 5,
            format: PixelFormat::Rgba,
            pixels: [80, 90, 100, 255].repeat(30),
            source: None,
        });
        let graphics = &mut app.chat.paint.images;
        graphics.images.push(Arc::clone(&image));
        graphics.placements.push(Placement {
            image_id: image.id,
            generation: image.generation,
            placement_id: 1,
            col: 2,
            row: 1,
            offset_x: 0,
            offset_y: 0,
            width: size.cell_width,
            height: size.cell_height,
            source_x: 1,
            source_y: 2,
            source_width: 3,
            source_height: 2,
            layer: ImageLayer::AboveText,
            z: 0,
        });
        let (area, _) = app.chat.image_surface().unwrap();
        let hits = Scene {
            size,
            surfaces: vec![Surface {
                id: "ui:chat",
                rect: outer,
                frame: &app.chat.surface().frame,
                palette: None,
                selection: &[],
                search: &[],
                focused: true,
                faint: false,
                content: SurfaceContent::Cells,
            }],
        }
        .hit_map();
        let preview = (area.x + 2, area.y + 1);
        pointer(&mut app, size, &hits, (area.x + 3, preview.1), 8, 'M');
        pointer(&mut app, size, &hits, (area.x + 3, preview.1), 8, 'm');
        assert!(app.image_viewer.is_none());
        pointer(&mut app, size, &hits, preview, 0, 'M');
        pointer(&mut app, size, &hits, preview, 0, 'm');
        assert!(app.image_viewer.is_none());
        pointer(&mut app, size, &hits, preview, 8, 'M');
        pointer(&mut app, size, &hits, preview, 8, 'm');
        let mut viewer = UiSurface::new(size);
        app.image_viewer
            .as_ref()
            .unwrap()
            .paint(&mut viewer, &theme, |_, _| false);
        assert!(Arc::ptr_eq(&viewer.frame.images[0], &image));
        let placement = &viewer.frame.placements[0];
        assert_eq!((placement.source_width, placement.source_height), (6, 5));
        while let Ok(operation) = input.try_recv() {
            assert!(!matches!(
                operation,
                bkpr_protocol::Operation::Pane {
                    request: bkpr_protocol::PaneRequest::Mouse { .. },
                    ..
                }
            ));
        }
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
                    mention_spans: Vec::new(),
                    reply_to: None,
                    attachments: Vec::new(),
                    source: None,
                    role: None,
                },
                text: text.into(),
                attachments: Vec::new(),
                current_delegation: None,
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
        let divider = first_x - 2;
        assert!(
            frame.rows[app.chat.paint.history.y as usize
                ..usize::from(app.chat.paint.history.y + app.chat.paint.history.height)]
                .iter()
                .all(|row| row.cells[divider].text == "│")
        );
        let blank = &frame.rows[last_y - 1].cells;
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
