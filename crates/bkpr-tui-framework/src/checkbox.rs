//! Single-row checkbox presentation; the caller owns the checked value and activation.
use crate::{Rect, window::TextLine};

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
        let mark = match (rect.width >= 3, self.checked) {
            (true, true) => "[x]",
            (true, false) => "[ ]",
            (false, true) => "x",
            (false, false) => "·",
        };
        let label = if rect.width > 4 && !self.label.is_empty() {
            let mut label = String::with_capacity(mark.len() + 1 + self.label.len());
            label.push_str(mark);
            label.push(' ');
            label.push_str(self.label);
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
}
