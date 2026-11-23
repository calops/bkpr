use super::{Anchor, Chat, Draft, Focus, Send, chat_images};
use bkpr_protocol::{Cursor, CursorShape, Frame, Size, channels::*};
use bkpr_tui_framework::{
    Rect, Role, Theme, UiSurface, clear, graphics_frame, markdown, safe_text, text, text_width,
    window,
};
use std::{
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
};

const MAX_NICK: u16 = 16;
const LIST_MIN_WIDTH: u16 = 55;

pub(in crate::app) struct Entry {
    pub(super) message: ChannelMessage,
    document: markdown::Document,
    width: u16,
    rows: Vec<Vec<Run>>,
}

struct Run {
    text: String,
    style: markdown::Style,
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
            .map(|row| {
                row.runs
                    .into_iter()
                    .map(|run| Run {
                        text: run.text.to_owned(),
                        style: run.style,
                    })
                    .collect()
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

pub(super) struct Paint {
    pub(super) surface: UiSurface,
    pub(super) rendered: Vec<u64>,
    hits: Vec<(Rect, Hit)>,
    outer: Rect,
    images: Frame,
    history: Rect,
    ranges: Vec<Range>,
    start: usize,
    tail: usize,
    key: Option<PaintKey>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PaintKey {
    outer: Rect,
    size: Size,
    theme: u64,
    read_only: bool,
    graphics: bool,
    focused: bool,
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
            outer: Rect::default(),
            images: graphics_frame(initial),
            history: Rect::default(),
            ranges: Vec::new(),
            start: 0,
            tail: 0,
            key: None,
        }
    }
}

impl Paint {
    pub(super) fn invalidate_hits(&mut self) {
        self.hits.clear();
        self.rendered.clear();
        self.ranges.clear();
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
        self.history.width.saturating_sub(3).max(1)
    }

    pub(super) fn scroll(&mut self, delta: isize) -> Option<(Anchor, bool)> {
        let start = self.start.saturating_add_signed(delta).min(self.tail);
        let index = self
            .ranges
            .partition_point(|range| range.start <= start)
            .checked_sub(1)?;
        let range = &self.ranges[index];
        let boundary = if delta < 0 {
            start == 0
        } else {
            start == self.tail
        };
        self.start = start;
        Some((
            Anchor {
                sequence: range.sequence,
                line: start - range.start,
            },
            boundary,
        ))
    }

    fn begin(&mut self, key: PaintKey, theme: &Theme) {
        self.key = Some(key);
        self.outer = key.outer;
        self.history = Rect::default();
        self.ranges.clear();
        self.rendered.clear();
        self.hits.clear();
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
        let style = theme.style(Role::Base);
        for cell in frame.rows.iter_mut().flat_map(|row| &mut row.cells) {
            cell.style.clone_from(style);
        }
    }

    fn line(&mut self, theme: &Theme, rect: Rect, value: &str, role: Role, selected: bool) {
        if rect.height > 0 && rect.width > 0 {
            draw(
                &mut self.surface.frame,
                theme,
                rect.x,
                rect.y,
                &window::fit(value, rect.width),
                role,
                selected,
                None,
            );
        }
    }

    fn target(&mut self, rect: Rect, hit: Hit) {
        if rect.height > 0 && rect.width > 0 {
            self.hits.push((rect, hit));
        }
    }
}

impl Chat {
    pub(in crate::app) fn redraw(&mut self) {
        self.paint.key = None;
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

    pub(in crate::app) fn paint(
        &mut self,
        outer: Rect,
        screen: Size,
        theme: &Theme,
        read_only: bool,
        graphics: bool,
        focused: bool,
    ) {
        let size = outer.size(screen);
        let key = PaintKey {
            outer,
            size,
            theme: theme.revision(),
            read_only,
            graphics,
            focused,
        };
        if self.paint.key == Some(key) {
            return;
        }
        self.paint.begin(key, theme);
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
        let channels_wide = size.cols >= LIST_MIN_WIDTH;
        let list_only = !channels_wide && self.focus == Some(Focus::Channels);
        let list_width = if channels_wide {
            (size.cols / 4).clamp(15, 24)
        } else if list_only {
            size.cols
        } else {
            0
        };
        let list_height = size.rows.saturating_sub(1);
        if list_width > 0 {
            paint.line(
                theme,
                row(0, 0, list_width),
                " Channels",
                Role::Title,
                false,
            );
            paint.target(
                Rect {
                    x: 0,
                    y: 0,
                    width: list_width,
                    height: size.rows,
                },
                Hit::List,
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
                let lead = if summary.unread_attention {
                    "!"
                } else if selected == Some(id) {
                    ">"
                } else {
                    " "
                };
                let name = channel_name(summary);
                let full = format!("{lead} {name}");
                let bounds = row(0, line as u16 + 1, list_width);
                let chosen = self.focus == Some(Focus::Channels) && self.list_cursor == index;
                let role = if summary.unread_attention {
                    Role::Warning
                } else if selected == Some(id) {
                    Role::Accent
                } else {
                    Role::Base
                };
                paint.line(theme, bounds, &full, role, chosen);
                paint.target(bounds, Hit::Channel(index));
            }
        }
        if list_only {
            return;
        }
        let left = list_width + u16::from(channels_wide);
        let columns = size.cols.saturating_sub(left);
        if channels_wide {
            for y in 0..size.rows {
                paint.line(theme, row(list_width, y, 1), "│", Role::Border, false);
            }
        }
        let summary = self
            .active
            .as_ref()
            .and_then(|active| self.catalog.get(&active.id));
        let header = if let Some(summary) = summary {
            channel_name(summary)
        } else if self.catalog_error.is_some() {
            " Channels unavailable".into()
        } else if self.catalog_ready {
            " No channels".into()
        } else {
            " Loading channels…".into()
        };
        let button = if channels_wide { "" } else { "[Channels] " };
        let button_width = (text_width(button) as u16).min(columns);
        if button_width > 0 {
            let bounds = row(left, 0, button_width);
            paint.line(theme, bounds, button, Role::Info, false);
            paint.target(bounds, Hit::List);
        }
        let mut right = size.cols;
        if !read_only && columns >= 34 {
            right -= 8;
            let target = row(right, 0, 8);
            paint.line(theme, target, "[+ New]", Role::Info, false);
            paint.target(target, Hit::Create);
            if summary.is_some_and(|summary| !summary.direct) {
                right -= 11;
                let target = row(right, 0, 11);
                paint.line(theme, target, "[Settings]", Role::Info, false);
                paint.target(target, Hit::Manage);
            }
        }
        paint.line(
            theme,
            row(
                left + button_width,
                0,
                right.saturating_sub(left + button_width),
            ),
            &header,
            Role::Title,
            false,
        );
        let Some(active) = &mut self.active else {
            if let Some(error) = &self.catalog_error {
                paint.line(theme, row(left, 1, columns), error, Role::Error, false);
            }
            return;
        };
        let Some(summary) = self.catalog.get(&active.id) else {
            return;
        };
        let draft = self.drafts.get(&active.id);
        let writing = !read_only && !summary.direct;
        let (draft_lines, caret_line) = draft.map_or_else(
            || (Vec::new(), 0),
            |draft| draft.lines(usize::from(columns.saturating_sub(3))),
        );
        let composer_rows = if writing {
            (draft_lines.len().max(1) as u16)
                .min(4)
                .min(size.rows.saturating_sub(3))
        } else {
            0
        };
        let choices = if active.roster_ready {
            draft.map_or(0, |draft| draft.choices(&active.roster).count())
        } else {
            0
        };
        let completion_rows = if draft.is_some_and(|draft| draft.completion.is_some()) && writing {
            (choices.max(1) as u16).min(size.rows.saturating_sub(composer_rows + 4))
        } else {
            0
        };
        let footer = size.rows.saturating_sub(1);
        let composer = footer.saturating_sub(composer_rows);
        let completion_at = composer.saturating_sub(completion_rows);
        let history_rect = Rect {
            x: left,
            y: 1,
            width: columns,
            height: completion_at.saturating_sub(1),
        };
        paint.history = history_rect;
        paint.images.size = history_rect.size(screen);
        let history = &mut active.history;
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
        if history.entries.is_empty() {
            paint.line(
                theme,
                row(left, history_rect.y, columns),
                history.error.as_deref().unwrap_or(if history.loaded {
                    "No messages yet"
                } else {
                    "Loading history…"
                }),
                Role::Muted,
                false,
            );
        }
        let new = summary
            .last_sequence
            .saturating_sub(history.paused_at.unwrap_or_else(|| history.latest()));
        let mut footer_hit = None;
        let status = if let Some(send) = self.sends.get(&active.id) {
            match send {
                Send::Pending(_) => (Role::Info, "Sending…".into()),
                Send::Unknown(_) => (
                    Role::Error,
                    "Send outcome unknown · Ctrl-r retries the same post".into(),
                ),
            }
        } else if let Some(error) =
            draft.and_then(|draft| draft.error.as_deref().or(draft.input.error()))
        {
            (Role::Error, error.to_owned())
        } else if let Some(error) = &history.error {
            (Role::Error, error.clone())
        } else if let Some((role, notice)) = &self.notice {
            (*role, notice.clone())
        } else if history.anchor.is_some() || new > 0 && history.loaded {
            footer_hit = Some(Hit::Latest);
            (
                Role::Info,
                if new > 0 {
                    format!("{new} new · [Latest]")
                } else {
                    "Reading history · [Latest]".into()
                },
            )
        } else if summary.direct {
            (Role::Muted, "Private agent conversation · read-only".into())
        } else if read_only {
            (Role::Muted, "Read-only attachment".into())
        } else if let Some(draft) = draft {
            (Role::Info, draft.wake_preview(summary.settings.user_wake))
        } else {
            (Role::Muted, "Enter sends · Alt-j adds a line".into())
        };
        if size.rows >= 2 {
            let bounds = row(left, footer, columns);
            paint.line(theme, bounds, &status.1, status.0, false);
            if let Some(hit) = footer_hit {
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
                paint.line(
                    theme,
                    row(left, completion_at, columns),
                    label,
                    Role::Muted,
                    false,
                );
            } else {
                for (i, (index, entry, chosen)) in draft
                    .choices(&active.roster)
                    .take(completion_rows.into())
                    .enumerate()
                {
                    let bounds = row(left, completion_at + i as u16, columns);
                    paint.line(
                        theme,
                        bounds,
                        &Draft::selection_label(entry, index),
                        Role::Secondary,
                        chosen,
                    );
                    paint.target(bounds, Hit::Mention(index));
                }
            }
        }
        if composer_rows > 0 {
            let enabled = !self.sends.contains_key(&active.id);
            let has_composer_focus = self.focus == Some(Focus::Composer) && focused && enabled;
            let visible_from = caret_line.saturating_sub(composer_rows.saturating_sub(1) as usize);
            let bounds = Rect {
                x: left,
                y: composer,
                width: columns,
                height: composer_rows,
            };
            paint.target(bounds, Hit::Composer);
            let reply = draft.and_then(|draft| draft.reply_to.as_ref());
            let lead = if reply.is_some() { "↪ " } else { "> " };
            paint.line(
                theme,
                row(left, composer, 2.min(columns)),
                lead,
                Role::Accent,
                false,
            );
            if let Some(draft) = draft {
                for (i, line) in draft_lines
                    .iter()
                    .enumerate()
                    .skip(visible_from)
                    .take(composer_rows.into())
                {
                    let y = composer + (i - visible_from) as u16;
                    let bounds = row(left.saturating_add(2), y, columns.saturating_sub(2));
                    paint.line(
                        theme,
                        bounds,
                        &draft.input.value[line.clone()],
                        Role::Base,
                        false,
                    );
                    for span in draft.highlighted(line) {
                        let x = bounds.x
                            + (text_width(&draft.input.value[line.start..span.start]) as u16)
                                .min(bounds.width);
                        let width = (text_width(&draft.input.value[span]) as u16)
                            .min(bounds.x.saturating_add(bounds.width).saturating_sub(x));
                        style_cells(
                            &mut paint.surface.frame,
                            theme,
                            x,
                            y,
                            width,
                            Role::Secondary,
                            false,
                            None,
                        );
                    }
                }
            }
            if has_composer_focus {
                let col = draft
                    .and_then(|draft| {
                        draft_lines.get(caret_line).map(|line| {
                            text_width(&draft.input.value[line.start..draft.input.cursor])
                        })
                    })
                    .unwrap_or(0);
                paint.surface.frame.cursor = Cursor {
                    x: (left + 2 + col.min(columns.into()) as u16).min(size.cols - 1),
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
    let compact = area.width < 34;
    let nick = if compact {
        0
    } else {
        MAX_NICK.min(area.width / 4)
    };
    let gutter = if compact { 0 } else { nick + 9 };
    let width = area.width.saturating_sub(gutter).max(1);
    let mut total = 0;
    let mut duplicates = HashMap::<&str, Option<&ParticipantId>>::new();
    for entry in entries.iter_mut() {
        entry.wrap(width);
        let len = entry.len(compact, graphics);
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
        let lower = end.min(at + len);
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
            if compact {
                paint.line(
                    theme,
                    row(area.x + 6, y, area.width.saturating_sub(6)),
                    &name,
                    role,
                    false,
                );
            } else {
                let label = window::fit(&name, nick);
                let aligned = (text_width(&label) as u16).min(nick);
                paint.line(
                    theme,
                    row(area.x + 6 + nick - aligned, y, aligned),
                    &label,
                    role,
                    false,
                );
                paint.line(
                    theme,
                    row(area.x + nick + 7, y, 1),
                    "│",
                    Role::Border,
                    false,
                );
            }
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
        for (offset, runs) in entry
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
            for run in runs {
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
        if lower == at + len {
            paint.rendered.push(sequence);
        }
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

fn channel_name(summary: &ChannelSummary) -> String {
    let name = safe_text(&summary.name);
    if summary.direct {
        format!("DM {name}")
    } else if name.starts_with('#') {
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
    style_cells(frame, theme, x, y, width, role, selected, rich);
    width
}

#[allow(clippy::too_many_arguments)]
fn style_cells(
    frame: &mut Frame,
    theme: &Theme,
    x: u16,
    y: u16,
    width: u16,
    role: Role,
    selected: bool,
    rich: Option<markdown::Style>,
) {
    let Some(row) = frame.rows.get_mut(usize::from(y)) else {
        return;
    };
    let style = if selected {
        theme.selected_style(role)
    } else {
        theme.style(role)
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
