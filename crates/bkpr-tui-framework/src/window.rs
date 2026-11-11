//! Terminal-default, cell-based decorations for existing local scene frames.
//! No terminal output or graphics capabilities are assumed here.
use crate::{Rect, Role, RoleSpan, text};
use bkpr_protocol::{Frame, Size, Style};
use unicode_width::UnicodeWidthChar;

/// Local text rectangle, content, and selection flag for surface painting.
pub type TextLine = (Rect, String, bool);

/// Single-cell corner style for a window frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Border {
    #[default]
    /// Square box-drawing corners.
    Plain,
    /// Rounded box-drawing corners.
    Rounded,
}

/// Strip terminal controls and directional formatting from metadata, not Unicode text.
pub fn clean(value: &str) -> String {
    value.chars().filter(|c| printable(*c)).collect()
}
fn printable(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Fits sanitized text using scalar cell widths; trailing combining marks stay
/// with their base. Multi-scalar grapheme clusters may be split at the bound.
pub fn fit(value: &str, width: u16) -> String {
    let mut result = String::new();
    let mut columns = 0;
    for ch in value.chars().filter(|c| printable(*c)) {
        let advance = UnicodeWidthChar::width(ch).unwrap_or(0);
        if columns + advance > usize::from(width) {
            break;
        }
        if advance != 0 || !result.is_empty() {
            result.push(ch);
        }
        columns += advance;
    }
    result
}

/// Centers a rectangle after constraining its dimensions to the screen.
pub fn centered(screen: Size, width: u16, height: u16) -> Rect {
    let width = width.min(screen.cols);
    let height = height.min(screen.rows);
    Rect {
        x: (screen.cols - width) / 2,
        y: (screen.rows - height) / 2,
        width,
        height,
    }
}

/// Prefer the area right of the sidebar. On tiny screens use a band strictly
/// above/below the source row; never hide the row which explains the popup.
/// No usable band means no popup, rather than an out-of-bounds/opaque screen.
pub fn beside(
    screen: Size,
    sidebar_width: u16,
    anchor_row: u16,
    width: u16,
    height: u16,
) -> Option<Rect> {
    if screen.cols == 0
        || screen.rows == 0
        || width == 0
        || height == 0
        || anchor_row >= screen.rows
    {
        return None;
    }
    let x = sidebar_width.min(screen.cols);
    let available = screen.cols - x;
    if available >= 4 {
        let height = height.min(screen.rows);
        return Some(Rect {
            x,
            y: anchor_row.min(screen.rows - height),
            width: width.min(available),
            height,
        });
    }
    let below = screen.rows - anchor_row - 1;
    let above = anchor_row;
    let (y, available) = if below >= above {
        (anchor_row + 1, below)
    } else {
        (0, above)
    };
    if available == 0 {
        return None;
    }
    let height = height.min(available);
    let y = if below >= above {
        y
    } else {
        anchor_row - height
    };
    Some(Rect {
        x: 0,
        y,
        width: width.min(screen.cols),
        height,
    })
}

/// At less than 3x3, omit decoration and give every available cell to content.
pub fn content(size: Size) -> Rect {
    if size.cols < 3 || size.rows < 3 {
        Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        }
    } else {
        Rect {
            x: 1,
            y: 1,
            width: size.cols - 2,
            height: size.rows - 2,
        }
    }
}

/// Shared panel/border styling geometry, matching the tiny-window decoration
/// policy of `content` and `border_rows`. Product title/hint spans layer above it.
pub fn theme_spans(size: Size) -> [RoleSpan; 5] {
    let panel = RoleSpan {
        rect: Rect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        },
        role: Role::Panel,
    };
    if size.cols < 3 || size.rows < 3 {
        return [
            panel,
            RoleSpan {
                rect: Rect::default(),
                role: Role::Border,
            },
            RoleSpan {
                rect: Rect::default(),
                role: Role::Border,
            },
            RoleSpan {
                rect: Rect::default(),
                role: Role::Border,
            },
            RoleSpan {
                rect: Rect::default(),
                role: Role::Border,
            },
        ];
    }
    [
        panel,
        RoleSpan {
            rect: Rect {
                x: 0,
                y: 0,
                width: size.cols,
                height: 1,
            },
            role: Role::Border,
        },
        RoleSpan {
            rect: Rect {
                x: 0,
                y: size.rows - 1,
                width: size.cols,
                height: 1,
            },
            role: Role::Border,
        },
        RoleSpan {
            rect: Rect {
                x: 0,
                y: 1,
                width: 1,
                height: size.rows - 2,
            },
            role: Role::Border,
        },
        RoleSpan {
            rect: Rect {
                x: size.cols - 1,
                y: 1,
                width: 1,
                height: size.rows - 2,
            },
            role: Role::Border,
        },
    ]
}

/// Bounded local-coordinate rows suitable for retained UiSurface painting.
pub fn border_rows(size: Size, title: &str, border: Border) -> Vec<TextLine> {
    if size.cols < 3 || size.rows < 3 {
        return Vec::new();
    }
    let (tl, tr, bl, br) = match border {
        Border::Plain => ('┌', '┐', '└', '┘'),
        Border::Rounded => ('╭', '╮', '╰', '╯'),
    };
    let middle = "─".repeat(usize::from(size.cols - 2));
    let mut rows = Vec::with_capacity(usize::from(size.rows) + 1);
    let mut add = |x, y, width, label| {
        rows.push((
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            label,
            false,
        ))
    };
    add(0, 0, size.cols, format!("{tl}{middle}{tr}"));
    add(0, size.rows - 1, size.cols, format!("{bl}{middle}{br}"));
    for y in 1..size.rows - 1 {
        add(0, y, 1, "│".into());
        add(size.cols - 1, y, 1, "│".into());
    }
    let title = fit(title, size.cols - 2);
    if !title.is_empty() {
        add(1, 0, size.cols - 2, title);
    }
    rows
}

/// Decorate an existing local frame, preserving its content and cursor state.
pub fn decorate(frame: &mut Frame, title: &str, border: Border) -> Rect {
    for (rect, label, selected) in border_rows(frame.size, title, border) {
        text(frame, rect.x, rect.y, &label, selected);
    }
    content(frame.size)
}

/// Paint connected, single-cell edge strips into an existing root-sized frame.
/// The caller owns clearing/caching the frame and projecting only the strips;
/// unrelated cells, resources and cursor state are unchanged. No palette or
/// terminal capability policy is imposed here.
///
/// Connection rules adapted from bkpr-zellij d7b40a37, tab/resize_feedback.rs.
///
/// MIT License
/// Copyright (c) 2020 Zellij contributors
///
/// Permission is hereby granted, free of charge, to any person obtaining a copy
/// of this software and associated documentation files (the "Software"), to deal
/// in the Software without restriction, including without limitation the rights
/// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
/// copies of the Software, and to permit persons to whom the Software is
/// furnished to do so, subject to the following conditions:
///
/// The above copyright notice and this permission notice shall be included in all
/// copies or substantial portions of the Software.
///
/// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
/// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
/// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
/// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
/// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
/// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
/// SOFTWARE.
pub fn paint_edge_feedback(frame: &mut Frame, segments: &[Rect], style: &Style) {
    let size = frame.size;
    // Query the small strip list directly instead of allocating a second canvas
    // or a per-cell map alongside the caller's retained frame.
    let axes = |x: u16, y: u16| {
        if x >= size.cols || y >= size.rows {
            return 0;
        }
        segments
            .iter()
            .filter(|s| s.contains(x, y))
            .fold(0, |axes, s| axes | if s.width == 1 { 1 } else { 2 })
    };
    for segment in segments {
        for y in segment.y..segment.y.saturating_add(segment.height).min(size.rows) {
            let Some(row) = frame.rows.get_mut(usize::from(y)) else {
                continue;
            };
            for x in segment.x..segment.x.saturating_add(segment.width).min(size.cols) {
                let Some(cell) = row.cells.get_mut(usize::from(x)) else {
                    continue;
                };
                let own = axes(x, y);
                let mut connections = 0;
                // Adjacent endpoints connect even when normalization places
                // them beside, rather than directly on, a crossing strip.
                for (neighbor, axis, bit) in [
                    (y.checked_sub(1).map(|y| (x, y)), 1, 1),
                    (y.checked_add(1).map(|y| (x, y)), 1, 2),
                    (x.checked_sub(1).map(|x| (x, y)), 2, 4),
                    (x.checked_add(1).map(|x| (x, y)), 2, 8),
                ] {
                    if let Some((nx, ny)) = neighbor {
                        let other = axes(nx, ny);
                        if other != 0 && (own | other) & axis != 0 {
                            connections |= bit;
                        }
                    }
                }
                let glyph = match connections {
                    5 => '┘',
                    9 => '└',
                    6 => '┐',
                    10 => '┌',
                    7 => '┤',
                    11 => '├',
                    13 => '┴',
                    14 => '┬',
                    15 => '┼',
                    4 | 8 | 12 => '─',
                    1..=3 => '│',
                    _ if own == 2 => '─',
                    _ => '│',
                };
                cell.text.clear();
                cell.text.push(glyph);
                cell.width = 1;
                cell.style.clone_from(style);
                cell.hyperlink = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn size(cols: u16, rows: u16) -> Size {
        Size {
            cols,
            rows,
            cell_width: 0,
            cell_height: 0,
        }
    }
    #[test]
    fn edge_feedback_connects_crossings_corners_tees_and_adjacent_endpoints() {
        let vertical = Rect {
            x: 2,
            y: 0,
            width: 1,
            height: 5,
        };
        let horizontal = Rect {
            x: 0,
            y: 2,
            width: 5,
            height: 1,
        };
        for (v, h, expected) in [
            (vertical, horizontal, "┼"),
            (
                vertical,
                Rect {
                    x: 2,
                    width: 3,
                    ..horizontal
                },
                "├",
            ),
            (
                vertical,
                Rect {
                    width: 3,
                    ..horizontal
                },
                "┤",
            ),
            (
                Rect {
                    height: 3,
                    ..vertical
                },
                horizontal,
                "┴",
            ),
            (
                Rect {
                    y: 2,
                    height: 3,
                    ..vertical
                },
                horizontal,
                "┬",
            ),
            (
                Rect {
                    y: 2,
                    height: 3,
                    ..vertical
                },
                Rect {
                    x: 2,
                    width: 3,
                    ..horizontal
                },
                "┌",
            ),
            (
                Rect {
                    y: 2,
                    height: 3,
                    ..vertical
                },
                Rect {
                    width: 3,
                    ..horizontal
                },
                "┐",
            ),
            (
                Rect {
                    height: 3,
                    ..vertical
                },
                Rect {
                    x: 2,
                    width: 3,
                    ..horizontal
                },
                "└",
            ),
            (
                Rect {
                    height: 3,
                    ..vertical
                },
                Rect {
                    width: 3,
                    ..horizontal
                },
                "┘",
            ),
            (
                vertical,
                Rect {
                    x: 3,
                    width: 2,
                    ..horizontal
                },
                "├",
            ),
        ] {
            let mut frame = crate::blank(size(5, 5));
            let style = crate::default_style();
            paint_edge_feedback(&mut frame, &[v, h], &style);
            assert_eq!(frame.rows[2].cells[2].text, expected);
        }
    }

    #[test]
    fn edge_feedback_clips_overflow_and_retains_unrelated_cells_cursor_and_style_policy() {
        let mut frame = crate::blank(size(5, 3));
        text(&mut frame, 0, 0, "keep", false);
        frame.cursor.visible = true;
        let cursor = frame.cursor.clone();
        let untouched = frame.rows[0].clone();
        let mut style = crate::default_style();
        style.foreground_source = bkpr_protocol::ColorSource::Palette(6);
        style.background_source = bkpr_protocol::ColorSource::Rgb([20, 30, 40]);
        style.background = [20, 30, 40];
        style.bold = true;
        let segments = [
            Rect {
                x: 4,
                y: 1,
                width: 1,
                height: u16::MAX,
            },
            Rect {
                x: 0,
                y: 2,
                width: u16::MAX,
                height: 1,
            },
            Rect {
                x: u16::MAX,
                y: u16::MAX,
                width: u16::MAX,
                height: u16::MAX,
            },
            Rect {
                x: 1,
                y: 1,
                width: 0,
                height: 1,
            },
        ];
        paint_edge_feedback(&mut frame, &segments, &style);
        assert_eq!(frame.rows[0], untouched);
        assert_eq!(frame.rows[1].cells[0].text, " ");
        assert_eq!(frame.rows[2].cells[4].text, "┘");
        assert_eq!(frame.rows[2].cells[4].style, style);
        assert_eq!(frame.cursor, cursor);
        let cells = frame.rows[2].cells.as_ptr();
        let glyph = frame.rows[2].cells[4].text.as_ptr();
        paint_edge_feedback(&mut frame, &segments, &style);
        assert_eq!(frame.rows[2].cells.as_ptr(), cells);
        assert_eq!(frame.rows[2].cells[4].text.as_ptr(), glyph);
        // Resource-only and incompletely populated frames remain safe.
        frame.rows.truncate(1);
        paint_edge_feedback(&mut frame, &segments, &style);
        paint_edge_feedback(&mut crate::graphics_frame(size(5, 3)), &segments, &style);
    }
    #[test]
    fn title_clipping_keeps_wide_and_combining_glyphs() {
        assert_eq!(fit("界e\u{301}x", 3), "界e\u{301}");
        assert_eq!(fit("界", 1), "");
        assert_eq!(fit("a\u{1b}[31m", 1), "a");
        assert_eq!(fit("\u{301}a", 1), "a");
    }
    #[test]
    fn placement_does_not_occlude_sidebar_or_source_row_when_possible() {
        let right = beside(size(80, 24), 25, 23, 60, 15).unwrap();
        assert_eq!(
            right,
            Rect {
                x: 25,
                y: 9,
                width: 55,
                height: 15
            }
        );
        let below = beside(size(10, 5), 10, 1, 50, 10).unwrap();
        assert_eq!(
            below,
            Rect {
                x: 0,
                y: 2,
                width: 10,
                height: 3
            }
        );
        assert!(!below.contains(0, 1));
        assert_eq!(beside(size(1, 1), 1, 0, 10, 10), None);
    }
    #[test]
    fn minimal_frames_have_no_border_or_lost_content() {
        assert!(border_rows(size(2, 8), "title", Border::Rounded).is_empty());
        assert_eq!(
            content(size(2, 8)),
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 8
            }
        );
    }
}
