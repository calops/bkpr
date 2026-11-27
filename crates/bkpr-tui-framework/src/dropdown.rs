//! Typed dropdown choices and anchored cell geometry; callers own input and commit policy.
use crate::{Rect, Role, RoleSpan, Theme, UiSurface, form, window};

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

    /// Present the value and disclosure indicator with fixed side padding.
    /// A placed graphical face replaces the brackets without moving either.
    pub fn trigger_with_face(
        &self,
        rect: Rect,
        value: T,
        open: bool,
        graphical: bool,
    ) -> window::TextLine {
        let edges = form::edges(rect, graphical);
        let indicator = if open { '▴' } else { '▾' };
        if rect.width == 0 {
            return (rect, String::new(), false);
        }
        let width = rect.width - 2 * u16::from(edges.is_some());
        let reserved = if width > 2 { 2 } else { 1 };
        let mut line = String::with_capacity(usize::from(rect.width - 1) + indicator.len_utf8());
        if let Some([left, _]) = edges {
            line.push(left);
        }
        let used = self.index(value).map_or(0, |index| {
            window::push_fit(&mut line, self.choices[index].1, width - reserved)
        });
        line.extend(std::iter::repeat_n(' ', usize::from(width - 1) - used));
        line.push(indicator);
        if let Some([_, right]) = edges {
            line.push(right);
        }
        (rect, line, false)
    }

    /// Fit a bordered popup below the trigger when possible, or above it when
    /// that shows more choices. In very tight views it can cover the trigger;
    /// omit the border if there is not room for both edges and a choice.
    pub fn layout(&self, trigger: Rect, viewport: Rect, active: usize) -> Option<Layout> {
        if self.choices.is_empty()
            || trigger.height != 1
            || trigger.width == 0
            || !viewport.contains(trigger.x, trigger.y)
        {
            return None;
        }
        let right = viewport.x.saturating_add(viewport.width);
        let mut x = trigger.x;
        if trigger.width >= 3 && right - viewport.x >= 3 {
            x = x.min(right - 3);
        }
        let width = trigger.width.min(right - x);
        let bottom = viewport.y.saturating_add(viewport.height);
        let below = bottom - trigger.y - 1;
        let above = trigger.y - viewport.y;
        let count = u16::try_from(self.choices.len()).unwrap_or(u16::MAX);
        let framed = width >= 3 && bottom - viewport.y >= 3;
        let budget = count.saturating_add(if framed { 2 } else { 0 });
        let below = below.min(budget);
        let above = above.min(budget);
        let minimum = if framed { 3 } else { 1 };
        let (y, height) = if below >= above && below >= minimum {
            (trigger.y + 1, below)
        } else if above >= minimum {
            (trigger.y - above, above)
        } else if framed {
            let height = budget.min(bottom - viewport.y);
            ((trigger.y + 1).min(bottom - height), height)
        } else {
            (trigger.y, 1)
        };
        let outer = Rect {
            x,
            y,
            width,
            height,
        };
        let rows = window::content_cells(width, height).height;
        Some(Layout {
            outer,
            first: active
                .min(self.choices.len() - 1)
                .saturating_sub(usize::from(rows) - 1),
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

    /// Paint the popup in its own outer-local surface, with no visible cursor.
    /// Seed the surface with the host cell metrics and compose it at
    /// [`Layout::outer`]. Only omit the Unicode rim when a graphical one is
    /// placed. A visible hovered choice takes precedence over the active choice.
    pub fn paint_popup(
        &self,
        surface: &mut UiSurface,
        layout: Layout,
        value: T,
        selection: (usize, Option<usize>),
        theme: &Theme,
        graphical: bool,
    ) {
        let (active, hover) = selection;
        let size = layout.outer.size(surface.frame.size);
        let local = Layout {
            outer: Rect {
                x: 0,
                y: 0,
                ..layout.outer
            },
            ..layout
        };
        let mut lines = if graphical {
            Vec::new()
        } else {
            window::border_rows(size, "", window::Border::Rounded)
        };
        lines.extend(
            local
                .indices()
                .filter_map(|index| self.option(local, index, value)),
        );
        surface.paint(size, lines, None);

        let selected = hover
            .and_then(|index| local.row(index))
            .or_else(|| local.row(active));
        let mut spans = window::theme_spans(size);
        spans[0] = RoleSpan {
            rect: selected.unwrap_or_default(),
            role: Role::Selected,
        };
        surface.apply_theme(
            theme,
            Role::Panel,
            if graphical { &spans[..1] } else { &spans },
        );
    }
}

/// The visible dropdown bounds and choice mapping, in the caller's coordinates.
#[derive(Clone, Copy)]
pub struct Layout {
    outer: Rect,
    first: usize,
}

impl Layout {
    /// Entire popup rectangle, including the border when it fits.
    pub fn outer(self) -> Rect {
        self.outer
    }

    /// Choice content rectangle; the surrounding border is not interactive.
    pub fn bounds(self) -> Rect {
        let body = window::content_cells(self.outer.width, self.outer.height);
        Rect {
            x: self.outer.x + body.x,
            y: self.outer.y + body.y,
            ..body
        }
    }

    /// Choice indices in display order.
    pub fn indices(self) -> std::ops::Range<usize> {
        self.first..self.first + usize::from(self.bounds().height)
    }

    /// Hit bounds of a visible choice, or `None` if it is scrolled out of view.
    pub fn row(self, index: usize) -> Option<Rect> {
        let bounds = self.bounds();
        let offset = index.checked_sub(self.first)?;
        (offset < usize::from(bounds.height)).then(|| Rect {
            y: bounds.y + offset as u16,
            height: 1,
            ..bounds
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
        height: 8,
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
            below.outer(),
            Rect {
                x: 7,
                y: 6,
                width: 7,
                height: 5,
            }
        );
        assert_eq!(
            below.bounds(),
            Rect {
                x: 8,
                y: 7,
                width: 5,
                height: 3,
            }
        );
        assert_eq!(below.indices(), 0..3);
        let (rect, label, _) = OPTIONS.option(below, 1, Choice::Some).unwrap();
        assert_eq!(
            rect,
            Rect {
                y: 8,
                height: 1,
                ..below.bounds()
            }
        );
        assert_eq!(label, "• Mentions");
        assert!(OPTIONS.option(below, 3, Choice::Some).is_none());

        let above = OPTIONS
            .layout(Rect { y: 11, ..TRIGGER }, VIEWPORT, 1)
            .unwrap();
        assert_eq!(above.outer(), below.outer());
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
        assert_eq!(layout.outer(), trigger);
        assert_eq!(layout.bounds(), trigger);
        assert_eq!(layout.indices(), 2..3);
        assert_eq!(OPTIONS.option(layout, 2, Choice::Some).unwrap().0, trigger);
        assert_eq!(
            OPTIONS
                .trigger_with_face(trigger, Choice::All, false, false)
                .1,
            "[▾]"
        );
        assert_eq!(
            OPTIONS
                .trigger_with_face(
                    Rect {
                        width: 6,
                        ..trigger
                    },
                    Choice::All,
                    true,
                    false,
                )
                .1,
            "[全 ▴]"
        );
    }

    #[test]
    fn short_viewport_keeps_a_border_and_the_active_row() {
        let viewport = Rect {
            height: 4,
            ..VIEWPORT
        };
        let layout = OPTIONS.layout(TRIGGER, viewport, 2).unwrap();
        assert_eq!(
            layout.outer(),
            Rect {
                x: 7,
                width: 7,
                ..viewport
            }
        );
        assert_eq!(layout.indices(), 1..3);
        assert_eq!(layout.row(2).unwrap().y, 6);
        assert!(!layout.bounds().contains(layout.outer().x, 6));
        assert!(layout.row(0).is_none());
    }

    #[test]
    fn trigger_switches_faces_without_moving_wide_text_or_disclosure() {
        let rect = Rect {
            width: 7,
            ..TRIGGER
        };
        assert_eq!(
            OPTIONS.trigger_with_face(rect, Choice::All, false, false),
            (rect, "[全  ▾]".into(), false)
        );
        assert_eq!(
            OPTIONS.trigger_with_face(rect, Choice::All, true, true),
            (rect, " 全  ▴ ".into(), false)
        );
    }

    #[test]
    fn popup_replaces_only_the_rim_in_outer_local_coordinates() {
        let layout = OPTIONS.layout(TRIGGER, VIEWPORT, 1).unwrap();
        let size = layout.outer().size(bkpr_protocol::Size {
            cols: VIEWPORT.width,
            rows: VIEWPORT.height,
            cell_width: 8,
            cell_height: 16,
        });
        let mut surface = UiSurface::new(size);
        let theme = Theme::default();
        OPTIONS.paint_popup(&mut surface, layout, Choice::Some, (1, None), &theme, false);
        assert_eq!(surface.frame.rows[0].cells[0].text, "╭");
        assert_eq!(surface.frame.rows[2].cells[1].text, "•");
        OPTIONS.paint_popup(&mut surface, layout, Choice::Some, (1, None), &theme, true);
        assert_eq!(surface.frame.rows[0].cells[0].text, " ");
        assert_eq!(surface.frame.rows[2].cells[1].text, "•");
        assert!(!surface.frame.cursor.visible);
    }
}
