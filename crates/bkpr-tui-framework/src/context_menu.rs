//! Anchored action menus with shared paint and hit geometry.
use crate::{
    Rect, Role, RoleSpan, SurfaceContent, Theme, UiSurface, overlay::Placement, text_width, window,
};
use bkpr_protocol::Size;

/// An available action and its trusted single-line presentation.
pub struct Item<T> {
    /// Caller-owned action value.
    pub action: T,
    /// Sanitized visible label.
    pub label: &'static str,
    /// Optional sanitized icon, omitted when space is tight.
    pub icon: &'static str,
}

/// A single-column menu. The caller owns overlay routing and action dispatch.
pub struct ContextMenu<T> {
    anchor: (u16, u16),
    width: u16,
    items: Vec<Item<T>>,
    selected: usize,
    pressed: Option<usize>,
}

impl<T> ContextMenu<T> {
    /// Start at the first item, with a preferred minimum outer width in cells.
    /// Empty menus have no placement.
    pub fn new(anchor: (u16, u16), min_width: u16, items: Vec<Item<T>>) -> Self {
        let width = items.iter().fold(min_width, |width, item| {
            let width_needed = 4
                + text_width(item.label)
                + if item.icon.is_empty() {
                    0
                } else {
                    text_width(item.icon) + 1
                };
            width.max(u16::try_from(width_needed).unwrap_or(u16::MAX))
        });
        Self {
            anchor,
            width,
            items,
            selected: 0,
            pressed: None,
        }
    }

    /// Fit the menu at its anchor, shifting inward near screen edges.
    /// Returns `None` when not all rows fit or there is no usable body.
    pub fn placement(&self, screen: Size) -> Option<Placement> {
        let height = u16::try_from(self.items.len()).ok()?.checked_add(2)?;
        if self.items.is_empty() || height > screen.rows || screen.cols < 4 {
            return None;
        }
        let width = self.width.min(screen.cols);
        Some(Placement::framed(
            Rect {
                x: self.anchor.0.min(screen.cols - width),
                y: self.anchor.1.min(screen.rows - height),
                width,
                height,
            },
            screen,
        ))
    }

    /// Move one row in the requested direction, wrapping at the ends.
    pub fn navigate(&mut self, forward: bool) {
        let len = self.items.len();
        if len != 0 {
            self.selected = (self.selected + if forward { 1 } else { len - 1 }) % len;
        }
    }

    /// The highlighted action, if the menu is nonempty.
    pub fn selected(&self) -> Option<&T> {
        self.items.get(self.selected).map(|item| &item.action)
    }

    /// Highlight the visible row under the pointer, preserving selection outside.
    /// Supply the content rectangle from the presented placement.
    pub fn hover(&mut self, content: Rect, x: u16, y: u16) {
        if let Some(row) = self.row_at(content, x, y) {
            self.selected = row;
        }
    }

    /// Record the primary press against the presented content bounds.
    pub fn press(&mut self, content: Rect, x: u16, y: u16) {
        self.pressed = self.row_at(content, x, y);
    }

    /// Activate only when a primary release lands on its original pressed row.
    pub fn release(&mut self, content: Rect, x: u16, y: u16) -> Option<&T> {
        let row = self.pressed.take()?;
        (self.row_at(content, x, y) == Some(row)).then(|| &self.items[row].action)
    }

    fn row_at(&self, content: Rect, x: u16, y: u16) -> Option<usize> {
        content
            .contains(x, y)
            .then(|| usize::from(y - content.y))
            .filter(|&row| row < self.items.len())
    }

    /// Paint rounded fallback borders and full-width selection. Pass the outer
    /// size of this menu's placement, including the current cell pixel size.
    pub fn paint(&self, surface: &mut UiSurface, size: Size, theme: &Theme) {
        let body = window::content(size);
        let row = |index: usize| Rect {
            y: body.y + index as u16,
            height: 1,
            ..body
        };
        let mut lines = window::border_rows(size, "", window::Border::Rounded);
        lines.extend(
            self.items
                .iter()
                .take(usize::from(body.height))
                .enumerate()
                .map(|(index, item)| {
                    let label = if !item.icon.is_empty()
                        && text_width(item.label) + text_width(item.icon) + 2
                            <= usize::from(body.width)
                    {
                        format!(" {} {}", item.icon, item.label)
                    } else {
                        item.label.to_owned()
                    };
                    (row(index), label, false)
                }),
        );
        surface.paint(size, lines, None);
        let [panel, top, bottom, left, right] = window::theme_spans(size);
        surface.apply_theme(
            theme,
            Role::Panel,
            &[
                panel,
                top,
                bottom,
                left,
                right,
                RoleSpan {
                    rect: row(self.selected),
                    role: Role::Selected,
                },
            ],
        );
    }
}

/// Cell projection for a painted menu. Ready graphical chrome replaces the
/// outer rim; otherwise the rounded Unicode border stays visible.
pub fn content(size: Size, decorated: bool) -> SurfaceContent {
    if decorated {
        SurfaceContent::CellsClipped(window::content(size))
    } else {
        SurfaceContent::All
    }
}
