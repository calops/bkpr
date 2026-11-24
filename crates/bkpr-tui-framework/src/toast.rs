//! Bounded, non-modal notifications rendered through retained trusted UI surfaces.
//! Applications own lifetime, placement, theme and optional graphical decoration.

use crate::{
    Rect, Role, RoleSpan, Theme, UiSurface,
    button::{Button, State},
    overlay::PointerPhase,
    window,
};
use bkpr_protocol::{Frame, MouseButton, Size};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const MAX_CARDS: usize = 3;
const MAX_BYTES: usize = 4096;
const MAX_LINES: u16 = 8;
const MAX_WIDTH: u16 = 120;
const DISMISS_LABEL: &str = "x";

/// Stack-issued notification identity, distinct from its reusable decoration slot.
pub type ToastId = u64;

/// Caller-selected top-center placement. Margins shrink on tiny viewports so a
/// dismiss affordance remains available whenever at least one cell exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToastLayout {
    /// Desired card width, clamped to 1–120 cells and available screen width.
    pub max_width: u16,
    /// Desired number of rows above the first card.
    pub top_margin: u16,
    /// Desired minimum horizontal margin on either side.
    pub side_margin: u16,
    /// Number of rows between cards.
    pub gap: u16,
    /// Maximum message rows per card, clamped to 1–8 and available height.
    pub max_lines: u16,
}

/// A retained local surface and its global placement/hitboxes. `body` matches
/// `window::content`; translate it by `outer` for a local `CellsClipped` clip.
/// `slot` is stable for this card and lies in `0..3`, suitable for bounded
/// decoration resource namespaces. It may be reused after the card disappears.
pub struct ToastCard {
    /// Stack-issued notification identity.
    pub id: ToastId,
    /// Reusable decoration namespace in `0..3`.
    pub slot: usize,
    /// Full card bounds in screen cells.
    pub outer: Rect,
    /// Content bounds in screen cells.
    pub body: Rect,
    /// Dismiss-button hitbox in screen cells.
    pub dismiss: Rect,
    /// Optional action-button hitbox in screen cells; omitted when space is tight.
    pub action: Option<Rect>,
    /// Retained card-local text and styling.
    pub surface: UiSurface,
    /// Whether the current paint uses caller-provided graphical decoration.
    pub graphical: bool,
    message: String,
    message_role: Role,
    action_label: Option<String>,
    action_text: String,
    deadline: Instant,
    rows: Vec<String>,
    spans: [RoleSpan; 6],
    painted: Option<(Size, bool)>,
    painted_buttons: Option<[State; 2]>,
}

impl ToastCard {
    fn button_rect(&self, target: ButtonTarget) -> Option<Rect> {
        match target {
            ButtonTarget::Dismiss => Some(self.dismiss).filter(|rect| rect.width != 0),
            ButtonTarget::Action => self.action,
        }
    }
}

fn paint_card_buttons(
    frame: &mut Frame,
    outer: Rect,
    dismiss: Rect,
    action: Option<(Rect, &str)>,
    theme: &Theme,
    states: [State; 2],
    mut place: impl FnMut(&mut Frame, Rect, State) -> bool,
) {
    for (button, state) in [Some((dismiss, DISMISS_LABEL)), action]
        .into_iter()
        .zip(states)
    {
        let Some((rect, label)) = button.filter(|(rect, _)| rect.width != 0) else {
            continue;
        };
        let rect = Rect {
            x: rect.x - outer.x,
            y: rect.y - outer.y,
            ..rect
        };
        let graphical = place(frame, rect, state);
        Button::new(label).paint(frame, rect, theme, state, graphical);
    }
}

/// Result of routing a pointer event through notification cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerRoute {
    /// Deliver to the underlying application.
    Pass,
    /// Consume the event without changing the queue.
    Block,
    /// Consume the event after removing the identified card.
    Dismissed(ToastId),
    /// Consume a valid action click without removing the card or renewing its lifetime.
    Activated(ToastId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonTarget {
    Dismiss,
    Action,
}

#[derive(Clone, Copy)]
struct Capture {
    id: ToastId,
    button: MouseButton,
    target: Option<(ButtonTarget, Rect)>,
    pressed: u8,
}

/// At most three notices are retained; a burst evicts the oldest. Deadlines are
/// fixed at insertion and never renewed by painting. No keyboard focus is owned.
#[derive(Default)]
pub struct ToastStack {
    cards: Vec<ToastCard>,
    next_id: ToastId,
    capture: Option<Capture>,
    pointer: Option<(u16, u16)>,
    layout: Option<(Size, ToastLayout)>,
}

impl ToastStack {
    /// Insert bounded, control-safe text. Byte and visual truncation are marked
    /// with an ellipsis. A zero lifetime expires at `now` on the next `expire`.
    pub fn push(&mut self, message: &str, now: Instant, lifetime: Duration) -> ToastId {
        self.push_styled(message, Role::Panel, now, lifetime)
    }

    /// Insert bounded text with a theme role for the message, leaving the card chrome unchanged.
    pub fn push_styled(
        &mut self,
        message: &str,
        role: Role,
        now: Instant,
        lifetime: Duration,
    ) -> ToastId {
        self.insert(message, role, None, now, lifetime)
    }

    /// Insert a notice with one caller-defined action. Labels are bounded and
    /// control-safe, and may be truncated or omitted on small viewports.
    /// Activation reports its ID without removing the card or changing its deadline.
    pub fn push_with_action(
        &mut self,
        message: &str,
        action_label: &str,
        now: Instant,
        lifetime: Duration,
    ) -> ToastId {
        self.insert(message, Role::Panel, Some(action_label), now, lifetime)
    }

    fn insert(
        &mut self,
        message: &str,
        message_role: Role,
        action_label: Option<&str>,
        now: Instant,
        lifetime: Duration,
    ) -> ToastId {
        self.expire(now);
        if self.cards.len() == MAX_CARDS {
            self.cards.remove(0);
        }
        let slot = (0..MAX_CARDS)
            .find(|slot| self.cards.iter().all(|card| card.slot != *slot))
            .expect("bounded toast slots");
        // Wraparound is practically unreachable; still never reuse a live ID.
        loop {
            self.next_id = self.next_id.wrapping_add(1);
            if self.cards.iter().all(|card| card.id != self.next_id)
                && self
                    .capture
                    .is_none_or(|capture| capture.id != self.next_id)
            {
                break;
            }
        }
        let id = self.next_id;
        self.cards.push(ToastCard {
            id,
            slot,
            outer: Rect::default(),
            body: Rect::default(),
            dismiss: Rect::default(),
            action: None,
            surface: UiSurface::new(Size {
                cols: 1,
                rows: 1,
                cell_width: 0,
                cell_height: 0,
            }),
            graphical: false,
            message: bounded_text(message),
            message_role,
            action_label: action_label.map(bounded_text),
            action_text: String::new(),
            // An unrepresentable lifetime cannot be scheduled. Expiring now is
            // preferable to retaining an unschedulable notification forever.
            deadline: now.checked_add(lifetime).unwrap_or(now),
            rows: Vec::new(),
            spans: [RoleSpan {
                rect: Rect::default(),
                role: Role::Panel,
            }; 6],
            painted: None,
            painted_buttons: None,
        });
        self.layout = None;
        id
    }

    /// Remove notices at or beyond their deadline. Any captured gesture keeps
    /// its tombstone until the matching release, even after expiry or eviction.
    pub fn expire(&mut self, now: Instant) -> bool {
        let old_len = self.cards.len();
        self.cards.retain(|card| now < card.deadline);
        let changed = old_len != self.cards.len();
        if changed {
            self.layout = None;
        }
        changed
    }

    /// Remove a notice by ID, preserving any captured gesture until all releases.
    /// Returns whether the notice was still retained.
    pub fn dismiss(&mut self, id: ToastId) -> bool {
        let old_len = self.cards.len();
        self.cards.retain(|card| card.id != id);
        let changed = old_len != self.cards.len();
        if changed {
            self.layout = None;
        }
        changed
    }

    /// Returns the earliest retained expiry for event-loop scheduling.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.cards.iter().map(|card| card.deadline).min()
    }

    /// Returns whether no notices remain, including unpositioned notices.
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Returns whether this stack owns an unfinished pointer gesture.
    pub fn pointer_captured(&self) -> bool {
        self.capture.is_some()
    }

    /// Clear pointer feedback without dropping any captured physical buttons.
    pub fn clear_button_hover(&mut self) {
        self.pointer = None;
    }

    /// Only positioned cards are returned. A zero-sized viewport has no cards
    /// to submit, without discarding the queue or changing its deadlines.
    pub fn cards(&self) -> impl Iterator<Item = &ToastCard> {
        self.cards
            .iter()
            .filter(|card| card.outer.width != 0 && card.outer.height != 0)
    }

    /// Current control styles in dismiss, then action order. Suitable as a
    /// retained presentation key; missing controls use the idle state.
    pub fn button_states(&self, card: &ToastCard) -> [State; 2] {
        button_states(card, self.pointer, self.capture)
    }

    /// Repaint the controls into a copy of this card's text frame. The callback
    /// may place material at the supplied card-local bounds; false selects plain
    /// painting. The caller owns the lifetime of any material it adds.
    pub fn paint_buttons(
        &self,
        card: &ToastCard,
        frame: &mut Frame,
        theme: &Theme,
        place: impl FnMut(&mut Frame, Rect, State) -> bool,
    ) {
        paint_card_buttons(
            frame,
            card.outer,
            card.dismiss,
            card.action.map(|rect| (rect, card.action_text.as_str())),
            theme,
            self.button_states(card),
            place,
        );
    }

    /// Reflow only when viewport, placement or queue changes. The callback owns
    /// graphical material policy and reports whether decoration is ready for
    /// this card. Paint all cells for plain cards; for graphical cards submit
    /// only local `body` cells alongside the caller's shared decoration frame.
    pub fn paint(
        &mut self,
        screen: Size,
        layout: ToastLayout,
        theme: &Theme,
        mut graphical: impl FnMut(usize, Rect, Rect) -> bool,
    ) {
        if self.layout != Some((screen, layout)) {
            self.reflow(screen, layout);
            self.layout = Some((screen, layout));
        }
        for card in &mut self.cards {
            if card.outer.width == 0 || card.outer.height == 0 {
                continue;
            }
            let size = card.outer.size(screen);
            // Tiny surfaces have no framing cells to replace with graphics.
            card.graphical =
                size.cols >= 3 && size.rows >= 3 && graphical(card.slot, card.outer, card.body);
            if card.painted != Some((size, card.graphical)) {
                let mut lines = if card.graphical {
                    Vec::new()
                } else {
                    window::border_rows(size, "", window::Border::Rounded)
                };
                let body = window::content(size);
                let text_width = text_width(body.width, card.dismiss.width);
                for (row, text) in card.rows.iter().enumerate() {
                    if text_width != 0 {
                        lines.push((
                            Rect {
                                x: body.x,
                                y: body.y + row as u16,
                                width: text_width,
                                height: 1,
                            },
                            text.clone(),
                            false,
                        ));
                    }
                }
                card.surface.paint(size, lines, None);
                card.spans[..5].copy_from_slice(&window::theme_spans(size));
                card.spans[5] = RoleSpan {
                    rect: Rect {
                        width: text_width,
                        height: card.rows.len() as u16,
                        ..body
                    },
                    role: card.message_role,
                };
                card.painted = Some((size, card.graphical));
                card.painted_buttons = None;
            }
            let theme_changed = card.surface.apply_theme(theme, Role::Panel, &card.spans);
            let states = button_states(card, self.pointer, self.capture);
            if !theme_changed && card.painted_buttons == Some(states) {
                continue;
            }
            paint_card_buttons(
                &mut card.surface.frame,
                card.outer,
                card.dismiss,
                card.action.map(|rect| (rect, card.action_text.as_str())),
                theme,
                states,
                |_, _, _| false,
            );
            card.painted_buttons = Some(states);
        }
    }

    fn reflow(&mut self, screen: Size, layout: ToastLayout) {
        let side = layout.side_margin.min(screen.cols.saturating_sub(1) / 2);
        let width = layout
            .max_width
            .clamp(1, MAX_WIDTH)
            .min(screen.cols - side * 2);
        let mut y = layout.top_margin.min(screen.rows.saturating_sub(1));
        for card in &mut self.cards {
            card.outer = Rect::default();
            card.body = Rect::default();
            card.dismiss = Rect::default();
            card.action = None;
            card.action_text.clear();
            if width == 0 || y >= screen.rows {
                continue;
            }
            let remaining = screen.rows - y;
            let framed = width >= 3 && remaining >= 3;
            let inset = if framed { 2 } else { 0 };
            let body_width = width - inset;
            let available_rows = remaining - inset;
            if body_width >= 3
                && available_rows >= 2
                && let Some(label) = &card.action_label
                && let Some(label) = wrap(label, body_width - 2, 1).pop()
            {
                card.action_text = label;
            }
            let footer_rows = u16::from(!card.action_text.is_empty());
            let max_rows = layout
                .max_lines
                .clamp(1, MAX_LINES)
                .min(available_rows - footer_rows);
            let dismiss_width = dismiss_width(body_width);
            card.rows = wrap(
                &card.message,
                text_width(body_width, dismiss_width),
                max_rows,
            );
            let height = (card.rows.len() as u16).max(1) + inset + footer_rows;
            card.outer = Rect {
                x: (screen.cols - width) / 2,
                y,
                width,
                height,
            };
            let body = window::content(card.outer.size(screen));
            card.body = Rect {
                x: card.outer.x + body.x,
                y: y + body.y,
                width: body.width,
                height: body.height,
            };
            card.dismiss = Rect {
                x: card.body.x + card.body.width - dismiss_width,
                y: card.body.y,
                width: dismiss_width,
                height: 1,
            };
            if footer_rows != 0 {
                let action_width = Button::new(&card.action_text).width();
                card.action = Some(Rect {
                    x: card.body.x + card.body.width - action_width,
                    y: card.body.y + card.body.height - 1,
                    width: action_width,
                    height: 1,
                });
            }
            card.painted = None;
            y = y.saturating_add(height).saturating_add(layout.gap);
        }
        if let Some(capture) = &mut self.capture
            && let Some((target, rect)) = capture.target
            && !self
                .cards
                .iter()
                .any(|card| card.id == capture.id && card.button_rect(target) == Some(rect))
        {
            capture.target = None;
        }
    }

    /// Route only when no other widget owns a prior gesture, or this stack
    /// already owns it. A press anywhere on a card captures its whole gesture;
    /// a button activates only on a primary release begun and ended on that button.
    /// Uncaptured outside events pass through; keyboard events need no routing.
    pub fn route_pointer(
        &mut self,
        x: u16,
        y: u16,
        phase: PointerPhase,
        button: MouseButton,
    ) -> PointerRoute {
        self.pointer = Some((x, y));
        if let Some(mut capture) = self.capture {
            let bit = button_bit(button);
            if phase == PointerPhase::Press {
                capture.pressed |= bit;
                self.capture = Some(capture);
            } else if phase == PointerPhase::Release {
                capture.pressed &= !bit;
                let target = if button == capture.button {
                    capture.target.take().and_then(|(target, rect)| {
                        (rect.contains(x, y)
                            && self.cards.iter().any(|card| {
                                card.id == capture.id && card.button_rect(target) == Some(rect)
                            }))
                        .then_some(target)
                    })
                } else {
                    None
                };
                self.capture = (capture.pressed != 0).then_some(capture);
                match target {
                    Some(ButtonTarget::Dismiss) => {
                        self.dismiss(capture.id);
                        return PointerRoute::Dismissed(capture.id);
                    }
                    Some(ButtonTarget::Action) => return PointerRoute::Activated(capture.id),
                    None => {}
                }
            }
            return PointerRoute::Block;
        }
        let Some(card) = self.cards().find(|card| card.outer.contains(x, y)) else {
            return PointerRoute::Pass;
        };
        if phase == PointerPhase::Press && button_bit(button) != 0 {
            self.capture = Some(Capture {
                id: card.id,
                button,
                target: if button != MouseButton::Left {
                    None
                } else {
                    [ButtonTarget::Dismiss, ButtonTarget::Action]
                        .into_iter()
                        .find_map(|target| {
                            card.button_rect(target)
                                .filter(|rect| rect.contains(x, y))
                                .map(|rect| (target, rect))
                        })
                },
                pressed: button_bit(button),
            });
        }
        PointerRoute::Block
    }
}

fn button_states(
    card: &ToastCard,
    pointer: Option<(u16, u16)>,
    capture: Option<Capture>,
) -> [State; 2] {
    [ButtonTarget::Dismiss, ButtonTarget::Action].map(|target| {
        let Some(rect) = card.button_rect(target) else {
            return State::Idle;
        };
        let armed = capture
            .is_some_and(|capture| capture.id == card.id && capture.target == Some((target, rect)));
        State::at(rect, pointer, armed)
    })
}

fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
        MouseButton::Back => 8,
        MouseButton::Forward => 16,
        MouseButton::None
        | MouseButton::WheelUp
        | MouseButton::WheelDown
        | MouseButton::WheelLeft
        | MouseButton::WheelRight => 0,
    }
}

fn dismiss_width(body_width: u16) -> u16 {
    let width = Button::new(DISMISS_LABEL).width();
    if body_width >= width { width } else { 1 }
}

fn text_width(body_width: u16, dismiss: u16) -> u16 {
    body_width.saturating_sub(dismiss + 1)
}

fn bounded_text(message: &str) -> String {
    // Bound work before asking the Unicode segmenter to inspect a cluster:
    // one untrusted cluster may otherwise contain arbitrarily many marks.
    let mut end = message.len().min(MAX_BYTES - '…'.len_utf8());
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    if end < message.len() {
        // The final cluster of a truncated prefix may continue beyond the
        // bound. Drop it whole rather than display an incomplete grapheme.
        end = message[..end]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start);
    }
    let mut text = window::clean(&message[..end]);
    if end < message.len() {
        text.push('…');
    }
    text
}

// An individual glyph wider than the row is represented by an ellipsis.
fn columns(value: &str) -> usize {
    crate::text_width(value)
}

fn wrap(message: &str, width: u16, max_rows: u16) -> Vec<String> {
    if width == 0 || max_rows == 0 {
        return Vec::new();
    }
    let width = usize::from(width);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    let mut graphemes = message.graphemes(true).peekable();
    while let Some(grapheme) = graphemes.next() {
        let advance = unicode_width::UnicodeWidthStr::width(grapheme);
        if used + advance > width && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            used = 0;
            if rows.len() == usize::from(max_rows) {
                ellipsize(rows.last_mut().expect("full wrapped rows"), width);
                return rows;
            }
        }
        if used == 0 && (advance == 0 || grapheme.chars().all(char::is_whitespace)) {
            continue;
        }
        if advance > width {
            row.push('…');
            used += 1;
        } else {
            row.push_str(grapheme);
            used += advance;
        }
        if used == width && graphemes.peek().is_some() && rows.len() + 1 == usize::from(max_rows) {
            ellipsize(&mut row, width);
            rows.push(row);
            return rows;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn ellipsize(row: &mut String, width: usize) {
    while columns(row) + 1 > width {
        let Some((index, _)) = row.grapheme_indices(true).next_back() else {
            break;
        };
        row.truncate(index);
    }
    row.push('…');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: u16, rows: u16) -> Size {
        Size {
            cols,
            rows,
            cell_width: 8,
            cell_height: 16,
        }
    }

    fn layout() -> ToastLayout {
        ToastLayout {
            max_width: 30,
            top_margin: 1,
            side_margin: 1,
            gap: 1,
            max_lines: 3,
        }
    }

    fn paint(stack: &mut ToastStack) {
        stack.paint(screen(80, 24), layout(), &Theme::default(), |_, _, _| false);
    }

    #[test]
    fn expiration_boundary_and_next_deadline_are_fixed() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("first", now, Duration::from_secs(5));
        stack.push("second", now, Duration::from_secs(2));
        paint(&mut stack);
        paint(&mut stack);
        assert_eq!(stack.next_deadline(), Some(now + Duration::from_secs(2)));
        assert!(!stack.expire(now + Duration::from_secs(2) - Duration::from_nanos(1)));
        assert!(stack.expire(now + Duration::from_secs(2)));
        assert_eq!(stack.next_deadline(), Some(now + Duration::from_secs(5)));
        assert!(stack.expire(now + Duration::from_secs(5)));
        assert!(stack.is_empty());
        assert_eq!(stack.next_deadline(), None);
    }

    #[test]
    fn reflow_cancels_button_intent_without_releasing_pointer_ownership() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("first", now, Duration::from_secs(1));
        let id = stack.push("second", now, Duration::from_secs(5));
        paint(&mut stack);
        let card = stack.cards().find(|card| card.id == id).unwrap();
        let old = card.dismiss;
        assert_eq!(stack.button_states(card)[0], State::Idle);

        stack.route_pointer(old.x, old.y, PointerPhase::Motion, MouseButton::None);
        let card = stack.cards().find(|card| card.id == id).unwrap();
        assert_eq!(stack.button_states(card)[0], State::Hover);
        stack.route_pointer(old.x, old.y, PointerPhase::Press, MouseButton::Left);
        let card = stack.cards().find(|card| card.id == id).unwrap();
        assert_eq!(stack.button_states(card)[0], State::Pressed);
        stack.route_pointer(old.x, old.y, PointerPhase::Press, MouseButton::Right);

        assert!(stack.expire(now + Duration::from_secs(1)));
        paint(&mut stack);
        let new = stack.cards().next().unwrap().dismiss;
        assert_ne!(old, new);
        assert_eq!(
            stack.route_pointer(new.x, new.y, PointerPhase::Release, MouseButton::Left),
            PointerRoute::Block
        );
        assert!(stack.pointer_captured());
        assert_eq!(
            stack.route_pointer(0, 23, PointerPhase::Release, MouseButton::Right),
            PointerRoute::Block
        );
        assert!(!stack.pointer_captured());
        assert_eq!(stack.cards().next().unwrap().id, id);
    }

    #[test]
    fn primary_dismiss_click_consumes_the_whole_gesture() {
        let mut stack = ToastStack::default();
        let id = stack.push("saved", Instant::now(), Duration::from_secs(5));
        paint(&mut stack);
        let dismiss = stack.cards().next().unwrap().dismiss;
        assert_eq!(
            stack.route_pointer(0, 23, PointerPhase::Press, MouseButton::Left),
            PointerRoute::Pass
        );
        assert_eq!(
            stack.route_pointer(dismiss.x, dismiss.y, PointerPhase::Press, MouseButton::Left),
            PointerRoute::Block
        );
        assert!(stack.pointer_captured());
        assert_eq!(
            stack.route_pointer(0, 23, PointerPhase::Motion, MouseButton::Left),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Release,
                MouseButton::Left
            ),
            PointerRoute::Dismissed(id)
        );
        assert!(stack.is_empty());
        assert!(!stack.pointer_captured());
        assert_eq!(
            stack.route_pointer(0, 23, PointerPhase::Release, MouseButton::Left),
            PointerRoute::Pass
        );
    }

    #[test]
    fn body_and_secondary_clicks_do_not_dismiss_or_leak() {
        let mut stack = ToastStack::default();
        stack.push("saved", Instant::now(), Duration::from_secs(5));
        paint(&mut stack);
        let card = stack.cards().next().unwrap();
        let body = card.body;
        let dismiss = card.dismiss;
        for (x, y, button) in [
            (body.x, body.y, MouseButton::Left),
            (dismiss.x, dismiss.y, MouseButton::Right),
        ] {
            assert_eq!(
                stack.route_pointer(x, y, PointerPhase::Press, button),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(0, 23, PointerPhase::Release, button),
                PointerRoute::Block
            );
            assert!(!stack.is_empty());
        }
        stack.route_pointer(dismiss.x, dismiss.y, PointerPhase::Press, MouseButton::Left);
        assert_eq!(
            stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Release,
                MouseButton::Right
            ),
            PointerRoute::Block
        );
        assert!(stack.pointer_captured());
        assert_eq!(
            stack.route_pointer(0, 23, PointerPhase::Release, MouseButton::Left),
            PointerRoute::Block
        );
        assert!(!stack.pointer_captured());
        assert!(!stack.is_empty());
    }

    #[test]
    fn captured_expired_card_does_not_dismiss_replacement_or_leak_release() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("first", now, Duration::from_secs(1));
        paint(&mut stack);
        let dismiss = stack.cards().next().unwrap().dismiss;
        stack.route_pointer(dismiss.x, dismiss.y, PointerPhase::Press, MouseButton::Left);
        assert!(stack.expire(now + Duration::from_secs(1)));
        stack.push(
            "replacement",
            now + Duration::from_secs(1),
            Duration::from_secs(5),
        );
        paint(&mut stack);
        assert_eq!(
            stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Wheel,
                MouseButton::WheelDown
            ),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Release,
                MouseButton::Left
            ),
            PointerRoute::Block
        );
        assert!(!stack.is_empty());
        assert!(!stack.pointer_captured());
    }

    #[test]
    fn chorded_buttons_keep_all_consumed_release_tombstones_after_expiry_or_dismissal() {
        for expire in [false, true] {
            let now = Instant::now();
            let mut stack = ToastStack::default();
            let id = stack.push("first", now, Duration::from_secs(1));
            paint(&mut stack);
            let dismiss = stack.cards().next().unwrap().dismiss;
            assert_eq!(
                stack.route_pointer(dismiss.x, dismiss.y, PointerPhase::Press, MouseButton::Left),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(0, 23, PointerPhase::Press, MouseButton::Right),
                PointerRoute::Block
            );
            if expire {
                assert!(stack.expire(now + Duration::from_secs(1)));
            }
            let release = stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Release,
                MouseButton::Left,
            );
            assert_eq!(
                release,
                if expire {
                    PointerRoute::Block
                } else {
                    PointerRoute::Dismissed(id)
                }
            );
            assert!(stack.pointer_captured());
            assert_eq!(
                stack.route_pointer(0, 23, PointerPhase::Motion, MouseButton::Right),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(0, 23, PointerPhase::Release, MouseButton::Right),
                PointerRoute::Block
            );
            assert!(!stack.pointer_captured());
            assert_eq!(
                stack.route_pointer(0, 23, PointerPhase::Motion, MouseButton::None),
                PointerRoute::Pass
            );
        }
    }

    #[test]
    fn bounded_burst_evicts_oldest_and_preserves_live_slots() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        let first = stack.push("first", now, Duration::from_secs(5));
        let second = stack.push("second", now, Duration::from_secs(5));
        let third = stack.push("third", now, Duration::from_secs(5));
        paint(&mut stack);
        let second_slot = stack.cards().find(|card| card.id == second).unwrap().slot;
        stack.push("fourth", now, Duration::from_secs(5));
        paint(&mut stack);
        assert_eq!(stack.cards().count(), 3);
        assert!(stack.cards().all(|card| card.id != first));
        assert!(stack.cards().any(|card| card.id == third));
        assert_eq!(
            stack.cards().find(|card| card.id == second).unwrap().slot,
            second_slot
        );
    }

    #[test]
    fn captured_evicted_card_keeps_its_release_tombstone() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("captured", now, Duration::from_secs(5));
        paint(&mut stack);
        let dismiss = stack.cards().next().unwrap().dismiss;
        stack.route_pointer(dismiss.x, dismiss.y, PointerPhase::Press, MouseButton::Left);
        for _ in 0..3 {
            stack.push("burst", now, Duration::from_secs(5));
        }
        paint(&mut stack);
        assert!(stack.pointer_captured());
        assert_eq!(
            stack.route_pointer(
                dismiss.x,
                dismiss.y,
                PointerPhase::Release,
                MouseButton::Left
            ),
            PointerRoute::Block
        );
        assert!(!stack.pointer_captured());
        assert_eq!(stack.cards().count(), 3);
    }

    #[test]
    fn zero_lifetime_expires_at_insertion_time() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("instant", now, Duration::ZERO);
        assert_eq!(stack.next_deadline(), Some(now));
        assert!(stack.expire(now));
        assert!(stack.is_empty());
    }

    #[test]
    fn unicode_wrap_is_bounded_and_marks_omitted_text() {
        assert_eq!(wrap("e\u{301}界z", 3, 3), vec!["e\u{301}界", "z"]);
        assert_eq!(wrap("abcdef", 3, 1), vec!["ab…"]);
        assert_eq!(wrap("界", 1, 1), vec!["…"]);
        assert_eq!(wrap("👩‍💻✈\u{fe0f}🇫🇷", 2, 3), vec!["👩‍💻", "✈\u{fe0f}", "🇫🇷"]);
        assert_eq!(wrap("👩‍💻x", 2, 1), vec!["…"]);
        let bounded = bounded_text(&"e\u{301}".repeat(MAX_BYTES));
        assert!(bounded.len() <= MAX_BYTES);
        assert!(bounded.ends_with('…'));
        assert!(
            !bounded_text("\u{1b}[31mhi\u{202e}\0")
                .chars()
                .any(char::is_control)
        );
        assert!(!bounded_text("hi\u{202e}").contains('\u{202e}'));
    }

    #[test]
    fn oversized_first_grapheme_is_bounded_before_segmentation_and_omitted_whole() {
        let giant_cluster = format!("a{}", "\u{301}".repeat(MAX_BYTES * 2));
        assert_eq!(bounded_text(&giant_cluster), "…");
        let prefix_then_cluster = format!("ok a{}", "\u{301}".repeat(MAX_BYTES * 2));
        assert_eq!(bounded_text(&prefix_then_cluster), "ok …");
    }

    #[test]
    fn tiny_viewports_keep_dismiss_usable_and_geometry_in_bounds() {
        for cols in 0..10 {
            for rows in 0..6 {
                let mut stack = ToastStack::default();
                stack.push(
                    "界e\u{301}very long notice",
                    Instant::now(),
                    Duration::from_secs(5),
                );
                let size = screen(cols, rows);
                stack.paint(size, layout(), &Theme::default(), |_, _, _| true);
                if cols == 0 || rows == 0 {
                    assert_eq!(stack.cards().count(), 0);
                    continue;
                }
                let card = stack.cards().next().unwrap();
                assert!(card.outer.x + card.outer.width <= cols);
                assert!(card.outer.y + card.outer.height <= rows);
                assert!(card.body.contains(card.dismiss.x, card.dismiss.y));
                assert!(card.dismiss.width >= 1);
                let (x, y) = (card.dismiss.x, card.dismiss.y);
                stack.route_pointer(x, y, PointerPhase::Press, MouseButton::Left);
                assert!(matches!(
                    stack.route_pointer(x, y, PointerPhase::Release, MouseButton::Left),
                    PointerRoute::Dismissed(_)
                ));
            }
        }
    }

    #[test]
    fn rich_readiness_restores_rounded_plain_border_without_changing_deadline() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push("notice", now, Duration::from_secs(5));
        let theme = Theme::default();
        stack.paint(screen(80, 24), layout(), &theme, |_, _, _| true);
        let card = stack.cards().next().unwrap();
        assert!(card.graphical);
        assert_eq!(card.surface.frame.rows[0].cells[0].text, " ");
        stack.paint(screen(80, 24), layout(), &theme, |_, _, _| false);
        let card = stack.cards().next().unwrap();
        assert!(!card.graphical);
        assert_eq!(card.surface.frame.rows[0].cells[0].text, "╭");
        assert_eq!(stack.next_deadline(), Some(now + Duration::from_secs(5)));
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    fn paint(stack: &mut ToastStack, cols: u16, rows: u16) {
        stack.paint(
            Size {
                cols,
                rows,
                cell_width: 8,
                cell_height: 16,
            },
            ToastLayout {
                max_width: 30,
                top_margin: 0,
                side_margin: 0,
                gap: 1,
                max_lines: 3,
            },
            &Theme::default(),
            |_, _, _| false,
        );
    }

    fn route(stack: &mut ToastStack, rect: Rect, phase: PointerPhase) -> PointerRoute {
        stack.route_pointer(rect.x, rect.y, phase, MouseButton::Left)
    }

    #[test]
    fn action_reports_identity_without_dismissal_or_deadline_extension() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        let id = stack.push_with_action("Message", "Details", now, Duration::from_secs(5));
        paint(&mut stack, 80, 24);
        let card = stack.cards().next().unwrap();
        let action = card.action.unwrap();
        let dismiss = card.dismiss;
        assert!(action.y > dismiss.y);
        assert_eq!(
            route(&mut stack, action, PointerPhase::Press),
            PointerRoute::Block
        );
        assert_eq!(
            route(&mut stack, action, PointerPhase::Release),
            PointerRoute::Activated(id)
        );
        assert!(!stack.pointer_captured());
        assert_eq!(stack.cards().count(), 1);
        assert_eq!(stack.next_deadline(), Some(now + Duration::from_secs(5)));
        assert_eq!(
            route(&mut stack, dismiss, PointerPhase::Press),
            PointerRoute::Block
        );
        assert_eq!(
            route(&mut stack, dismiss, PointerPhase::Release),
            PointerRoute::Dismissed(id)
        );
        assert!(!stack.dismiss(id));
    }

    #[test]
    fn action_requires_primary_press_and_release_on_the_same_button() {
        let mut stack = ToastStack::default();
        stack.push_with_action("Message", "Details", Instant::now(), Duration::from_secs(5));
        paint(&mut stack, 80, 24);
        let card = stack.cards().next().unwrap();
        let action = card.action.unwrap();
        let dismiss = card.dismiss;
        let body = card.body;
        for (start, end, button) in [
            (action, body, MouseButton::Left),
            (action, dismiss, MouseButton::Left),
            (dismiss, action, MouseButton::Left),
            (body, action, MouseButton::Left),
            (action, action, MouseButton::Right),
        ] {
            assert_eq!(
                stack.route_pointer(start.x, start.y, PointerPhase::Press, button),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(end.x, end.y, PointerPhase::Motion, button),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(end.x, end.y, PointerPhase::Release, button),
                PointerRoute::Block
            );
            assert!(!stack.pointer_captured());
            assert_eq!(stack.cards().count(), 1);
        }
        route(&mut stack, action, PointerPhase::Press);
        assert_eq!(
            stack.route_pointer(79, 23, PointerPhase::Release, MouseButton::Left),
            PointerRoute::Block
        );
        assert!(!stack.pointer_captured());
    }

    #[test]
    fn removed_expired_and_evicted_actions_capture_every_chord_tail() {
        for removal in 0..3 {
            let now = Instant::now();
            let mut stack = ToastStack::default();
            let id = stack.push_with_action("Message", "Details", now, Duration::from_secs(1));
            paint(&mut stack, 80, 24);
            let action = stack.cards().next().unwrap().action.unwrap();
            route(&mut stack, action, PointerPhase::Press);
            assert_eq!(
                stack.route_pointer(79, 23, PointerPhase::Press, MouseButton::Right),
                PointerRoute::Block
            );
            match removal {
                0 => assert!(stack.dismiss(id)),
                1 => assert!(stack.expire(now + Duration::from_secs(1))),
                _ => {
                    for _ in 0..3 {
                        stack.push_with_action(
                            "Replacement",
                            "Details",
                            now,
                            Duration::from_secs(5),
                        );
                    }
                }
            }
            if removal != 2 {
                stack.push_with_action("Replacement", "Details", now, Duration::from_secs(5));
            }
            paint(&mut stack, 80, 24);
            assert_eq!(
                route(&mut stack, action, PointerPhase::Release),
                PointerRoute::Block
            );
            assert!(stack.pointer_captured());
            assert_eq!(
                stack.route_pointer(79, 23, PointerPhase::Wheel, MouseButton::WheelDown),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(79, 23, PointerPhase::Motion, MouseButton::Right),
                PointerRoute::Block
            );
            assert_eq!(
                stack.route_pointer(79, 23, PointerPhase::Release, MouseButton::Right),
                PointerRoute::Block
            );
            assert!(!stack.pointer_captured());
            assert!(stack.cards().all(|card| card.id != id));
        }
    }

    #[test]
    fn activated_card_can_be_removed_while_chord_tail_remains_owned() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        let id = stack.push_with_action("Message", "Details", now, Duration::from_secs(5));
        paint(&mut stack, 80, 24);
        let action = stack.cards().next().unwrap().action.unwrap();
        route(&mut stack, action, PointerPhase::Press);
        stack.route_pointer(79, 23, PointerPhase::Press, MouseButton::Right);
        assert_eq!(
            route(&mut stack, action, PointerPhase::Release),
            PointerRoute::Activated(id)
        );
        assert!(stack.dismiss(id));
        assert!(stack.pointer_captured());
        assert_eq!(
            route(&mut stack, action, PointerPhase::Release),
            PointerRoute::Block
        );
        assert_eq!(
            stack.route_pointer(79, 23, PointerPhase::Release, MouseButton::Right),
            PointerRoute::Block
        );
        assert!(!stack.pointer_captured());
        assert!(stack.is_empty());
    }

    #[test]
    fn tiny_action_cards_preserve_dismiss_and_never_overlap_message() {
        for cols in 1..14 {
            for rows in 1..7 {
                let mut stack = ToastStack::default();
                let id = stack.push_with_action(
                    "Message",
                    "👩‍💻e\u{301}界",
                    Instant::now(),
                    Duration::from_secs(5),
                );
                paint(&mut stack, cols, rows);
                let card = stack.cards().next().unwrap();
                let dismiss = card.dismiss;
                assert!(card.body.contains(dismiss.x, dismiss.y));
                assert!(card.outer.x + card.outer.width <= cols);
                assert!(card.outer.y + card.outer.height <= rows);
                if let Some(action) = card.action {
                    assert!(card.body.contains(action.x, action.y));
                    assert!(action.x + action.width <= card.body.x + card.body.width);
                    assert!(action.y > dismiss.y);
                    assert!(action.y >= card.body.y + card.rows.len() as u16);
                }
                route(&mut stack, dismiss, PointerPhase::Press);
                assert_eq!(
                    route(&mut stack, dismiss, PointerPhase::Release),
                    PointerRoute::Dismissed(id)
                );
            }
        }
    }

    #[test]
    fn unicode_action_label_is_visible_whole_and_truncates_to_footer_width() {
        let mut stack = ToastStack::default();
        let id = stack.push_with_action(
            "Message",
            "e\u{301}界",
            Instant::now(),
            Duration::from_secs(5),
        );
        paint(&mut stack, 9, 8);
        let card = stack.cards().next().unwrap();
        let action = card.action.unwrap();
        assert_eq!(action.width, 5);
        let row = &card.surface.frame.rows[usize::from(action.y - card.outer.y)];
        assert!(row.cells.iter().any(|cell| cell.text == "e\u{301}"));
        assert!(row.cells.iter().any(|cell| cell.text == "界"));
        paint(&mut stack, 5, 8);
        let card = stack.cards().next().unwrap();
        let action = card.action.unwrap();
        assert_eq!(action.width, 3);
        let row = &card.surface.frame.rows[usize::from(action.y - card.outer.y)];
        assert!(row.cells.iter().any(|cell| cell.text == "…"));
        route(&mut stack, action, PointerPhase::Press);
        assert_eq!(
            route(&mut stack, action, PointerPhase::Release),
            PointerRoute::Activated(id)
        );
    }
}
