//! A one-cell progress indicator driven by the caller's existing event loop.
use std::time::{Duration, Instant};

const PERIOD: Duration = Duration::from_millis(120);

/// Retained, 120-ms frame sequence with caller-scheduled deadlines.
pub struct Spinner {
    epoch: Instant,
    next: Option<Instant>,
    frame: usize,
    frames: &'static [char],
    active: bool,
}

impl Spinner {
    /// Frames are presentation policy supplied by the caller, with no per-tick allocation.
    /// Each frame must occupy one display cell.
    ///
    /// Panics if `frames` is empty.
    pub fn new(now: Instant, frames: &'static [char]) -> Self {
        assert!(!frames.is_empty(), "a spinner requires at least one frame");
        Self {
            epoch: now,
            next: None,
            frame: 0,
            frames,
            active: false,
        }
    }

    /// Changes activity, restarting at frame zero; returns whether activity changed.
    pub fn set_active(&mut self, active: bool, now: Instant) -> bool {
        if self.active == active {
            return false;
        }
        self.active = active;
        self.epoch = now;
        self.frame = 0;
        self.next = active.then(|| now.checked_add(PERIOD)).flatten();
        true
    }

    /// Coalesces missed ticks without replaying frames or drifting the phase.
    /// Returns whether the frame index changed; the deadline may change even if false.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.next.is_none_or(|next| now < next) {
            return false;
        }
        let elapsed = now.duration_since(self.epoch).as_nanos();
        let period = PERIOD.as_nanos();
        let frame = (elapsed / period % self.frames.len() as u128) as usize;
        let changed = self.frame != frame;
        self.frame = frame;
        self.next = now.checked_add(PERIOD - Duration::from_nanos((elapsed % period) as u64));
        changed
    }

    /// Returns the current frame, or a space while inactive.
    pub fn glyph(&self) -> char {
        if self.active {
            self.frames[self.frame]
        } else {
            ' '
        }
    }

    /// Returns the next scheduled tick, or `None` when inactive or unschedulable.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES: &[char] = &['a', 'b', 'c', 'd'];

    #[test]
    fn inactive_indicators_never_schedule_repaints() {
        let now = Instant::now();
        let mut spinner = Spinner::new(now, FRAMES);
        assert_eq!(spinner.next_deadline(), None);
        assert_eq!(spinner.glyph(), ' ');
        assert!(!spinner.advance(now + Duration::from_secs(30)));
        spinner.set_active(true, now);
        spinner.advance(now + PERIOD);
        assert!(spinner.set_active(false, now + PERIOD));
        assert_eq!(spinner.next_deadline(), None);
        assert_eq!(spinner.glyph(), ' ');
        assert!(!spinner.advance(now + Duration::from_secs(60)));
    }

    #[test]
    fn late_ticks_coalesce_and_keep_the_original_phase() {
        let now = Instant::now();
        let mut spinner = Spinner::new(now, FRAMES);
        spinner.set_active(true, now);
        assert!(!spinner.advance(now + PERIOD - Duration::from_nanos(1)));
        assert!(spinner.advance(now + PERIOD));
        let glyph = spinner.glyph();
        let cycles = FRAMES.len() as u32 * 3 + 1;
        assert!(!spinner.advance(now + PERIOD * cycles + Duration::from_millis(30)));
        assert_eq!(spinner.glyph(), glyph);
        assert_eq!(spinner.next_deadline(), Some(now + PERIOD * (cycles + 1)));
        assert!(spinner.advance(now + PERIOD * (cycles + 1)));
        assert_ne!(spinner.glyph(), glyph);
    }

    #[test]
    fn reactivation_restarts_without_old_deadlines() {
        let now = Instant::now();
        let mut spinner = Spinner::new(now, FRAMES);
        spinner.set_active(true, now);
        let first = spinner.glyph();
        spinner.advance(now + PERIOD * 2);
        spinner.set_active(false, now + PERIOD * 2);
        let resumed = now + Duration::from_secs(5);
        spinner.set_active(true, resumed);
        assert_eq!(spinner.glyph(), first);
        assert_eq!(spinner.next_deadline(), Some(resumed + PERIOD));
        assert!(!spinner.set_active(true, resumed + Duration::from_millis(10)));
        assert_eq!(spinner.next_deadline(), Some(resumed + PERIOD));
    }
}
