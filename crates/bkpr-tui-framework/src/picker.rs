//! Palette-independent section geometry for floating search pickers.

use crate::{Rect, RoleSpan, window};
use bkpr_protocol::{Frame, Size};

/// Shared search, results and optional details card geometry, in overlay cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerLayout {
    /// Search heading row.
    pub title: Rect,
    /// Search editor row.
    pub input: Rect,
    /// Painted and interactive result rows.
    pub results: Rect,
    /// Details content, empty for a two-card picker.
    pub details: Rect,
    cards: [(Rect, Rect); 3],
    count: usize,
}

impl PickerLayout {
    /// Build floating cards, or return `None` when compact content is necessary.
    pub fn new(size: Size, details: bool) -> Option<Self> {
        if size.cols < 40 || size.rows < if details { 14 } else { 8 } {
            return None;
        }
        let search = Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: 4,
        };
        let body = Rect {
            y: 4,
            height: size.rows - 4,
            ..search
        };
        let (results, detail) = if !details {
            (body, Rect::default())
        } else if size.cols >= 84 {
            let split = (u32::from(size.cols) * 2 / 5) as u16;
            (
                Rect {
                    width: split,
                    ..body
                },
                Rect {
                    x: split,
                    width: size.cols - split,
                    ..body
                },
            )
        } else {
            let split = ((u32::from(body.height) * 2 / 5) as u16).max(4);
            (
                Rect {
                    height: split,
                    ..body
                },
                Rect {
                    y: 4 + split,
                    height: body.height - split,
                    ..body
                },
            )
        };
        let inset = |rect: Rect| Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width - 4,
            height: rect.height - 2,
        };
        let title = Rect {
            x: 2,
            y: 1,
            width: size.cols - 4,
            height: 1,
        };
        let result_content = inset(results);
        let detail_content = if details {
            inset(detail)
        } else {
            Rect::default()
        };
        Some(Self {
            title,
            input: Rect { y: 2, ..title },
            results: result_content,
            details: detail_content,
            cards: [
                (search, Rect { height: 2, ..title }),
                (results, result_content),
                (detail, detail_content),
            ],
            count: if details { 3 } else { 2 },
        })
    }

    /// Outer and content rectangles in Search, Results, Details order.
    pub fn cards(&self) -> &[(Rect, Rect)] {
        &self.cards[..self.count]
    }

    /// Rounded Unicode border rows for these exact outer rectangles.
    pub fn border_rows(&self) -> Vec<window::TextLine> {
        self.cards()
            .iter()
            .flat_map(|(outer, _)| {
                window::border_rows(
                    Size {
                        cols: outer.width,
                        rows: outer.height,
                        cell_width: 0,
                        cell_height: 0,
                    },
                    "",
                    window::Border::Rounded,
                )
                .into_iter()
                .map(|(mut rect, text, selected)| {
                    rect.x += outer.x;
                    rect.y += outer.y;
                    (rect, text, selected)
                })
            })
            .collect()
    }

    /// Border-only semantic roles, leaving content styling to the caller.
    pub fn border_spans(&self) -> Vec<RoleSpan> {
        self.cards()
            .iter()
            .flat_map(|(outer, _)| {
                window::theme_spans(Size {
                    cols: outer.width,
                    rows: outer.height,
                    cell_width: 0,
                    cell_height: 0,
                })
                .into_iter()
                .skip(1)
                .map(|mut span| {
                    span.rect.x += outer.x;
                    span.rect.y += outer.y;
                    span
                })
            })
            .collect()
    }

    /// Paint rounded borders without disturbing content, resources or the caret.
    pub fn decorate(&self, frame: &mut Frame) {
        for (rect, label, selected) in self.border_rows() {
            crate::text(frame, rect.x, rect.y, &label, selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_and_borders_share_bounded_geometry_at_responsive_boundaries() {
        for cols in [40, 83, 84, 120] {
            for details in [false, true] {
                let size = Size {
                    cols,
                    rows: 14,
                    cell_width: 9,
                    cell_height: 18,
                };
                let layout = PickerLayout::new(size, details).unwrap();
                assert_eq!(layout.cards().len(), if details { 3 } else { 2 });
                for (outer, content) in layout.cards() {
                    assert!(content.x > outer.x && content.y > outer.y);
                    assert!(content.x + content.width < outer.x + outer.width);
                    assert!(content.y + content.height < outer.y + outer.height);
                    let rows = layout.border_rows();
                    assert!(rows.iter().any(|(rect, text, _)| rect.x == outer.x
                        && rect.y == outer.y
                        && text.starts_with('╭')
                        && text.ends_with('╮')));
                    assert!(
                        rows.iter()
                            .all(|(rect, _, _)| rect.x + rect.width <= cols && rect.y < size.rows)
                    );
                }
                if details {
                    assert_eq!(layout.results.y == layout.details.y, cols >= 84);
                }
            }
        }
        assert!(
            PickerLayout::new(
                Size {
                    cols: 39,
                    rows: 30,
                    cell_width: 9,
                    cell_height: 18,
                },
                true
            )
            .is_none()
        );
        assert!(
            PickerLayout::new(
                Size {
                    cols: 80,
                    rows: 13,
                    cell_width: 9,
                    cell_height: 18,
                },
                true
            )
            .is_none()
        );
    }
}
