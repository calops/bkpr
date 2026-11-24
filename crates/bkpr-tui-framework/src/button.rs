//! Single-row controls with stable cell geometry across text and graphics modes.
use crate::decoration::{DecorationGeometry, DecorationKind, DecorationStyle};
use crate::graphics::SvgGraphics;
use crate::{Rect, Role, Theme, clear, text, text_width, window};
use bkpr_protocol::Frame;

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
}

impl<'a> Button<'a> {
    /// Create a button from a sanitized single-line text label or icon.
    pub fn new(label: &'a str) -> Self {
        Self { label }
    }

    /// Preferred cell width including the two side cells shared by both modes.
    pub fn width(&self) -> u16 {
        u16::try_from(text_width(self.label).saturating_add(2)).unwrap_or(u16::MAX)
    }

    /// Paint one reserved row. Use a graphical face only when it was placed.
    /// Narrow bounds omit brackets before clipping the label. The cursor stays unchanged.
    pub fn paint(
        &self,
        frame: &mut Frame,
        mut rect: Rect,
        theme: &Theme,
        state: State,
        graphical: bool,
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
        let measured = text_width(self.label);
        let clipped;
        let (label, used) = if measured <= usize::from(budget) {
            (self.label, measured as u16)
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
        let mut style = theme
            .style(if graphical { Role::Panel } else { Role::Accent })
            .clone();
        style.background = background;
        style.background_source = background_source;
        style.inverse = !graphical && state != State::Idle;
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

/// The three reusable, shadowless face materials for one host cell size and palette.
#[derive(Clone, Copy)]
pub struct Materials([DecorationStyle; 3]);

impl Materials {
    /// Use caller-selected face colors in idle, hovered, pressed order. Radius and
    /// bevel thickness are physical pixels, independent of label size.
    pub fn new(
        faces: [[u8; 3]; 3],
        radius_px: u32,
        bevel_milli: u32,
        highlight: u16,
        shade: u16,
    ) -> Self {
        Self(std::array::from_fn(|index| DecorationStyle {
            kind: if index == 2 {
                DecorationKind::ButtonPressed
            } else {
                DecorationKind::ButtonRaised
            },
            background: faces[index],
            surface: faces[index],
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
        }))
    }

    /// Materials to request together through the existing graphics worker.
    pub fn styles(&self) -> &[DecorationStyle; 3] {
        &self.0
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
            self.0[state as usize],
            base,
            &[],
            DecorationGeometry {
                bounds_px: [left + cw / 4, top + 1, right - cw / 4, bottom - 1],
                clip_px,
            },
        )
    }
}
