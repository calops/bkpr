//! Single-row checkbox presentation; the caller owns the checked value and activation.
use crate::{Rect, window, window::TextLine};

/// A checkbox with an optional sanitized, single-line label.
pub struct Checkbox<'a> {
    checked: bool,
    label: &'a str,
}

impl<'a> Checkbox<'a> {
    /// Borrow a label to place immediately after the check mark.
    pub fn new(checked: bool, label: &'a str) -> Self {
        Self { checked, label }
    }

    /// Cell bounds of the mark, for an optional independent role span.
    pub fn mark(rect: Rect) -> Rect {
        Rect {
            width: if rect.width >= 3 {
                3
            } else {
                rect.width.min(1)
            },
            ..rect
        }
    }

    /// Present a checkbox in a reserved row; uses a compact mark under three cells.
    pub fn line(&self, rect: Rect) -> TextLine {
        if rect.width == 0 {
            return (rect, String::new(), false);
        }
        self.marked_line(rect, false, 0)
    }

    /// Present the same mark and label columns with either text brackets or a
    /// placed graphical face. Narrow rows keep the compact text mark.
    pub fn line_with_face(&self, rect: Rect, graphical: bool) -> TextLine {
        if rect.width < 3 {
            return self.line(rect);
        }
        self.marked_line(rect, graphical, 1)
    }

    fn marked_line(&self, rect: Rect, graphical: bool, right_pad: u16) -> TextLine {
        let mark = if graphical {
            if self.checked { " ✓ " } else { " · " }
        } else {
            match (rect.width >= 3, self.checked) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "x",
                (false, false) => "·",
            }
        };
        let label = if rect.width > 4 + right_pad && !self.label.is_empty() {
            let mut label = String::with_capacity(mark.len() + 1 + self.label.len());
            label.push_str(mark);
            label.push(' ');
            if right_pad == 0 {
                label.push_str(self.label);
            } else {
                window::push_fit(&mut label, self.label, rect.width - 4 - right_pad);
            }
            label
        } else {
            mark.into()
        };
        (rect, label, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_and_label_share_stable_bounds_across_states() {
        let rect = Rect {
            x: 7,
            y: 3,
            width: 12,
            height: 1,
        };
        assert_eq!(Checkbox::mark(rect), Rect { width: 3, ..rect });
        assert_eq!(Checkbox::new(true, "Agent").line(rect).1, "[x] Agent");
        assert_eq!(Checkbox::new(false, "Agent").line(rect).1, "[ ] Agent");

        let compact = Rect { width: 2, ..rect };
        assert_eq!(Checkbox::mark(compact), Rect { width: 1, ..rect });
        assert_eq!(Checkbox::new(true, "Agent").line(compact).1, "x");
        assert_eq!(Checkbox::new(false, "Agent").line(compact).1, "·");
    }

    #[test]
    fn graphical_mark_and_plain_brackets_leave_the_same_label_budget() {
        let rect = Rect {
            x: 7,
            y: 3,
            width: 7,
            height: 1,
        };
        let checkbox = Checkbox::new(true, "界x");
        assert_eq!(
            checkbox.line_with_face(rect, false),
            (rect, "[x] 界".into(), false)
        );
        assert_eq!(
            checkbox.line_with_face(rect, true),
            (rect, " ✓  界".into(), false)
        );
        let compact = Rect { width: 2, ..rect };
        assert_eq!(
            checkbox.line_with_face(compact, true),
            checkbox.line(compact)
        );
    }
}
