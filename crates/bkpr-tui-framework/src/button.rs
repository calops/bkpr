//! Single-row controls with stable cell geometry across text and graphics modes.
use crate::decoration::{DecorationGeometry, DecorationKind, DecorationStyle};
use crate::graphics::SvgGraphics;
use crate::{Rect, clear, text, text_width, window};
use bkpr_protocol::Frame;
use bkpr_protocol::{ColorSource, Style};
use palette::color_difference::Wcag21RelativeContrast;

/// Visual state of a button; the owning surface decides whether to activate it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// No pointer inside the control.
    #[default]
    Idle,
    /// Pointer inside, without a matching captured press.
    Hover,
    /// Pointer inside after a press captured by this control.
    Pressed,
    /// Unavailable action, using an optional muted face or a dim text fallback.
    Disabled,
}

impl State {
    /// Resolve visual feedback using the current hitbox and the owner's capture.
    pub fn at(rect: Rect, pointer: Option<(u16, u16)>, armed: bool) -> Self {
        if !pointer.is_some_and(|(x, y)| rect.contains(x, y)) {
            Self::Idle
        } else if armed {
            Self::Pressed
        } else {
            Self::Hover
        }
    }
}

/// A borrowed, single-line button label. It doesn't own input or the cursor.
pub struct Button<'a> {
    label: &'a str,
    wide_icon: bool,
}

impl<'a> Button<'a> {
    /// Create a button from a sanitized single-line text label or icon.
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            wide_icon: false,
        }
    }

    /// Create a button with a single icon that needs two cells even if its Unicode width is one.
    pub fn wide_icon(label: &'a str) -> Self {
        Self {
            label,
            wide_icon: true,
        }
    }

    fn label_width(&self) -> usize {
        text_width(self.label).max(if self.wide_icon { 2 } else { 0 })
    }

    /// Preferred cell width including the two side cells shared by both modes.
    pub fn width(&self) -> u16 {
        u16::try_from(self.label_width().saturating_add(2)).unwrap_or(u16::MAX)
    }

    /// Paint one reserved row. Use a graphical face only when it was placed.
    /// Bounds below three cells omit the brackets; wider bounds reserve both side cells.
    /// A wide icon is omitted if its full slot doesn't fit.
    /// The cursor stays unchanged.
    pub fn paint(&self, frame: &mut Frame, rect: Rect, ink: &Style, state: State, graphical: bool) {
        self.paint_inner(frame, rect, ink, state, graphical, true);
    }

    /// Paint the shared button geometry with caller-supplied ink in both modes.
    /// Use this when the graphical face is independent of the text color.
    pub fn paint_with_ink(
        &self,
        frame: &mut Frame,
        rect: Rect,
        ink: &Style,
        state: State,
        graphical: bool,
    ) {
        self.paint_inner(frame, rect, ink, state, graphical, false);
    }

    fn paint_inner(
        &self,
        frame: &mut Frame,
        mut rect: Rect,
        ink: &Style,
        state: State,
        graphical: bool,
        contrast: bool,
    ) {
        rect.width = rect.width.min(frame.size.cols.saturating_sub(rect.x));
        if rect.width == 0 || rect.height != 1 {
            return;
        }
        let Some(cell) = frame
            .rows
            .get(usize::from(rect.y))
            .and_then(|row| row.cells.get(usize::from(rect.x)))
        else {
            return;
        };
        let background = cell.style.background;
        let background_source = cell.style.background_source;
        clear(frame, rect);
        let framed = rect.width >= 3;
        let budget = rect.width - if framed { 2 } else { 0 };
        let measured = self.label_width();
        let clipped;
        let (label, used) = if measured <= usize::from(budget) {
            (self.label, measured as u16)
        } else if self.wide_icon {
            ("", 0)
        } else {
            clipped = window::fit(self.label, budget);
            (clipped.as_str(), text_width(&clipped) as u16)
        };
        let start = rect.x + (rect.width - used) / 2;
        if framed && !graphical {
            text(frame, rect.x, rect.y, "[", false);
            text(frame, rect.x + rect.width - 1, rect.y, "]", false);
        }
        text(frame, start, rect.y, label, false);
        let mut style = ink.clone();
        if graphical && contrast && state != State::Disabled {
            let face = crate::color::luminance(state.face(ink.foreground));
            let black = crate::color::luminance([0; 3]);
            let white = crate::color::luminance([255; 3]);
            style.foreground = if face.relative_contrast(black) >= face.relative_contrast(white) {
                [0; 3]
            } else {
                [255; 3]
            };
            style.foreground_source = ColorSource::Rgb(style.foreground);
        }
        style.background = background;
        style.background_source = background_source;
        style.inverse = !graphical && matches!(state, State::Hover | State::Pressed);
        style.faint |= !graphical && state == State::Disabled;
        if !graphical && state == State::Pressed {
            style.bold = true;
            style.underline = 1;
        }
        for cell in &mut frame.rows[usize::from(rect.y)].cells
            [usize::from(rect.x)..usize::from(rect.x + rect.width)]
        {
            cell.style.clone_from(&style);
        }
    }
}

impl State {
    fn face(self, ink: [u8; 3]) -> [u8; 3] {
        match self {
            Self::Idle | Self::Disabled => ink,
            Self::Hover => {
                let lighter = ink.map(|c| c + (255 - c) / 8);
                if crate::color::luminance(lighter).relative_contrast(crate::color::luminance(ink))
                    < 1.1
                {
                    ink.map(|c| c - c / 8)
                } else {
                    lighter
                }
            }
            Self::Pressed => ink.map(|c| c - c / 10),
        }
    }
}

/// Reusable, shadowless face materials for one host cell size and palette.
#[derive(Clone, Copy)]
pub struct Materials {
    enabled: [DecorationStyle; 3],
    disabled: Option<DecorationStyle>,
}

impl Materials {
    /// Use the same RGB accent supplied as text ink in plain mode. The framework
    /// derives the three faces. Radius and bevel thickness use physical pixels.
    pub fn new(ink: [u8; 3], radius_px: u32, bevel_milli: u32, highlight: u16, shade: u16) -> Self {
        let enabled = [State::Idle, State::Hover, State::Pressed].map(|state| {
            let face = state.face(ink);
            DecorationStyle {
                kind: if state == State::Pressed {
                    DecorationKind::ButtonPressed
                } else {
                    DecorationKind::ButtonRaised
                },
                background: face,
                surface: face,
                foreground: [255; 3],
                inset_px: [0; 2],
                outset_px: [0; 2],
                radius_px,
                sigma_milli: 1,
                offset_milli: [0; 2],
                shadow_opacity_milli: 0,
                lip_width_milli: bevel_milli,
                lip_highlight_milli: highlight.min(1000),
                lip_shade_milli: shade.min(1000),
            }
        });
        Self {
            enabled,
            disabled: None,
        }
    }

    /// Add a muted, non-interactive face using the same radius and bounds as
    /// the enabled faces. Supply its fill color independently of the text ink.
    pub fn with_disabled(mut self, ink: [u8; 3]) -> Self {
        let idle = self.enabled[State::Idle as usize];
        self.disabled = Some(DecorationStyle {
            background: ink,
            surface: ink,
            lip_highlight_milli: idle.lip_highlight_milli / 2,
            lip_shade_milli: idle.lip_shade_milli / 2,
            ..idle
        });
        self
    }

    /// Optional disabled face to request through the same graphics worker.
    pub fn disabled_style(&self) -> Option<DecorationStyle> {
        self.disabled
    }

    /// Enabled materials to request together through the existing graphics worker.
    pub fn styles(&self) -> &[DecorationStyle; 3] {
        &self.enabled
    }

    /// Append a prepared, below-text button face entirely inside its hitbox.
    /// Reserves 64 placement IDs from `base`. Returns false when unavailable.
    pub fn place(
        &self,
        graphics: &SvgGraphics,
        frame: &mut Frame,
        rect: Rect,
        state: State,
        base: u32,
    ) -> bool {
        let [cw, ch] = [frame.size.cell_width, frame.size.cell_height].map(i64::from);
        if !graphics.is_enabled()
            || rect.width < 3
            || rect.height != 1
            || !(4..=512).contains(&cw)
            || !(6..=512).contains(&ch)
        {
            return false;
        }
        let style = if state == State::Disabled {
            let Some(style) = self.disabled else {
                return false;
            };
            style
        } else {
            self.enabled[state as usize]
        };
        let [left, top, right, bottom] = [
            i64::from(rect.x) * cw,
            i64::from(rect.y) * ch,
            (i64::from(rect.x) + i64::from(rect.width)) * cw,
            (i64::from(rect.y) + 1) * ch,
        ];
        let clip_px = [left, top, right, bottom].map(|edge| edge as u32);
        graphics.decorate_window_with_geometry(
            frame,
            rect,
            style,
            base,
            &[],
            DecorationGeometry {
                bounds_px: [left + cw / 4, top + 1, right - cw / 4, bottom - 1],
                clip_px,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_face_keeps_the_caller_ink_and_label_column() {
        let size = bkpr_protocol::Size {
            cols: 9,
            rows: 1,
            cell_width: 8,
            cell_height: 16,
        };
        let rect = Rect {
            x: 2,
            y: 0,
            width: 5,
            height: 1,
        };
        let foreground = [110, 120, 130];
        let ink = Style {
            foreground,
            foreground_source: ColorSource::Rgb(foreground),
            ..crate::default_style()
        };
        let button = Button::new("OK");
        let mut frame = crate::blank(size);
        button.paint(&mut frame, rect, &ink, State::Disabled, false);
        assert_eq!(frame.rows[0].cells[2].text, "[");
        assert_eq!(frame.rows[0].cells[3].text, "O");
        assert!(frame.rows[0].cells[3].style.faint);

        button.paint(&mut frame, rect, &ink, State::Disabled, true);
        let cells = &frame.rows[0].cells;
        assert_eq!(cells[2].text, " ");
        assert_eq!(cells[3].text, "O");
        assert_eq!(
            cells[3].style.foreground_source,
            ColorSource::Rgb(foreground)
        );
        assert!(!cells[3].style.faint);
        assert!(!cells[3].style.inverse);
    }
}
