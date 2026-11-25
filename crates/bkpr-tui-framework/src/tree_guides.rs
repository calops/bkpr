//! Tree prefixes with shared plain and graphical geometry.

use crate::{ForegroundSpan, Rect, Role, RoleSpan, graphics::SvgGraphics};
use bkpr_protocol::{ColorSource, Frame};

const INDENT: u16 = 2;

/// A visible focus edge, including the row where it turns into its child.
#[derive(Clone, Copy)]
pub struct FocusEdge {
    /// One-based child level from the root.
    pub depth: u16,
    /// First row below the parent, in the caller's row index space.
    pub start: usize,
    /// Child row, inclusive.
    pub end: usize,
}

/// All focus edges crossing a single row.
#[derive(Clone, Copy, Default)]
pub struct Focus {
    active: u8,
    turns: u8,
}

impl Focus {
    /// Resolve a row against the supplied focus path in the same index space.
    pub fn at(edges: &[Option<FocusEdge>], row: usize) -> Self {
        let mut focus = Self::default();
        for edge in edges.iter().flatten() {
            if (edge.start..=edge.end).contains(&row) {
                focus.active |= level_bit(edge.depth);
                if edge.end == row {
                    focus.turns |= level_bit(edge.depth);
                }
            }
        }
        focus
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TreeGuide {
    Stem,
    Tee,
    Elbow,
}

/// Indentation and connector topology for one row, excluding its label.
/// Continuation masks use bits 1 through 7 for the corresponding one-based levels.
#[derive(Clone, Copy, Default)]
pub struct Prefix {
    width: u16,
    stems: u8,
    branch: Option<TreeGuide>,
}

impl Prefix {
    /// Indent a node, ending in a junction or, for its last sibling, a rounded elbow.
    /// The root at depth zero has no prefix.
    pub fn node(depth: u16, continuations: u8, last_sibling: bool) -> Self {
        Self {
            width: depth.saturating_mul(INDENT),
            stems: continuations,
            branch: (depth != 0).then_some(if last_sibling {
                TreeGuide::Elbow
            } else {
                TreeGuide::Tee
            }),
        }
    }

    /// Indent a secondary text row by complete ancestor slots and a single gutter cell.
    /// Set `child_stem` when that final cell starts a child branch below a parent.
    pub fn secondary(depth: u16, continuations: u8, child_stem: bool) -> Self {
        let bit = level_bit(depth);
        Self {
            width: depth
                .saturating_sub(1)
                .saturating_mul(INDENT)
                .saturating_add(1),
            stems: if child_stem {
                continuations | bit
            } else {
                continuations & !bit
            },
            branch: None,
        }
    }

    /// Continue existing stems through a spacer occupying complete slots.
    pub fn spacer(depth: u16, continuations: u8) -> Self {
        Self {
            width: depth.saturating_mul(INDENT),
            stems: continuations,
            branch: None,
        }
    }

    /// Cell width shared by both presentations, including empty slots.
    pub fn width(self) -> u16 {
        self.width
    }

    fn slots(self, width: u16) -> impl Iterator<Item = (u16, u8, Option<TreeGuide>, u16)> {
        let width = width.min(self.width);
        (0..width).step_by(usize::from(INDENT)).map(move |offset| {
            let bit = level_bit(offset / INDENT + 1);
            let stem = (self.stems & bit != 0).then_some(TreeGuide::Stem);
            let guide = if self.width - offset <= INDENT {
                self.branch.or(stem)
            } else {
                stem
            };
            (offset, bit, guide, INDENT.min(width - offset))
        })
    }

    /// Append the Unicode fallback, or reserve the same width without glyphs.
    pub fn write_plain(self, label: &mut String, show: bool) {
        for (_, _, guide, width) in self.slots(self.width) {
            let (first, second) = match guide.filter(|_| show) {
                Some(TreeGuide::Stem) => ('│', ' '),
                Some(TreeGuide::Tee) => ('├', '─'),
                Some(TreeGuide::Elbow) => ('╰', '─'),
                None => (' ', ' '),
            };
            label.push(first);
            if width == INDENT {
                label.push(second);
            }
        }
    }

    /// Append plain guide styles inside the caller's one-row prefix bounds.
    /// The active path uses the base role; other slots use the muted role and supplied ink.
    pub fn plain_styles(
        self,
        bounds: Rect,
        focus: Focus,
        muted: ColorSource,
        roles: &mut Vec<RoleSpan>,
        foregrounds: &mut Vec<ForegroundSpan>,
    ) {
        for (offset, bit, _, width) in self.slots(bounds.width) {
            let rect = Rect {
                x: bounds.x + offset,
                width,
                height: 1,
                ..bounds
            };
            let active = focus.active & bit != 0;
            roles.push(RoleSpan {
                rect,
                role: if active { Role::Base } else { Role::Muted },
            });
            if !active {
                foregrounds.push(ForegroundSpan {
                    rect,
                    foreground: muted,
                });
            }
        }
    }

    /// Place ready guide graphics and clear only the text cells they replace.
    /// Text and effects share coordinates; `bounds` limits painting to one row.
    /// Reserve two placement IDs per cell in the effects frame starting at `base`.
    pub fn paint_fancy(
        self,
        graphics: &SvgGraphics,
        text: &mut Frame,
        effects: &mut Frame,
        bounds: Rect,
        focus: Focus,
        base: u32,
    ) {
        if !graphics.is_enabled() || text.size != effects.size {
            return;
        }
        let Some(row) = text.rows.get_mut(usize::from(bounds.y)) else {
            return;
        };
        let width = bounds.width.min(text.size.cols.saturating_sub(bounds.x));
        for (offset, bit, guide, width) in self.slots(width) {
            let Some(guide) = guide else { continue };
            let span = if matches!(guide, TreeGuide::Stem) {
                1
            } else {
                INDENT
            };
            if span > width {
                continue;
            }
            let x = bounds.x + offset;
            let Some(cells) = row.cells.get_mut(usize::from(x)..usize::from(x + span)) else {
                continue;
            };
            let cell = u32::from(bounds.y) * u32::from(text.size.cols) + u32::from(x);
            let Some(id) = cell
                .checked_mul(2)
                .and_then(|offset| base.checked_add(offset))
                .filter(|&id| id < u32::MAX)
            else {
                continue;
            };
            if graphics.place_tree_guide(
                effects,
                [i32::from(x), i32::from(bounds.y)],
                guide,
                focus.active & bit != 0,
                focus.turns & bit != 0,
                id,
            ) {
                for cell in cells {
                    cell.text.clear();
                    cell.text.push(' ');
                }
            }
        }
    }
}

fn level_bit(level: u16) -> u8 {
    1u8.checked_shl(u32::from(level)).unwrap_or(0)
}
