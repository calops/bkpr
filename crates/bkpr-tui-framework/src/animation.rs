//! Event-loop-driven timing and foreground-only effects for existing UI frames.
use std::time::{Duration, Instant};

use bkpr_protocol::{ColorSource, Frame, Style};

use crate::Rect;

/// A dormant-by-default periodic clock. The caller owns waiting and repainting.
/// Activation restarts the cycle; missed ticks coalesce against its original epoch.
pub struct AnimationClock {
    epoch: Instant,
    interval: Duration,
    cycle: Duration,
    next: Option<Instant>,
    phase_nanos: u128,
    active: bool,
}

impl AnimationClock {
    /// Create an inactive clock with the given tick interval and phase cycle.
    ///
    /// # Panics
    /// Panics if `interval` or `cycle` is zero.
    pub fn new(now: Instant, interval: Duration, cycle: Duration) -> Self {
        assert!(!interval.is_zero(), "animation interval must be positive");
        assert!(!cycle.is_zero(), "animation cycle must be positive");
        Self {
            epoch: now,
            interval,
            cycle,
            next: None,
            phase_nanos: 0,
            active: false,
        }
    }

    /// Return whether activation changed. Repeated activation retains its epoch.
    pub fn set_active(&mut self, active: bool, now: Instant) -> bool {
        if self.active == active {
            return false;
        }
        self.active = active;
        self.epoch = now;
        self.phase_nanos = 0;
        self.next = active.then(|| now.checked_add(self.interval)).flatten();
        true
    }

    /// Sample the current phase once when due, without replaying missed ticks.
    /// Return whether the sampled phase changed, rather than merely a tick being due.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.next.is_none_or(|next| now < next) {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.epoch).as_nanos();
        let phase_nanos = elapsed % self.cycle.as_nanos();
        let changed = self.phase_nanos != phase_nanos;
        self.phase_nanos = phase_nanos;
        let remainder = elapsed % self.interval.as_nanos();
        let remainder = Duration::new(
            (remainder / 1_000_000_000) as u64,
            (remainder % 1_000_000_000) as u32,
        );
        self.next = now.checked_add(self.interval - remainder);
        changed
    }

    /// Next sampling deadline, or `None` when inactive or beyond `Instant`'s range.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.next
    }

    /// Most recently sampled cycle position in `[0, 1)`; zero while inactive.
    pub fn phase(&self) -> f64 {
        (self.phase_nanos as f64 / self.cycle.as_nanos() as f64).min(1.0 - f64::EPSILON)
    }
}

/// A smooth band traveling left to right across the requested rectangle.
/// This effect is stateless: restore stable styling before each application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSweep {
    /// RGB ink color at the center of the band.
    pub highlight: [u8; 3],
    /// Band half-width as a fraction of the rectangle's width; finite and positive.
    pub width: f64,
}

impl TextSweep {
    /// Mix explicit RGB visible ink toward the highlight, returning actual changes.
    /// Inverse text uses its background channel as ink, preserving its displayed
    /// background and all attributes. Default/indexed ink is intentionally skipped.
    /// Whole wide graphemes share one weight; clipped graphemes are left untouched.
    /// Cursor, graphics resources, text and hyperlinks are never modified.
    /// Finite phases are clamped to `[0, 1]`; invalid phase or width leaves the frame unchanged.
    /// Weights use smoothstep falloff; RGB channels are interpolated and rounded.
    pub fn apply(&self, frame: &mut Frame, rect: Rect, phase: f64) -> bool {
        if rect.width == 0
            || rect.height == 0
            || !self.width.is_finite()
            || self.width <= 0.0
            || !phase.is_finite()
        {
            return false;
        }
        let left = usize::from(rect.x);
        let right = usize::from(rect.x.saturating_add(rect.width).min(frame.size.cols));
        let bottom = usize::from(rect.y.saturating_add(rect.height).min(frame.size.rows));
        // Normalizing by the band width avoids overflow for very wide bands.
        let center = phase.clamp(0.0, 1.0) * (1.0 / self.width + 2.0) - 1.0;
        let mut changed = false;
        for row in frame.rows.iter_mut().take(bottom).skip(usize::from(rect.y)) {
            let end = right.min(row.cells.len());
            let mut x = left;
            while x < end {
                let cell = &row.cells[x];
                let width = usize::from(cell.width);
                if width == 0
                    || width > 2
                    || x + width > end
                    || cell.style.invisible
                    || cell.text.is_empty()
                    || cell.text.chars().all(char::is_whitespace)
                {
                    x += 1;
                    continue;
                }
                if row.cells[x + 1..x + width]
                    .iter()
                    .any(|cell| cell.width != 0)
                {
                    x += width;
                    continue;
                }
                let position = (x - left) as f64 + width as f64 * 0.5;
                let distance = (position / f64::from(rect.width) / self.width - center).abs();
                if distance < 1.0 {
                    let t = 1.0 - distance;
                    let weight = t * t * (3.0 - 2.0 * t);
                    for cell in &mut row.cells[x..x + width] {
                        changed |= mix_ink(&mut cell.style, self.highlight, weight);
                    }
                }
                x += width;
            }
        }
        changed
    }
}

fn mix_ink(style: &mut Style, highlight: [u8; 3], weight: f64) -> bool {
    if style.invisible {
        return false;
    }
    let (ink, source) = if style.inverse {
        (&mut style.background, &mut style.background_source)
    } else {
        (&mut style.foreground, &mut style.foreground_source)
    };
    if !matches!(source, ColorSource::Rgb(_)) {
        return false;
    }
    let mixed = std::array::from_fn(|channel| {
        let base = f64::from(ink[channel]);
        (base + (f64::from(highlight[channel]) - base) * weight).round() as u8
    });
    if *ink == mixed {
        return false;
    }
    *ink = mixed;
    *source = ColorSource::Rgb(mixed);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{Image, ImageLayer, PixelFormat, Placement, Size};
    use std::sync::Arc;

    const INTERVAL: Duration = Duration::from_millis(20);
    const CYCLE: Duration = Duration::from_millis(200);

    #[test]
    fn inactive_clocks_have_no_deadlines_and_reactivation_restarts() {
        let now = Instant::now();
        let mut clock = AnimationClock::new(now, INTERVAL, CYCLE);
        assert_eq!(clock.next_deadline(), None);
        assert_eq!(clock.phase(), 0.0);
        assert!(!clock.advance(now + CYCLE));
        assert!(clock.set_active(true, now));
        assert_eq!(clock.next_deadline(), Some(now + INTERVAL));
        assert!(clock.advance(now + INTERVAL));
        assert_eq!(clock.phase(), 0.1);
        assert!(!clock.set_active(true, now + INTERVAL));
        assert_eq!(clock.next_deadline(), Some(now + INTERVAL * 2));
        assert!(clock.set_active(false, now + INTERVAL));
        assert_eq!(clock.next_deadline(), None);
        assert_eq!(clock.phase(), 0.0);
        assert!(!clock.advance(now + CYCLE * 100));
        let resumed = now + CYCLE * 101;
        assert!(clock.set_active(true, resumed));
        assert_eq!(clock.next_deadline(), Some(resumed + INTERVAL));
        assert_eq!(clock.phase(), 0.0);
    }

    #[test]
    fn delayed_ticks_coalesce_without_moving_the_original_schedule() {
        let now = Instant::now();
        let mut clock = AnimationClock::new(now, INTERVAL, CYCLE);
        clock.set_active(true, now);
        assert!(!clock.advance(now + INTERVAL - Duration::from_nanos(1)));
        assert!(clock.advance(now + INTERVAL));
        assert_eq!(clock.phase(), 0.1);
        assert!(!clock.advance(now + CYCLE * 30 + INTERVAL));
        assert_eq!(clock.phase(), 0.1);
        assert_eq!(clock.next_deadline(), Some(now + CYCLE * 30 + INTERVAL * 2));
        let delayed = now + CYCLE * 30 + Duration::from_millis(67);
        assert!(clock.advance(delayed));
        assert_eq!(clock.phase(), 0.335);
        assert_eq!(clock.next_deadline(), Some(now + CYCLE * 30 + INTERVAL * 4));
        assert!(!clock.advance(delayed));
    }

    fn fixture() -> Frame {
        let mut frame = crate::blank(Size {
            cols: 10,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        });
        crate::text(&mut frame, 1, 0, "a界e\u{301}z", false);
        crate::text(&mut frame, 1, 1, "a界e\u{301}z", true);
        for row in &mut frame.rows {
            for cell in &mut row.cells {
                cell.style.foreground = [40, 80, 120];
                cell.style.foreground_source = ColorSource::Rgb([40, 80, 120]);
                cell.style.background = [10, 20, 30];
                cell.style.background_source = ColorSource::Rgb([10, 20, 30]);
                cell.style.bold = true;
                cell.style.italic = true;
                cell.style.faint = true;
                cell.style.underline = 2;
                cell.hyperlink = Some("https://example.com".into());
            }
        }
        frame.images.push(Arc::new(Image {
            id: 7,
            generation: 2,
            width: 1,
            height: 1,
            format: PixelFormat::Rgba,
            pixels: vec![10, 20, 30, 128],
        }));
        frame.placements.push(Placement {
            image_id: 7,
            generation: 2,
            placement_id: 3,
            col: 2,
            row: 1,
            offset_x: 0,
            offset_y: 0,
            width: 8,
            height: 16,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            layer: ImageLayer::BelowText,
            z: 0,
        });
        frame.cursor.visible = true;
        frame
    }

    #[test]
    fn sweep_changes_only_visible_ink_preserving_selection_and_wide_graphemes() {
        let mut frame = fixture();
        let baseline = frame.clone();
        let sweep = TextSweep {
            highlight: [240, 220, 200],
            width: 0.75,
        };
        assert!(sweep.apply(
            &mut frame,
            Rect {
                x: 1,
                y: 0,
                width: 5,
                height: 2
            },
            0.5
        ));
        for y in 0..2 {
            assert_ne!(
                frame.rows[y].cells[2].style,
                baseline.rows[y].cells[2].style
            );
            assert_eq!(frame.rows[y].cells[2].style, frame.rows[y].cells[3].style);
            for x in 1..6 {
                let actual = &frame.rows[y].cells[x];
                let before = &baseline.rows[y].cells[x];
                let mut expected = before.clone();
                if before.style.inverse {
                    expected.style.background = actual.style.background;
                    expected.style.background_source = actual.style.background_source;
                    assert_eq!(actual.style.foreground, before.style.foreground);
                } else {
                    expected.style.foreground = actual.style.foreground;
                    expected.style.foreground_source = actual.style.foreground_source;
                    assert_eq!(actual.style.background, before.style.background);
                }
                assert_eq!(*actual, expected);
            }
        }
        // Restoring only the expected ink changes must recover every other field,
        // including cursor state, graphics alpha, resources and frame metadata.
        let mut restored = frame.clone();
        for y in 0..2 {
            for x in 1..6 {
                restored.rows[y].cells[x].style = baseline.rows[y].cells[x].style.clone();
            }
        }
        assert_eq!(restored, baseline);
        assert!(Arc::ptr_eq(&frame.images[0], &baseline.images[0]));
    }

    #[test]
    fn sweep_skips_unresolved_invisible_and_partially_clipped_ink() {
        let mut frame = fixture();
        frame.rows[0].cells[1].style.foreground_source = ColorSource::Default;
        frame.rows[0].cells[4].style.foreground_source = ColorSource::Palette(2);
        frame.rows[0].cells[5].style.invisible = true;
        frame.rows[1].cells[1].style.background_source = ColorSource::Default;
        frame.rows[1].cells[4].style.background_source = ColorSource::Palette(3);
        frame.rows[1].cells[5].style.invisible = true;
        let baseline = frame.clone();
        let sweep = TextSweep {
            highlight: [255; 3],
            width: 1.0,
        };
        // Starts at the wide glyph's continuation, never colors half a glyph.
        assert!(!sweep.apply(
            &mut frame,
            Rect {
                x: 3,
                y: 0,
                width: 20,
                height: 20
            },
            0.5
        ));
        assert_eq!(frame, baseline);
        assert!(!sweep.apply(
            &mut frame,
            Rect {
                x: 1,
                y: 0,
                width: 2,
                height: 2
            },
            0.5
        ));
        assert_eq!(frame, baseline);
        assert!(!sweep.apply(
            &mut frame,
            Rect {
                x: u16::MAX,
                y: u16::MAX,
                width: 1,
                height: 1
            },
            0.5
        ));
    }

    #[test]
    fn sweep_has_smooth_falloff_and_no_accumulation_after_baseline_restore() {
        let baseline = fixture();
        let rect = Rect {
            x: 1,
            y: 0,
            width: 5,
            height: 1,
        };
        let sweep = TextSweep {
            highlight: [240, 220, 200],
            width: 0.5,
        };
        let mut middle = baseline.clone();
        assert!(sweep.apply(&mut middle, rect, 0.5));
        assert!(
            middle.rows[0].cells[2].style.foreground[0]
                > middle.rows[0].cells[1].style.foreground[0]
        );
        let mut replay = baseline.clone();
        assert!(sweep.apply(&mut replay, rect, 0.5));
        assert_eq!(replay, middle);
        for phase in [0.0, 1.0, f64::NAN] {
            let mut frame = baseline.clone();
            assert!(!sweep.apply(&mut frame, rect, phase));
            assert_eq!(frame, baseline);
        }
    }
}
