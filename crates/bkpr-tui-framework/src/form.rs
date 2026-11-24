//! Shared text geometry for single-row form faces.

use crate::{Rect, TextInput, window};
use crate::{
    button::place_row_face,
    decoration::{DecorationKind, DecorationStyle},
    graphics::SvgGraphics,
};
use bkpr_protocol::Frame;

/// Two below-text faces for a single-row recessed field. Focus changes the
/// material, but pointer presses never move the face or reverse its bevel.
pub struct RecessedFields([DecorationStyle; 2]);

impl RecessedFields {
    /// Set the surrounding matte and the idle/focused opening colors using a
    /// native pane material as the source of the inset shadow.
    pub fn new(
        pane: DecorationStyle,
        matte: [u8; 3],
        colors: [[u8; 3]; 2],
        radius_px: u32,
        cell_width: u32,
    ) -> Self {
        Self(colors.map(|background| DecorationStyle {
            kind: DecorationKind::RecessedField,
            inset_px: [0; 2],
            outset_px: [0; 2],
            radius_px,
            background,
            surface: matte,
            sigma_milli: (cell_width * 150).max(750),
            offset_milli: [0, 650],
            shadow_opacity_milli: 540,
            ..pane
        }))
    }

    /// Materials to request on the existing graphics worker.
    pub fn styles(&self) -> &[DecorationStyle; 2] {
        &self.0
    }

    /// Place a field face inside its hit rectangle, reserving 64 placement IDs.
    /// Returns false when graphics are unavailable or geometry is too small.
    pub fn place(
        &self,
        graphics: &SvgGraphics,
        frame: &mut Frame,
        rect: Rect,
        focused: bool,
        base: u32,
    ) -> bool {
        place_row_face(graphics, frame, rect, self.0[usize::from(focused)], base)
    }
}

pub(crate) fn edges(rect: Rect, graphical: bool) -> Option<[char; 2]> {
    (rect.width >= 3).then_some(if graphical { [' '; 2] } else { ['[', ']'] })
}

/// Present a one-row input and return the caret's surface-local column.
/// A placed graphical face replaces the brackets without changing text or caret
/// bounds. Rows below four columns have no brackets, leaving room for text
/// beside the caret where possible. Empty and non-row bounds have no caret.
pub fn input_line(
    rect: Rect,
    input: &TextInput,
    graphical: bool,
) -> (Vec<window::TextLine>, Option<u16>) {
    if rect.height != 1 || rect.width == 0 {
        return (Vec::new(), None);
    }
    let edges = if rect.width >= 4 {
        edges(rect, graphical)
    } else {
        None
    };
    let inner = rect.inset_horizontal(u16::from(edges.is_some()));
    let (text, caret) = input.visible(usize::from(inner.width));
    let caret = Some(inner.x.saturating_add(caret as u16));
    let lines = if let Some([left, right]) = edges {
        let edge_text = |edge: char| {
            if graphical {
                String::new()
            } else {
                edge.to_string()
            }
        };
        let left = (Rect { width: 1, ..rect }, edge_text(left), false);
        let right = (
            Rect {
                x: rect.x.saturating_add(rect.width - 1),
                width: 1,
                ..rect
            },
            edge_text(right),
            false,
        );
        vec![left, (inner, text, false), right]
    } else {
        vec![(inner, text, false)]
    };
    (lines, caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_and_caret_keep_their_columns_with_a_graphical_face() {
        let rect = Rect {
            x: 7,
            y: 3,
            width: 6,
            height: 1,
        };
        let input = TextInput::new("a👩‍💻".into(), 64);
        let (plain, plain_caret) = input_line(rect, &input, false);
        let (fancy, fancy_caret) = input_line(rect, &input, true);
        assert!(
            plain
                .iter()
                .map(|line| line.0)
                .eq(fancy.iter().map(|line| line.0))
        );
        assert_eq!(plain[1], (rect.inset_horizontal(1), "a👩‍💻".into(), false));
        assert_eq!(fancy[1], plain[1]);
        assert_eq!(plain_caret, Some(11));
        assert_eq!(fancy_caret, plain_caret);
    }

    #[test]
    fn narrow_input_leaves_room_for_the_caret_in_both_modes() {
        let rect = Rect {
            x: 7,
            y: 3,
            width: 3,
            height: 1,
        };
        let input = TextInput::new("a界".into(), 64);
        let (plain, caret) = input_line(rect, &input, false);
        assert_eq!(plain, [(rect, String::from("界"), false)]);
        assert_eq!(caret, Some(rect.x + 2));
        assert_eq!(input_line(rect, &input, true), (plain, caret));
        assert_eq!(
            input_line(Rect { width: 0, ..rect }, &input, true),
            (Vec::new(), None)
        );
    }
}
