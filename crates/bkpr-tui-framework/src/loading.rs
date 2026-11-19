//! Caller-scheduled delayed loading feedback, without terminal or input ownership.
use crate::{Rect, Role, Spinner, Theme, UiSurface, window};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// A pending episode's delayed, caller-styled spinner.
/// Visibility suspends scheduling without resetting the episode's original delay.
pub struct LoadingIndicator {
    spinner: Spinner,
    delay: Duration,
    started: Option<Instant>,
    show_at: Option<Instant>,
    visible: bool,
    shown: bool,
}

impl LoadingIndicator {
    /// Create an idle indicator. Frames must each occupy one cell and be nonempty.
    /// Panics if `frames` is empty, as does [`Spinner::new`].
    pub fn new(now: Instant, delay: Duration, frames: &'static [char]) -> Self {
        Self {
            spinner: Spinner::new(now, frames),
            delay,
            started: None,
            show_at: None,
            visible: false,
            shown: false,
        }
    }

    /// Observe pending work and visibility, coalescing overdue animation ticks.
    /// Returns whether the displayed glyph changed (including appearing/disappearing).
    /// Completion, cancellation and terminal failure must all pass `pending = false`.
    pub fn update(&mut self, pending: bool, visible: bool, now: Instant) -> bool {
        let previous = self.glyph();
        if !pending {
            self.started = None;
            self.show_at = None;
        } else if self.started.is_none() {
            self.started = Some(now);
            self.show_at = now.checked_add(self.delay);
        }
        self.visible = pending && visible;
        self.shown = self.visible
            && self
                .started
                .is_some_and(|start| now.saturating_duration_since(start) >= self.delay);
        self.spinner.set_active(self.shown, now);
        if self.shown {
            self.spinner.advance(now);
        }
        previous != self.glyph()
    }

    /// The single show deadline before the delay, or the next visible animation tick.
    /// Hidden and retired episodes never schedule work.
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.visible {
            None
        } else if self.shown {
            self.spinner.next_deadline()
        } else {
            self.show_at
        }
    }

    /// The visible spinner glyph, absent while hidden, delayed or retired.
    pub fn glyph(&self) -> Option<char> {
        self.shown.then(|| self.spinner.glyph())
    }

    /// Paint one centered, sanitized line on a caller-owned indicator surface.
    /// Only retained text bounds are cleared; cursor, images and placements survive.
    /// The returned tight cell rectangle is the caller's composition clip, not an
    /// input region. Use a dedicated text surface: its retained lines are replaced.
    pub fn paint(
        &self,
        surface: &mut UiSurface,
        bounds: Rect,
        label: &str,
        role: Role,
        theme: &Theme,
    ) -> Option<Rect> {
        let size = surface.frame.size;
        let width = bounds.width.min(size.cols.saturating_sub(bounds.x));
        let height = bounds.height.min(size.rows.saturating_sub(bounds.y));
        let cursor = surface.frame.cursor.clone();
        let Some(glyph) = self.glyph().filter(|_| width != 0 && height != 0) else {
            surface.paint(size, Vec::new(), Some(cursor));
            return None;
        };
        let mut text = glyph.to_string();
        if width > 2 {
            let label = window::fit(label, width - 2);
            if !label.is_empty() {
                text.push(' ');
                text.push_str(&label);
            }
        }
        let text = window::fit(&text, width);
        let columns = UnicodeWidthStr::width(text.as_str()) as u16;
        if columns == 0 {
            surface.paint(size, Vec::new(), Some(cursor));
            return None;
        }
        let rect = Rect {
            x: bounds.x + (width - columns) / 2,
            y: bounds.y + (height - 1) / 2,
            width: columns,
            height: 1,
        };
        surface.paint(size, vec![(rect, text, false)], Some(cursor));
        for cell in &mut surface.frame.rows[usize::from(rect.y)].cells
            [usize::from(rect.x)..usize::from(rect.x) + usize::from(columns)]
        {
            cell.style = theme.style(role).clone();
        }
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::Size;

    const FRAMES: &[char] = &['a', 'b', 'c', 'd'];

    #[test]
    fn fast_completion_never_flashes_and_new_episode_gets_its_own_delay() {
        let now = Instant::now();
        let delay = Duration::from_millis(200);
        let mut indicator = LoadingIndicator::new(now, delay, FRAMES);
        assert!(!indicator.update(true, true, now));
        assert_eq!(indicator.next_deadline(), Some(now + delay));
        assert_eq!(indicator.glyph(), None);
        assert!(!indicator.update(false, true, now + delay / 2));
        assert_eq!(indicator.next_deadline(), None);
        assert!(!indicator.update(true, true, now + delay));
        assert_eq!(indicator.next_deadline(), Some(now + delay * 2));
    }

    #[test]
    fn hidden_episodes_keep_delay_and_ticks_coalesce_by_visible_glyph() {
        let now = Instant::now();
        let delay = Duration::from_millis(200);
        let mut indicator = LoadingIndicator::new(now, delay, FRAMES);
        assert!(!indicator.update(true, false, now));
        assert_eq!(indicator.next_deadline(), None);
        assert!(!indicator.update(true, false, now + delay));
        assert!(indicator.update(true, true, now + delay));
        assert_eq!(indicator.glyph(), Some('a'));
        assert!(!indicator.update(true, true, now + delay + Duration::from_millis(480)));
        assert_eq!(
            indicator.next_deadline(),
            Some(now + delay + Duration::from_millis(600))
        );
        assert!(indicator.update(true, true, now + delay + Duration::from_millis(850)));
        assert_eq!(indicator.glyph(), Some('d'));
        assert!(indicator.update(true, false, now + Duration::from_secs(2)));
        assert_eq!(indicator.next_deadline(), None);
        assert!(indicator.update(true, true, now + Duration::from_secs(3)));
        assert_eq!(indicator.glyph(), Some('a'));
        assert!(indicator.update(false, true, now + Duration::from_secs(4)));
        assert_eq!(indicator.next_deadline(), None);
    }

    #[test]
    fn paint_is_centered_clipped_and_preserves_cursor_and_resources() {
        let now = Instant::now();
        let mut indicator = LoadingIndicator::new(now, Duration::ZERO, FRAMES);
        indicator.update(true, true, now);
        let size = Size {
            cols: 12,
            rows: 5,
            cell_width: 8,
            cell_height: 16,
        };
        let mut surface = UiSurface::new(size);
        surface.frame.cursor.visible = true;
        let cursor = surface.frame.cursor.clone();
        let images = surface.frame.images.clone();
        let placements = surface.frame.placements.clone();
        let rect = indicator
            .paint(
                &mut surface,
                Rect {
                    x: 2,
                    y: 1,
                    width: 8,
                    height: 3,
                },
                "\n界e\u{301}界",
                Role::Accent,
                &Theme::default(),
            )
            .unwrap();
        assert_eq!(
            rect,
            Rect {
                x: 2,
                y: 2,
                width: 7,
                height: 1
            }
        );
        assert_eq!(surface.frame.cursor, cursor);
        assert_eq!(surface.frame.images, images);
        assert_eq!(surface.frame.placements, placements);
        assert!(surface.frame.rows.iter().enumerate().all(|(y, row)| {
            row.cells
                .iter()
                .enumerate()
                .all(|(x, cell)| (y == 2 && (2..9).contains(&x)) || cell.text.trim().is_empty())
        }));
        let tiny = indicator.paint(
            &mut surface,
            Rect {
                x: 11,
                y: 4,
                width: u16::MAX,
                height: u16::MAX,
            },
            "界",
            Role::Accent,
            &Theme::default(),
        );
        assert_eq!(
            tiny,
            Some(Rect {
                x: 11,
                y: 4,
                width: 1,
                height: 1
            })
        );
        assert_eq!(surface.frame.cursor, cursor);
    }
}
