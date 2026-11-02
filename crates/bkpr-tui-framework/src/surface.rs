//! Retained trusted UI text and divider scenes; physical output remains with SceneRenderer.
use crate::{Rect, blank, clear, text, window};
use bkpr_protocol::{Cursor, Frame, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divider {
    pub rect: Rect,
    pub vertical: bool,
}

/// Text changes clear old bounds and repaint widget lines, not a fresh screen.
/// Decorations contain geometry only, never application layout or pane identities.
pub struct UiSurface {
    pub frame: Frame,
    lines: Vec<window::TextLine>,
    dividers: Vec<Divider>,
}
impl UiSurface {
    pub fn new(size: Size) -> Self {
        Self {
            frame: blank(size),
            lines: Vec::new(),
            dividers: Vec::new(),
        }
    }
    pub fn paint(&mut self, size: Size, lines: Vec<window::TextLine>, cursor: Option<Cursor>) {
        self.paint_parts(size, lines, Vec::new(), cursor);
    }
    pub fn paint_parts(
        &mut self,
        size: Size,
        lines: Vec<window::TextLine>,
        dividers: Vec<Divider>,
        cursor: Option<Cursor>,
    ) {
        if self.frame.size != size {
            self.frame = blank(size);
            self.lines.clear();
            self.dividers.clear();
        }
        if self.lines != lines || self.dividers != dividers {
            for (rect, _, _) in self.lines.iter().chain(&lines) {
                clear(&mut self.frame, *rect);
            }
            for divider in self.dividers.iter().chain(&dividers) {
                clear(&mut self.frame, divider.rect);
            }
            for (rect, label, selected) in &lines {
                let width = rect.width.min(size.cols.saturating_sub(rect.x));
                text(
                    &mut self.frame,
                    rect.x,
                    rect.y,
                    &window::fit(label, width),
                    *selected,
                );
            }
            for divider in &dividers {
                let rect = divider.rect;
                let width = rect.width.min(size.cols.saturating_sub(rect.x));
                let label = if divider.vertical { "│" } else { "─" }.repeat(usize::from(width));
                for y in rect.y..rect.y.saturating_add(rect.height).min(size.rows) {
                    text(&mut self.frame, rect.x, y, &label, false);
                    for cell in self.frame.rows[usize::from(y)]
                        .cells
                        .iter_mut()
                        .skip(usize::from(rect.x))
                        .take(usize::from(width))
                    {
                        cell.style.faint = true;
                    }
                }
            }
            self.dividers = dividers;
            self.lines = lines;
        }
        self.frame.cursor.visible = false;
        if let Some(cursor) = cursor {
            self.frame.cursor = cursor;
        }
    }
}
