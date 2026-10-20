use std::time::{Duration, Instant};
use zellij_tile::prelude::set_timeout;

const FRAME_INTERVAL: Duration = Duration::from_millis(200);
const FRAME_COUNT: usize = 12;

#[derive(Default)]
pub struct WorkingClock {
    frame: usize,
    deadline: Option<Instant>,
}

impl WorkingClock {
    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn schedule(&mut self, now: Instant, active: bool) {
        if !active {
            self.deadline = None;
        } else if self.deadline.is_none() {
            self.deadline = Some(now + FRAME_INTERVAL);
            set_timeout(FRAME_INTERVAL.as_secs_f64());
        }
    }

    pub fn tick(&mut self, now: Instant, active: bool) -> bool {
        if !active {
            self.deadline = None;
            return false;
        }
        let Some(deadline) = self.deadline else {
            return false;
        };
        // Timer events are shared with unrelated host timers and cannot be cancelled.
        if now < deadline {
            return false;
        }
        self.frame = (self.frame + 1) % FRAME_COUNT;
        self.deadline = None;
        self.schedule(now, true);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_one_frame_per_deadline_and_wraps_clockwise() {
        let start = Instant::now();
        let mut clock = WorkingClock::default();
        assert_eq!(clock.frame(), 0);
        clock.schedule(start, true);
        for step in 1..=FRAME_COUNT {
            assert!(clock.tick(start + FRAME_INTERVAL * step as u32, true));
            assert_eq!(clock.frame(), step % FRAME_COUNT);
        }
        assert_eq!(clock.deadline, Some(start + FRAME_INTERVAL * 13));
    }

    #[test]
    fn repeated_schedules_and_duplicate_or_early_timers_do_not_advance() {
        let start = Instant::now();
        let mut clock = WorkingClock::default();
        assert!(!clock.tick(start, true));
        clock.schedule(start, true);
        let deadline = start + FRAME_INTERVAL;
        clock.schedule(start + Duration::from_millis(50), true);
        assert_eq!(clock.deadline, Some(deadline));
        assert!(!clock.tick(deadline - Duration::from_millis(1), true));
        assert_eq!(clock.frame(), 0);
        assert!(clock.tick(deadline, true));
        assert!(!clock.tick(deadline, true));
        assert_eq!(clock.frame(), 1);
        assert_eq!(clock.deadline, Some(deadline + FRAME_INTERVAL));
    }

    #[test]
    fn late_timers_advance_once_without_catchup_chains() {
        let start = Instant::now();
        let mut clock = WorkingClock::default();
        clock.schedule(start, true);
        let late = start + Duration::from_secs(2);
        assert!(clock.tick(late, true));
        assert_eq!(clock.frame(), 1);
        assert_eq!(clock.deadline, Some(late + FRAME_INTERVAL));
        assert!(!clock.tick(late, true));
    }

    #[test]
    fn stopping_and_restarting_ignores_old_timers_and_keeps_the_frame() {
        let start = Instant::now();
        let mut clock = WorkingClock::default();
        clock.schedule(start, true);
        assert!(clock.tick(start + FRAME_INTERVAL, true));
        clock.schedule(start + FRAME_INTERVAL, false);
        assert_eq!(clock.deadline, None);
        assert!(!clock.tick(start + FRAME_INTERVAL * 2, true));
        let restart = start + FRAME_INTERVAL * 2 + Duration::from_millis(50);
        clock.schedule(restart, true);
        assert!(!clock.tick(start + FRAME_INTERVAL * 3, true));
        assert_eq!(clock.frame(), 1);
        assert!(clock.tick(restart + FRAME_INTERVAL, true));
        assert_eq!(clock.frame(), 2);
        assert!(!clock.tick(restart + FRAME_INTERVAL * 2, false));
        assert_eq!(clock.deadline, None);
        assert!(!clock.tick(restart + FRAME_INTERVAL * 3, true));
        assert_eq!(clock.frame(), 2);
    }
}
