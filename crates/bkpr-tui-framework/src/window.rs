//! Terminal-default, cell-based decorations for existing local scene frames.
//! No terminal output or graphics capabilities are assumed here.
use crate::{Rect, Role, RoleSpan, text};
use bkpr_protocol::{Frame, Size};
use unicode_width::UnicodeWidthChar;

pub type TextLine = (Rect, String, bool);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Border {
    #[default]
    Plain,
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

/// Fit whole terminal glyphs; trailing combining marks stay with their base.
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
