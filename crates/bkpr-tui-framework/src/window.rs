//! Terminal-default, cell-based decorations for existing local scene frames.
//! No terminal output or graphics capabilities are assumed here.
use crate::{Rect, Role, RoleSpan, text};
use bkpr_protocol::{Frame, Size, Style};
use unicode_segmentation::UnicodeSegmentation;

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

/// Strip terminal controls, host image placeholders and directional formatting from metadata.
pub fn clean(value: &str) -> String {
    value.chars().filter(|c| printable(*c)).collect()
}
fn printable(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{10eeee}' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Fit sanitized text into `width` cells without splitting extended graphemes.
/// Leading zero-width graphemes are omitted; trailing marks remain with their base.
pub fn fit(value: &str, width: u16) -> String {
    let value = if value.chars().all(printable) {
        std::borrow::Cow::Borrowed(value)
    } else {
        std::borrow::Cow::Owned(clean(value))
    };
    let mut result = String::new();
    let mut columns = 0;
    for grapheme in value.graphemes(true) {
        let advance = unicode_width::UnicodeWidthStr::width(grapheme);
        if columns + advance > usize::from(width) {
            break;
        }
        if advance != 0 || !result.is_empty() {
            result.push_str(grapheme);
        }
        columns += advance;
    }
    result
}

/// Wrap a line of printable UI text by terminal cells, without splitting graphemes.
/// Prefer whitespace breaks; omit graphemes too wide for the entire row.
pub fn wrap_cells(value: &str, width: u16) -> impl Iterator<Item = String> + '_ {
    use unicode_width::UnicodeWidthStr as _;

    let value = if value.chars().all(printable) {
        std::borrow::Cow::Borrowed(value)
    } else {
        std::borrow::Cow::Owned(clean(value))
    };
    let mut offset = 0;
    std::iter::from_fn(move || {
        while width != 0 && offset < value.len() {
            let rest = value[offset..].trim_start();
            offset = value.len() - rest.len();
            let mut row = String::new();
            let mut used = 0;
            let mut boundary = None;
            let mut end = rest.len();
            for (index, glyph) in rest.grapheme_indices(true) {
                let cells = glyph.width();
                if cells > usize::from(width) || cells == 0 && row.is_empty() {
                    continue;
                }
                if glyph.chars().all(char::is_whitespace) && !row.ends_with(' ') {
                    boundary = Some((row.len(), index));
                }
                if used + cells > usize::from(width) {
                    end = index;
                    if let Some((keep, next)) = boundary.filter(|(keep, _)| *keep != 0) {
                        row.truncate(keep);
                        end = next;
                    }
                    break;
                }
                row.push_str(glyph);
                used += cells;
            }
            offset += end;
            if !row.is_empty() {
                return Some(row);
            }
        }
        None
    })
}

/// Center a wrapped heading and body in the given area, prioritizing heading rows.
/// Returns outer bounds, local paint rows and the local heading highlight region.
/// Tiny areas fall back to unframed text.
pub fn notice_rows<'a>(
    area: Rect,
    size: Size,
    title: &str,
    body: impl Iterator<Item = &'a str>,
    framed: bool,
) -> Option<(Rect, Vec<TextLine>, Rect)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let inset = u16::from(framed && area.width >= 3 && area.height >= 3);
    let width = area.width.saturating_sub(2 * inset).min(72);
    let capacity = usize::from(area.height - 2 * inset);
    let mut labels: Vec<_> = wrap_cells(title, width).take(capacity).collect();
    if labels.is_empty() {
        labels.push(String::new());
    }
    let heading_rows = labels.len();
    labels.extend(
        body.flat_map(str::lines)
            .flat_map(|line| wrap_cells(line, width))
            .take(capacity - heading_rows),
    );
    let columns = |label: &str| crate::text_width(label) as u16;
    let text_width = labels.iter().map(|label| columns(label)).max()?.max(1);
    let min_width = text_width + 2 * inset;
    let width = min_width + 2 * u16::from(inset != 0 && area.width - min_width >= 2);
    let height = labels.len() as u16 + 2 * inset;
    let outer = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut rows = if inset != 0 {
        border_rows(outer.size(size), "", Border::Rounded)
    } else {
        Vec::new()
    };
    rows.extend(labels.into_iter().enumerate().map(|(index, label)| {
        let cols = columns(&label);
        (
            Rect {
                x: (width - cols) / 2,
                y: inset + index as u16,
                width: cols,
                height: 1,
            },
            label,
            false,
        )
    }));
    let title = Rect {
        x: inset,
        y: inset,
        width: width - 2 * inset,
        height: heading_rows as u16,
    };
    Some((outer, rows, title))
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

/// Extend content-local separator rules through a surrounding single-cell frame.
/// Horizontal and vertical intersections use connected Unicode junctions.
pub fn connect_separators(frame: &mut Frame, content: Rect, rules: impl IntoIterator<Item = Rect>) {
    if content.x == 0 || content.y == 0 {
        return;
    }
    let left = content.x - 1;
    let right = content.x + content.width;
    let bottom = content.y + content.height;
    for rule in rules {
        if rule.height == 1 && rule.width > 1 {
            let y = content.y + rule.y;
            text(frame, left, y, "├", false);
            text(frame, right, y, "┤", false);
        } else if rule.width == 1 && rule.height > 0 {
            let x = content.x + rule.x;
            let top = content.y + rule.y;
            text(frame, x, top.saturating_sub(1), "┬", false);
            text(frame, x, bottom, "┴", false);
        }
    }
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
    #[test]
    fn notice_rows_wrap_whole_graphemes_and_degrade_inside_tiny_areas() {
        let size = Size {
            cols: 7,
            rows: 5,
            cell_width: 9,
            cell_height: 18,
        };
        let area = Rect {
            x: 4,
            y: 3,
            width: 7,
            height: 5,
        };
        let (outer, rows, title) =
            notice_rows(area, size, "Error", ["界 👩‍💻e\u{301}"].into_iter(), true).unwrap();
        assert_eq!(outer, area);
        assert_eq!((title.x, title.y, title.width), (1, 1, 5));
        assert!(rows.iter().any(|(_, text, _)| text == "╭─────╮"));
        assert!(rows.iter().any(|(_, text, _)| text == "界"));
        assert!(rows.iter().any(|(_, text, _)| text == "👩‍💻e\u{301}"));
        assert!(rows.iter().all(|(rect, text, _)| {
            rect.x + rect.width <= outer.width
                && rect.y < outer.height
                && crate::text_width(text) <= usize::from(rect.width)
        }));

        let (_, rows, title) =
            notice_rows(area, size, "Offline now", ["body"].into_iter(), true).unwrap();
        assert_eq!(title.height, 3);
        assert!(rows.iter().any(|(_, text, _)| text == "now"));
        assert!(!rows.iter().any(|(_, text, _)| text == "body"));

        let tiny = Rect {
            width: 2,
            height: 1,
            ..area
        };
        let (outer, rows, _) =
            notice_rows(tiny, size, "👩‍💻x", ["omitted"].into_iter(), true).unwrap();
        assert_eq!(outer, tiny);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "👩‍💻");
    }

    #[test]
    fn compact_separators_connect_to_both_rounded_edges_and_each_other() {
        let size = Size {
            cols: 22,
            rows: 10,
            cell_width: 9,
            cell_height: 18,
        };
        let mut frame = crate::UiSurface::new(size).frame;
        let content = decorate(&mut frame, "", Border::Rounded);
        let horizontal = Rect {
            x: 0,
            y: 2,
            width: content.width,
            height: 1,
        };
        let vertical = Rect {
            x: 8,
            y: 3,
            width: 1,
            height: content.height - 3,
        };
        crate::text(
            &mut frame,
            content.x,
            content.y + horizontal.y,
            &"─".repeat(usize::from(horizontal.width)),
            false,
        );
        for y in vertical.y..vertical.y + vertical.height {
            crate::text(
                &mut frame,
                content.x + vertical.x,
                content.y + y,
                "│",
                false,
            );
        }
        connect_separators(&mut frame, content, [horizontal, vertical]);
        let glyph = |x: usize, y: usize| frame.rows[y].cells[x].text.as_str();
        assert_eq!(glyph(0, 0), "╭");
        assert_eq!(glyph(21, 0), "╮");
        assert_eq!(glyph(0, 9), "╰");
        assert_eq!(glyph(21, 9), "╯");
        assert_eq!(glyph(0, 3), "├");
        assert_eq!(glyph(21, 3), "┤");
        assert_eq!(glyph(9, 3), "┬");
        assert_eq!(glyph(9, 9), "┴");
        assert!((1..21).all(|x| glyph(x, 3) == if x == 9 { "┬" } else { "─" }));
    }

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
        for glyph in ["👩‍💻", "✈\u{fe0f}", "🇫🇷"] {
            assert_eq!(fit(glyph, 0), "");
            assert_eq!(fit(glyph, 1), "");
            assert_eq!(fit(&format!("{glyph}x"), 2), glyph);
            assert_eq!(fit(&format!("{glyph}x"), 3), format!("{glyph}x"));
        }
        assert_eq!(fit("👩\x1b\u{200d}💻x", 2), "👩‍💻");
        assert_eq!(fit("👩\u{202e}\u{200d}💻x", 2), "👩‍💻");
        assert_eq!(fit("👩\u{10eeee}\u{200d}💻x", 2), "👩‍💻");
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
