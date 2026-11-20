//! Event-loop-driven timing and foreground-only effects for existing UI frames.
use std::time::{Duration, Instant};

use bkpr_protocol::{ColorSource, Frame, Style};

use crate::Rect;

/// A dormant-by-default periodic clock. The caller owns waiting and repainting.
/// Activation restarts elapsed time; missed ticks coalesce against its original epoch.
pub struct AnimationClock {
    epoch: Instant,
    interval: Duration,
    next: Option<Instant>,
    elapsed: Duration,
    active: bool,
}

impl AnimationClock {
    /// Create an inactive clock with the given positive tick interval.
    ///
    /// # Panics
    /// Panics if `interval` is zero.
    pub fn new(now: Instant, interval: Duration) -> Self {
        assert!(!interval.is_zero(), "animation interval must be positive");
        Self {
            epoch: now,
            interval,
            next: None,
            elapsed: Duration::ZERO,
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
        self.elapsed = Duration::ZERO;
        self.next = active.then(|| now.checked_add(self.interval)).flatten();
        true
    }

    /// Sample active elapsed time once when due, without replaying missed ticks.
    /// Return whether the sampled elapsed time changed.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.next.is_none_or(|next| now < next) {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.epoch);
        let changed = self.elapsed != elapsed;
        self.elapsed = elapsed;
        let remainder = elapsed.as_nanos() % self.interval.as_nanos();
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

    /// Most recently sampled time since activation; zero while inactive.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// A fixed-width band traveling left to right at a constant terminal-cell velocity.
/// Each rectangle repeats independently. Restore stable styling before application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSweep {
    /// RGB ink color at the center of the band.
    pub highlight: [u8; 3],
    /// Band half-width in terminal cells; finite and positive.
    pub half_width_cells: f64,
    /// Left-to-right velocity in terminal cells per second; finite and positive.
    pub cells_per_second: f64,
}

impl TextSweep {
    /// Mix explicit RGB visible ink toward the highlight, returning actual changes.
    /// Inverse text uses its background channel as ink, preserving its displayed
    /// background and all attributes. Default/indexed ink is intentionally skipped.
    /// Whole wide graphemes share one weight; clipped graphemes are left untouched.
    /// Cursor, graphics resources, text and hyperlinks are never modified.
    /// The band starts entirely left of `rect`, wrapping after crossing its right edge.
    /// Invalid half-width or velocity leaves the frame unchanged.
    /// Weights use smoothstep falloff; RGB channels are interpolated and rounded.
    pub fn apply(&self, frame: &mut Frame, rect: Rect, elapsed: Duration) -> bool {
        self.apply_bands(frame, rect, elapsed, None)
    }

    /// Apply bands at an explicit positive, finite terminal-cell spacing.
    ///
    /// Spacing and velocity determine repetition frequency independently of band
    /// width. Overlapping bands apply their strongest weight once. Invalid
    /// spacing leaves the frame unchanged; other constraints match [`Self::apply`].
    pub fn apply_periodic(
        &self,
        frame: &mut Frame,
        rect: Rect,
        elapsed: Duration,
        spacing_cells: f64,
    ) -> bool {
        if !spacing_cells.is_finite() || spacing_cells <= 0.0 {
            return false;
        }
        self.apply_bands(frame, rect, elapsed, Some(spacing_cells))
    }

    fn apply_bands(
        &self,
        frame: &mut Frame,
        rect: Rect,
        elapsed: Duration,
        spacing: Option<f64>,
    ) -> bool {
        if rect.width == 0
            || rect.height == 0
            || !self.half_width_cells.is_finite()
            || self.half_width_cells <= 0.0
            || !self.cells_per_second.is_finite()
            || self.cells_per_second <= 0.0
        {
            return false;
        }
        let left = usize::from(rect.x);
        let right = usize::from(rect.x.saturating_add(rect.width).min(frame.size.cols));
        let bottom = usize::from(rect.y.saturating_add(rect.height).min(frame.size.rows));
        let seconds = elapsed.as_secs_f64();
        let period = spacing.unwrap_or(f64::from(rect.width) + 2.0 * self.half_width_cells);
        let center = if period.is_finite() {
            let distance = seconds * self.cells_per_second;
            let progress = if distance.is_finite() {
                distance % period
            } else {
                (seconds % (period / self.cells_per_second)) * self.cells_per_second
            };
            progress - self.half_width_cells
        } else {
            // Normalize enormous bands before either distance or period can overflow.
            let period = f64::from(rect.width) / self.half_width_cells + 2.0;
            let distance = seconds * (self.cells_per_second / self.half_width_cells);
            (distance % period - 1.0) * self.half_width_cells
        };
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
                let distance = if let Some(spacing) = spacing {
                    let offset = (position - center).rem_euclid(spacing);
                    offset.min(spacing - offset)
                } else {
                    (position - center).abs()
                } / self.half_width_cells;
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

    #[test]
    fn hidden_clocks_have_no_deadlines_and_reactivation_restarts_elapsed_time() {
        let now = Instant::now();
        let mut clock = AnimationClock::new(now, INTERVAL);
        assert_eq!(clock.next_deadline(), None);
        assert_eq!(clock.elapsed(), Duration::ZERO);
        assert!(!clock.advance(now + Duration::from_secs(1)));
        assert!(clock.set_active(true, now));
        assert_eq!(clock.next_deadline(), Some(now + INTERVAL));
        assert!(clock.advance(now + INTERVAL));
        assert_eq!(clock.elapsed(), INTERVAL);
        assert!(!clock.set_active(true, now + INTERVAL));
        assert_eq!(clock.next_deadline(), Some(now + INTERVAL * 2));
        assert!(clock.set_active(false, now + INTERVAL));
        assert_eq!(clock.next_deadline(), None);
        assert_eq!(clock.elapsed(), Duration::ZERO);
        let resumed = now + Duration::from_secs(86_400);
        assert!(!clock.advance(resumed));
        assert!(clock.set_active(true, resumed));
        assert_eq!(clock.next_deadline(), Some(resumed + INTERVAL));
        assert_eq!(clock.elapsed(), Duration::ZERO);
        assert!(clock.advance(resumed + INTERVAL));
        assert_eq!(clock.elapsed(), INTERVAL);
    }

    #[test]
    fn delayed_ticks_sample_elapsed_time_without_moving_the_original_schedule() {
        let now = Instant::now();
        let mut clock = AnimationClock::new(now, INTERVAL);
        clock.set_active(true, now);
        assert!(!clock.advance(now + INTERVAL - Duration::from_nanos(1)));
        assert_eq!(clock.elapsed(), Duration::ZERO);
        assert!(clock.advance(now + INTERVAL));
        assert_eq!(clock.elapsed(), INTERVAL);
        let skipped = INTERVAL * 301;
        assert!(clock.advance(now + skipped));
        assert_eq!(clock.elapsed(), skipped);
        assert_eq!(clock.next_deadline(), Some(now + skipped + INTERVAL));
        let delayed = Duration::from_millis(6067);
        assert!(clock.advance(now + delayed));
        assert_eq!(clock.elapsed(), delayed);
        assert_eq!(
            clock.next_deadline(),
            Some(now + Duration::from_millis(6080))
        );
        assert!(!clock.advance(now + delayed));
        assert!(!clock.advance(now + Duration::from_millis(6079)));
        assert_eq!(clock.elapsed(), delayed);
    }

    #[test]
    fn clocks_handle_unrepresentable_deadlines_without_panicking() {
        let now = Instant::now();
        let mut clock = AnimationClock::new(now, Duration::MAX);
        assert!(clock.set_active(true, now));
        assert_eq!(clock.next_deadline(), now.checked_add(Duration::MAX));
        assert!(!clock.advance(now));
        assert_eq!(clock.elapsed(), Duration::ZERO);
        assert!(clock.set_active(false, now));
        assert_eq!(clock.next_deadline(), None);
    }

    fn ink_line(width: u16) -> Frame {
        let mut frame = crate::blank(Size {
            cols: width,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        });
        crate::text(&mut frame, 0, 0, &"x".repeat(usize::from(width)), false);
        for cell in &mut frame.rows[0].cells {
            cell.style.foreground = [0; 3];
            cell.style.foreground_source = ColorSource::Rgb([0; 3]);
        }
        frame
    }

    #[test]
    fn short_and_long_rectangles_share_cell_displacement_and_band_width() {
        let sweep = TextSweep {
            highlight: [255; 3],
            half_width_cells: 2.0,
            cells_per_second: 4.0,
        };
        for (millis, center_cell) in [(625, 0), (1125, 2)] {
            let mut short = ink_line(8);
            let mut long = ink_line(20);
            let short_rect = Rect {
                x: 0,
                y: 0,
                width: 8,
                height: 1,
            };
            let long_rect = Rect {
                width: 20,
                ..short_rect
            };
            let elapsed = Duration::from_millis(millis);
            assert!(sweep.apply(&mut short, short_rect, elapsed));
            assert!(sweep.apply(&mut long, long_rect, elapsed));
            for x in 0..8 {
                assert_eq!(short.rows[0].cells[x].style, long.rows[0].cells[x].style);
            }
            assert_eq!(
                short.rows[0].cells[center_cell].style.foreground,
                sweep.highlight
            );
            assert_eq!(
                short.rows[0].cells[center_cell + 1].style.foreground,
                [128; 3]
            );
            assert_eq!(
                short.rows[0].cells[center_cell + 2].style.foreground,
                [0; 3]
            );
        }
    }

    #[test]
    fn repeated_bands_keep_speed_and_do_not_amplify_overlapping_ink() {
        let sweep = TextSweep {
            highlight: [255; 3],
            half_width_cells: 2.0,
            cells_per_second: 4.0,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 28,
            height: 1,
        };
        for (millis, peaks) in [(625, [0, 16]), (1125, [2, 18])] {
            let mut frame = ink_line(rect.width);
            sweep.apply_periodic(&mut frame, rect, Duration::from_millis(millis), 16.0);
            for x in peaks {
                assert_eq!(frame.rows[0].cells[x].style.foreground, [255; 3]);
                assert_eq!(frame.rows[0].cells[x + 1].style.foreground, [128; 3]);
                assert_eq!(frame.rows[0].cells[x + 2].style.foreground, [0; 3]);
            }
        }
        let mut overlap = ink_line(4);
        let sweep = TextSweep {
            half_width_cells: 8.0,
            ..sweep
        };
        let rect = Rect { width: 4, ..rect };
        sweep.apply_periodic(&mut overlap, rect, Duration::from_millis(2125), 10.0);
        assert_eq!(overlap.rows[0].cells[0].style.foreground, [255; 3]);
        assert_eq!(overlap.rows[0].cells[3].style.foreground, [174; 3]);
    }

    #[test]
    fn wider_bands_keep_the_same_repetition_period_and_color_more_cells() {
        let rect = Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 1,
        };
        let mut colored = Vec::new();
        for half_width in [2.0, 6.0] {
            let sweep = TextSweep {
                highlight: [255; 3],
                half_width_cells: half_width,
                cells_per_second: 4.0,
            };
            let elapsed = Duration::from_secs_f64((half_width + 0.5) / 4.0);
            let mut first = ink_line(rect.width);
            sweep.apply_periodic(&mut first, rect, elapsed, 16.0);
            let mut next = ink_line(rect.width);
            sweep.apply_periodic(&mut next, rect, elapsed + Duration::from_secs(4), 16.0);
            assert_eq!(first.rows, next.rows);
            for column in [0, 16] {
                assert_eq!(first.rows[0].cells[column].style.foreground, [255; 3]);
            }
            colored.push(
                first.rows[0]
                    .cells
                    .iter()
                    .filter(|cell| cell.style.foreground != [0; 3])
                    .count(),
            );
        }
        assert!(colored[1] > colored[0]);
    }

    #[test]
    fn rectangles_wrap_on_their_own_cell_distance_periods() {
        let sweep = TextSweep {
            highlight: [255; 3],
            half_width_cells: 2.0,
            cells_per_second: 4.0,
        };
        let short_rect = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 1,
        };
        let long_rect = Rect {
            width: 20,
            ..short_rect
        };
        let mut short = ink_line(8);
        let mut long = ink_line(20);
        assert!(sweep.apply(&mut short, short_rect, Duration::from_millis(625)));
        assert!(sweep.apply(&mut long, long_rect, Duration::from_millis(625)));
        let mut short_repeat = ink_line(8);
        assert!(sweep.apply(&mut short_repeat, short_rect, Duration::from_millis(3625)));
        assert_eq!(short_repeat, short);
        let mut long_later = ink_line(20);
        assert!(sweep.apply(&mut long_later, long_rect, Duration::from_millis(3625)));
        assert_ne!(long_later, long);
        assert_eq!(long_later.rows[0].cells[12].style.foreground, [255; 3]);
        let mut long_repeat = ink_line(20);
        assert!(sweep.apply(&mut long_repeat, long_rect, Duration::from_millis(6625)));
        assert_eq!(long_repeat, long);
    }

    #[test]
    fn invalid_sweep_parameters_leave_frames_unchanged() {
        let baseline = fixture();
        let rect = Rect {
            x: 1,
            y: 0,
            width: 5,
            height: 2,
        };
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for (half_width_cells, cells_per_second) in [(invalid, 4.0), (2.0, invalid)] {
                let mut frame = baseline.clone();
                let sweep = TextSweep {
                    highlight: [255; 3],
                    half_width_cells,
                    cells_per_second,
                };
                assert!(!sweep.apply(&mut frame, rect, Duration::MAX));
                assert_eq!(frame, baseline);
            }
        }
    }

    #[test]
    fn finite_extreme_parameters_keep_ink_and_other_frame_fields_valid() {
        let baseline = fixture();
        let rect = Rect {
            x: 1,
            y: 0,
            width: 5,
            height: 2,
        };
        for (half_width_cells, cells_per_second) in [
            (2.0, f64::MAX),
            (f64::MAX, f64::MAX),
            (f64::from_bits(1), f64::MAX),
            (2.0, f64::from_bits(1)),
        ] {
            let sweep = TextSweep {
                highlight: [240, 220, 200],
                half_width_cells,
                cells_per_second,
            };
            let mut frame = baseline.clone();
            sweep.apply(&mut frame, rect, Duration::MAX);
            for y in 0..2 {
                for x in 1..6 {
                    let actual = &frame.rows[y].cells[x].style;
                    let before = &baseline.rows[y].cells[x].style;
                    let (ink, base, source) = if before.inverse {
                        (
                            actual.background,
                            before.background,
                            &actual.background_source,
                        )
                    } else {
                        (
                            actual.foreground,
                            before.foreground,
                            &actual.foreground_source,
                        )
                    };
                    assert_eq!(*source, ColorSource::Rgb(ink));
                    for channel in 0..3 {
                        assert!(ink[channel] >= base[channel]);
                        assert!(ink[channel] <= sweep.highlight[channel]);
                    }
                    frame.rows[y].cells[x].style = before.clone();
                }
            }
            assert_eq!(frame, baseline);
        }
    }

    #[test]
    fn finite_extreme_band_widths_still_reach_visible_ink() {
        for (half_width_cells, cells_per_second) in [(f64::from_bits(1), 0.5), (f64::MAX, f64::MAX)]
        {
            let mut frame = ink_line(8);
            let sweep = TextSweep {
                highlight: [255; 3],
                half_width_cells,
                cells_per_second,
            };
            assert!(sweep.apply(
                &mut frame,
                Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 1,
                },
                Duration::from_secs(1)
            ));
            assert_eq!(frame.rows[0].cells[0].style.foreground, [255; 3]);
        }
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
            source: None,
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
            half_width_cells: 3.0,
            cells_per_second: 5.5,
        };
        assert!(sweep.apply(
            &mut frame,
            Rect {
                x: 1,
                y: 0,
                width: 5,
                height: 2
            },
            Duration::from_secs(1)
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
            half_width_cells: 20.0,
            cells_per_second: 22.0,
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
            Duration::from_secs(1)
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
            Duration::from_secs(1)
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
            Duration::from_secs(1)
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
            half_width_cells: 2.5,
            cells_per_second: 5.0,
        };
        let mut middle = baseline.clone();
        assert!(sweep.apply(&mut middle, rect, Duration::from_secs(1)));
        assert!(
            middle.rows[0].cells[2].style.foreground[0]
                > middle.rows[0].cells[1].style.foreground[0]
        );
        let mut replay = baseline.clone();
        assert!(sweep.apply(&mut replay, rect, Duration::from_secs(1)));
        assert_eq!(replay, middle);
        for elapsed in [Duration::ZERO, Duration::from_secs(2)] {
            let mut frame = baseline.clone();
            assert!(!sweep.apply(&mut frame, rect, elapsed));
            assert_eq!(frame, baseline);
        }
    }
}
