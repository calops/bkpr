//! Typed dropdown choices and anchored cell geometry; callers own input and commit policy.
use crate::{Rect, text_width, window};

/// An ordered, borrowed set of choice values and sanitized, single-line labels.
pub struct Dropdown<'a, T> {
    choices: &'a [(T, &'a str)],
}

impl<'a, T: Copy + Eq> Dropdown<'a, T> {
    /// Borrow choices in display and quick-cycle order.
    pub const fn new(choices: &'a [(T, &'a str)]) -> Self {
        Self { choices }
    }

    /// Position of a value in the choice list, if present.
    pub fn index(&self, value: T) -> Option<usize> {
        self.choices.iter().position(|(choice, _)| *choice == value)
    }

    /// Typed value at a choice position.
    pub fn value(&self, index: usize) -> Option<T> {
        self.choices.get(index).map(|(value, _)| *value)
    }

    /// Move a highlighted choice without wrapping or committing; empty lists return zero.
    pub fn move_by(&self, index: usize, delta: isize) -> usize {
        index
            .saturating_add_signed(delta)
            .min(self.choices.len().saturating_sub(1))
    }

    /// Find the previous or next value with wraparound.
    pub fn cycle(&self, value: T, previous: bool) -> Option<T> {
        let index = self.index(value)?;
        let next = if previous {
            index.checked_sub(1).unwrap_or(self.choices.len() - 1)
        } else {
            (index + 1) % self.choices.len()
        };
        self.value(next)
    }

    /// Present the current value with a trailing disclosure indicator.
    pub fn trigger(&self, rect: Rect, value: T, open: bool) -> window::TextLine {
        let indicator = if open { '▴' } else { '▾' };
        if rect.width == 0 {
            return (rect, String::new(), false);
        }
        let mut line = self.index(value).map_or_else(String::new, |index| {
            let reserved = if rect.width > 2 { 2 } else { 1 };
            window::fit(self.choices[index].1, rect.width - reserved)
        });
        let padding = usize::from(rect.width) - text_width(&line) - 1;
        line.reserve_exact(padding + indicator.len_utf8());
        line.extend(std::iter::repeat_n(' ', padding));
        line.push(indicator);
        (rect, line, false)
    }

    /// Fit choices below the trigger when possible, or above it when that shows more rows.
    /// With only the trigger row available, choices scroll in that row instead.
    pub fn layout(&self, trigger: Rect, viewport: Rect, active: usize) -> Option<Layout> {
        if self.choices.is_empty() || trigger.width == 0 || !viewport.contains(trigger.x, trigger.y)
        {
            return None;
        }
        let count = u16::try_from(self.choices.len()).unwrap_or(u16::MAX);
        let bottom = viewport.y.saturating_add(viewport.height);
        let below = bottom.saturating_sub(trigger.y + 1).min(count);
        let above = (trigger.y - viewport.y).min(count);
        let (y, height) = if below >= above && below != 0 {
            (trigger.y + 1, below)
        } else if above != 0 {
            (trigger.y - above, above)
        } else {
            (trigger.y, 1)
        };
        Some(Layout {
            bounds: Rect {
                x: trigger.x,
                y,
                width: trigger
                    .width
                    .min(viewport.x.saturating_add(viewport.width) - trigger.x),
                height,
            },
            first: active
                .min(self.choices.len() - 1)
                .saturating_sub(usize::from(height) - 1),
        })
    }

    /// Present a visible choice, marking the committed value when space permits.
    pub fn option(&self, layout: Layout, index: usize, value: T) -> Option<window::TextLine> {
        let rect = layout.row(index)?;
        let (choice, label) = self.choices.get(index)?;
        let prefix = if rect.width < 3 {
            ""
        } else if *choice == value {
            "• "
        } else {
            "  "
        };
        let mut text = String::with_capacity(prefix.len() + label.len());
        text.push_str(prefix);
        text.push_str(label);
        Some((rect, text, false))
    }
}

/// The visible part of an open dropdown, in surface-local coordinates.
#[derive(Clone, Copy)]
pub struct Layout {
    bounds: Rect,
    first: usize,
}

impl Layout {
    /// Entire list rectangle, clipped to the caller's viewport.
    pub fn bounds(self) -> Rect {
        self.bounds
    }

    /// Choice indices in display order.
    pub fn indices(self) -> std::ops::Range<usize> {
        self.first..self.first + usize::from(self.bounds.height)
    }

    fn row(self, index: usize) -> Option<Rect> {
        self.indices().contains(&index).then(|| Rect {
            y: self.bounds.y + (index - self.first) as u16,
            height: 1,
            ..self.bounds
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Choice {
        All,
        Some,
        None,
    }

    const OPTIONS: Dropdown<'_, Choice> = Dropdown::new(&[
        (Choice::All, "全て"),
        (Choice::Some, "Mentions"),
        (Choice::None, "None"),
    ]);

    const VIEWPORT: Rect = Rect {
        x: 2,
        y: 4,
        width: 12,
        height: 6,
    };
    const TRIGGER: Rect = Rect {
        x: 7,
        y: 5,
        width: 9,
        height: 1,
    };

    #[test]
    fn quick_cycle_wraps_but_the_active_choice_clamps() {
        assert_eq!(OPTIONS.cycle(Choice::All, true), Some(Choice::None));
        assert_eq!(OPTIONS.cycle(Choice::None, false), Some(Choice::All));
        let active = OPTIONS.index(Choice::Some).unwrap();
        assert_eq!(
            OPTIONS.value(OPTIONS.move_by(active, -2)),
            Some(Choice::All)
        );
        assert_eq!(
            OPTIONS.value(OPTIONS.move_by(active, 2)),
            Some(Choice::None)
        );
        assert_eq!(OPTIONS.value(3), None);
    }

    #[test]
    fn choice_lines_fit_below_or_above_the_trigger_without_changing_order() {
        let below = OPTIONS.layout(TRIGGER, VIEWPORT, 1).unwrap();
        assert_eq!(
            below.bounds(),
            Rect {
                x: 7,
                y: 6,
                width: 7,
                height: 3,
            }
        );
        assert_eq!(below.indices(), 0..3);
        let (rect, label, _) = OPTIONS.option(below, 1, Choice::Some).unwrap();
        assert_eq!(
            rect,
            Rect {
                y: 7,
                height: 1,
                ..below.bounds()
            }
        );
        assert_eq!(label, "• Mentions");
        assert!(OPTIONS.option(below, 3, Choice::Some).is_none());

        let above = OPTIONS
            .layout(Rect { y: 9, ..TRIGGER }, VIEWPORT, 1)
            .unwrap();
        assert_eq!(above.bounds(), below.bounds());
        assert_eq!(above.indices(), below.indices());
    }

    #[test]
    fn one_row_menu_reveals_the_active_choice_and_trigger_keeps_disclosure() {
        let trigger = Rect {
            width: 3,
            ..TRIGGER
        };
        let viewport = Rect {
            y: 5,
            height: 1,
            ..VIEWPORT
        };
        let layout = OPTIONS.layout(trigger, viewport, 2).unwrap();
        assert_eq!(layout.bounds(), trigger);
        assert_eq!(layout.indices(), 2..3);
        assert_eq!(OPTIONS.option(layout, 2, Choice::Some).unwrap().0, trigger);
        assert_eq!(OPTIONS.trigger(trigger, Choice::All, false).1, "  ▾");
        assert_eq!(
            OPTIONS
                .trigger(
                    Rect {
                        width: 5,
                        ..trigger
                    },
                    Choice::All,
                    true
                )
                .1,
            "全  ▴"
        );
    }
}
