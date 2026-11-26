//! Tree prefixes with shared plain and graphical geometry.

use crate::{CellArtwork, ForegroundSpan, Rect, Role, RoleSpan, graphics::SvgGraphics};
use bkpr_protocol::ColorSource;

const INDENT: u16 = 2;

/// A highlighted edge, including the row where it turns into its child.
#[derive(Clone, Copy)]
pub struct FocusEdge {
    /// One-based child level from the root (`1..=7`). Other levels are ignored.
    pub depth: u16,
    /// First row below the parent, in the caller's row index space.
    pub start: usize,
    /// Child row, inclusive.
    pub end: usize,
}

/// Bright paths crossing a single row.
#[derive(Clone, Copy, Default)]
pub struct Focus {
    continues: u8,
    turns: u8,
}

impl Focus {
    /// Resolve a row against the supplied edges in the same index space.
    pub fn at(edges: &[Option<FocusEdge>], row: usize) -> Self {
        let mut focus = Self::default();
        for edge in edges.iter().flatten() {
            if (edge.start..=edge.end).contains(&row) {
                if edge.end == row {
                    focus.turns |= level_bit(edge.depth);
                } else {
                    focus.continues |= level_bit(edge.depth);
                }
            }
        }
        focus
    }

    /// Also turn into a child on this row at the given one-based level (`1..=7`).
    pub fn with_turn(mut self, depth: u16) -> Self {
        self.turns |= level_bit(depth);
        self
    }

    fn path(self, bit: u8) -> Path {
        match (self.turns & bit != 0, self.continues & bit != 0) {
            (true, true) => Path::Fork,
            (true, false) => Path::Turn,
            (false, true) => Path::Through,
            (false, false) => Path::Muted,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TreeGuide {
    Stem,
    Tee,
    Elbow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Path {
    Muted,
    Through,
    Turn,
    Fork,
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
        for (offset, bit, guide, width) in self.slots(bounds.width) {
            let rect = Rect {
                x: bounds.x + offset,
                width,
                height: 1,
                ..bounds
            };
            let path = focus.path(bit);
            let mut style = |rect, active| {
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
            };
            if matches!(guide, Some(TreeGuide::Tee)) && path == Path::Through && width == INDENT {
                // The trunk and junction share a Unicode glyph; the arm's
                // second cell can still carry its own ink.
                style(Rect { width: 1, ..rect }, true);
                style(
                    Rect {
                        x: rect.x + 1,
                        width: 1,
                        ..rect
                    },
                    false,
                );
            } else {
                style(rect, path != Path::Muted);
            }
        }
    }

    /// Offer ready guide graphics for the caller's one-row fallback bounds.
    /// Only complete groups admitted by the compositor replace text.
    pub fn paint_fancy(
        self,
        graphics: &SvgGraphics,
        artwork: &mut CellArtwork,
        bounds: Rect,
        focus: Focus,
    ) {
        if !graphics.is_enabled() || bounds.height == 0 || bounds.y >= artwork.frame.size.rows {
            return;
        }
        let width = bounds
            .width
            .min(artwork.frame.size.cols.saturating_sub(bounds.x));
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
            if !artwork.room_for(if matches!(guide, TreeGuide::Tee) {
                2
            } else {
                1
            }) {
                continue;
            }
            let x = bounds.x + offset;
            let start = artwork.frame.placements.len();
            if graphics.place_tree_guide(
                &mut artwork.frame,
                [i32::from(x), i32::from(bounds.y)],
                guide,
                focus.path(bit),
                start as u32 + 1,
            ) {
                artwork.groups.push((
                    Rect {
                        x,
                        width: span,
                        height: 1,
                        ..bounds
                    },
                    start..artwork.frame.placements.len(),
                ));
            }
        }
    }
}

fn level_bit(level: u16) -> u8 {
    if (1..=7).contains(&level) {
        1 << level
    } else {
        0
    }
}
