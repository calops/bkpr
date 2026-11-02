//! Geometry shared by pane projection, visible dividers and resize interaction.
use bkpr_protocol::{Axis, LayoutNode};
use bkpr_tui_framework::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Split {
    pub path: Vec<bool>,
    pub axis: Axis,
    pub area: Rect,
    pub divider: Rect,
    pub ratio: u16,
}

// Rect coordinates and exclusive ends must remain representable even when a
// caller supplies an area extending beyond the terminal coordinate space.
pub(crate) fn visible_area(rect: Rect) -> Rect {
    Rect {
        width: rect.width.min(u16::MAX - rect.x),
        height: rect.height.min(u16::MAX - rect.y),
        ..rect
    }
}

fn length(rect: Rect, axis: Axis) -> u16 {
    match axis {
        Axis::Horizontal => rect.width,
        Axis::Vertical => rect.height,
    }
}

fn boundary(usable: u16, ratio: u16) -> u16 {
    ((u32::from(usable) * u32::from(ratio)) / 1000).clamp(1, u32::from(usable - 1)) as u16
}

/// The ratio divides the usable cells after reserving one real divider cell.
/// Unrepresentable splits collapse to their first subtree in every consumer.
pub(crate) fn partition(rect: Rect, axis: Axis, ratio: u16) -> Option<(Rect, Rect, Rect)> {
    let rect = visible_area(rect);
    if rect.width == 0 || rect.height == 0 || length(rect, axis) < 3 {
        return None;
    }
    let usable = length(rect, axis) - 1;
    let first = boundary(usable, ratio);
    Some(match axis {
        Axis::Horizontal => (
            Rect {
                width: first,
                ..rect
            },
            Rect {
                x: rect.x + first,
                width: 1,
                ..rect
            },
            Rect {
                x: rect.x + first + 1,
                width: usable - first,
                ..rect
            },
        ),
        Axis::Vertical => (
            Rect {
                height: first,
                ..rect
            },
            Rect {
                y: rect.y + first,
                height: 1,
                ..rect
            },
            Rect {
                y: rect.y + first + 1,
                height: usable - first,
                ..rect
            },
        ),
    })
}

pub(crate) fn dividers(layout: Option<&LayoutNode>, area: Rect, out: &mut Vec<Split>) {
    fn walk(layout: &LayoutNode, area: Rect, path: &mut Vec<bool>, out: &mut Vec<Split>) {
        let LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } = layout
        else {
            return;
        };
        let Some((a, divider, b)) = partition(area, *axis, *ratio) else {
            path.push(false);
            walk(first, area, path, out);
            path.pop();
            return;
        };
        out.push(Split {
            path: path.clone(),
            axis: *axis,
            area,
            divider,
            ratio: *ratio,
        });
        path.push(false);
        walk(first, a, path, out);
        *path.last_mut().unwrap() = true;
        walk(second, b, path, out);
        path.pop();
    }
    let area = visible_area(area);
    if area.width > 0
        && area.height > 0
        && let Some(layout) = layout
    {
        walk(layout, area, &mut Vec::new(), out);
    }
}

// One column and a header plus one application row are useful leaf minima.
// Nested splits include their own real divider. Saturation handles deep trees.
fn minimum(layout: &LayoutNode, axis: Axis) -> u16 {
    match layout {
        LayoutNode::Leaf { .. } => match axis {
            Axis::Horizontal => 1,
            Axis::Vertical => 2,
        },
        LayoutNode::Split {
            axis: split_axis,
            first,
            second,
            ..
        } => {
            let a = minimum(first, axis);
            let b = minimum(second, axis);
            if *split_axis == axis {
                a.saturating_add(b).saturating_add(1)
            } else {
                a.max(b)
            }
        }
    }
}

fn node_at<'a>(mut layout: &'a LayoutNode, path: &[bool]) -> Option<&'a LayoutNode> {
    for second in path {
        let LayoutNode::Split {
            first,
            second: other,
            ..
        } = layout
        else {
            return None;
        };
        layout = if *second { other } else { first };
    }
    Some(layout)
}

fn set_ratio(layout: &mut LayoutNode, path: &[bool], value: u16) {
    let mut node = layout;
    for second in path {
        let LayoutNode::Split {
            first,
            second: other,
            ..
        } = node
        else {
            unreachable!()
        };
        node = if *second { other } else { first };
    }
    let LayoutNode::Split { ratio, .. } = node else {
        unreachable!()
    };
    *ratio = value;
}

fn ceil_ratio(cells: u32, usable: u16) -> u32 {
    (cells * 1000).div_ceil(u32::from(usable))
}

fn resize(
    layout: &LayoutNode,
    split: &Split,
    target: i32,
    forward: Option<bool>,
) -> Option<LayoutNode> {
    let LayoutNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node_at(layout, &split.path)?
    else {
        return None;
    };
    // A stale drag descriptor must never target a different split or boundary.
    if *axis != split.axis || *ratio != split.ratio {
        return None;
    }
    let (_, divider, _) = partition(split.area, *axis, *ratio)?;
    if divider != split.divider {
        return None;
    }
    let usable = length(visible_area(split.area), *axis) - 1;
    let a = minimum(first, *axis);
    let b = minimum(second, *axis);
    let (low, high) = if u32::from(a) + u32::from(b) <= u32::from(usable) {
        (a, usable - b)
    } else {
        (1, usable - 1)
    };
    let current = boundary(usable, *ratio);
    if target == i32::from(current) {
        return None;
    }
    let target = target.clamp(i32::from(low), i32::from(high)) as u16;
    if current == target
        || forward == Some(true) && target < current
        || forward == Some(false) && target > current
    {
        return None;
    }
    // Work in native thousandths, not a transient cell-only layout. At very
    // large widths some cell positions cannot be represented by the protocol.
    let min_ratio = if low == 1 {
        1
    } else {
        ceil_ratio(u32::from(low), usable).max(1)
    };
    let max_ratio = if high == usable - 1 {
        999
    } else {
        (ceil_ratio(u32::from(high) + 1, usable) - 1).min(999)
    };
    if min_ratio > max_ratio {
        return None;
    }
    let upper = ceil_ratio(u32::from(target), usable).clamp(min_ratio, max_ratio);
    let lower = (ceil_ratio(u32::from(target) + 1, usable) - 1).clamp(min_ratio, max_ratio);
    let value = match forward {
        Some(true) => upper,
        Some(false) => lower,
        None => {
            let before = (u32::from(target) * 1000 / u32::from(usable)).clamp(min_ratio, max_ratio);
            if boundary(usable, before as u16).abs_diff(target)
                <= boundary(usable, upper as u16).abs_diff(target)
            {
                before
            } else {
                upper
            }
        }
    } as u16;
    if boundary(usable, value) == current {
        return None;
    }
    let mut result = layout.clone();
    set_ratio(&mut result, &split.path, value);
    Some(result)
}

pub(crate) fn drag(layout: &LayoutNode, split: &Split, x: u16, y: u16) -> Option<LayoutNode> {
    let offset = match split.axis {
        Axis::Horizontal => i32::from(x) - i32::from(split.area.x),
        Axis::Vertical => i32::from(y) - i32::from(split.area.y),
    };
    resize(layout, split, offset, None)
}

pub(crate) fn nudge(
    layout: &LayoutNode,
    area: Rect,
    pane: &str,
    direction: Direction,
    cells: u16,
) -> Option<LayoutNode> {
    fn find(
        node: &LayoutNode,
        area: Rect,
        pane: &str,
        axis: Axis,
        path: &mut Vec<bool>,
    ) -> (bool, Option<Split>) {
        let LayoutNode::Split {
            axis: split_axis,
            ratio,
            first,
            second,
        } = node
        else {
            return (
                matches!(node, LayoutNode::Leaf { pane_id } if pane_id == pane),
                None,
            );
        };
        let Some((a, divider, b)) = partition(area, *split_axis, *ratio) else {
            // Keep path identities even when the second subtree is hidden.
            path.push(false);
            let found = find(first, area, pane, axis, path);
            path.pop();
            return found;
        };
        path.push(false);
        let mut found = find(first, a, pane, axis, path);
        if !found.0 {
            *path.last_mut().unwrap() = true;
            found = find(second, b, pane, axis, path);
        }
        path.pop();
        if found.0 && found.1.is_none() && *split_axis == axis {
            found.1 = Some(Split {
                path: path.clone(),
                axis,
                area,
                divider,
                ratio: *ratio,
            });
        }
        found
    }
    if cells == 0 {
        return None;
    }
    let (axis, forward) = match direction {
        Direction::Left => (Axis::Horizontal, false),
        Direction::Right => (Axis::Horizontal, true),
        Direction::Up => (Axis::Vertical, false),
        Direction::Down => (Axis::Vertical, true),
    };
    let area = visible_area(area);
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let split = find(layout, area, pane, axis, &mut Vec::new()).1?;
    let current = match axis {
        Axis::Horizontal => split.divider.x - split.area.x,
        Axis::Vertical => split.divider.y - split.area.y,
    };
    let offset = if forward {
        i32::from(cells)
    } else {
        -i32::from(cells)
    };
    resize(layout, &split, i32::from(current) + offset, Some(forward))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::layout_rects;
    use std::collections::HashSet;

    fn leaf(id: &str) -> LayoutNode {
        LayoutNode::Leaf { pane_id: id.into() }
    }

    fn split(axis: Axis, ratio: u16, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 7,
            y: 3,
            width,
            height,
        }
    }

    fn nested() -> LayoutNode {
        split(
            Axis::Horizontal,
            500,
            split(Axis::Vertical, 500, leaf("a"), leaf("b")),
            split(Axis::Horizontal, 500, leaf("c"), leaf("d")),
        )
    }

    fn boundaries(layout: &LayoutNode, area: Rect) -> Vec<Split> {
        let mut result = Vec::new();
        dividers(Some(layout), area, &mut result);
        result
    }

    fn projected(layout: &LayoutNode, area: Rect) -> Vec<(String, Rect)> {
        let mut result = Vec::new();
        layout_rects(Some(layout), area, &mut result);
        result
    }

    fn assert_coverage(layout: &LayoutNode, area: Rect) {
        let area = visible_area(area);
        let panes = projected(layout, area);
        let splits = boundaries(layout, area);
        let mut covered = HashSet::new();
        for rect in panes
            .iter()
            .map(|(_, rect)| *rect)
            .chain(splits.iter().map(|s| s.divider))
        {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    assert!(area.contains(x, y), "projection escapes the layout area");
                    assert!(
                        covered.insert((x, y)),
                        "pane or divider overlaps another projection"
                    );
                }
            }
        }
        assert_eq!(
            covered.len(),
            usize::from(area.width) * usize::from(area.height)
        );
    }

    #[test]
    fn pane_and_real_divider_cells_cover_nested_and_tiny_layouts_exactly_once() {
        for tree in [
            nested(),
            split(Axis::Horizontal, 1, nested(), leaf("e")),
            split(Axis::Vertical, 999, leaf("e"), nested()),
        ] {
            for width in 0..25 {
                for height in 0..15 {
                    assert_coverage(&tree, area(width, height));
                }
            }
        }
    }

    #[test]
    fn keyboard_moves_screen_boundary_and_preserves_other_splits_and_topology() {
        let tree = nested();
        let rect = area(81, 25);
        let original = boundaries(&tree, rect);
        // The vertical boundary nearest b moves, not the horizontal root.
        let down = nudge(&tree, rect, "b", Direction::Down, 5).unwrap();
        let moved = boundaries(&down, rect);
        assert_eq!(moved[0], original[0]);
        assert_eq!(moved[2], original[2]);
        assert_eq!(moved[1].divider.y, original[1].divider.y + 5);
        assert_eq!(
            projected(&down, rect)
                .iter()
                .map(|p| p.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        let up = nudge(&down, rect, "a", Direction::Up, 5).unwrap();
        assert_eq!(projected(&up, rect), projected(&tree, rect));
        let right = nudge(&tree, rect, "c", Direction::Right, 1).unwrap();
        let moved = boundaries(&right, rect);
        assert_eq!(moved[0], original[0]);
        assert_eq!(moved[1], original[1]);
        assert_eq!(moved[2].divider.x, original[2].divider.x + 1);
        let left = nudge(&right, rect, "d", Direction::Left, 1).unwrap();
        assert_eq!(projected(&left, rect), projected(&tree, rect));
        // A perpendicular inner split falls back to the matching ancestor.
        let right = nudge(&tree, rect, "a", Direction::Right, 1).unwrap();
        assert_eq!(
            boundaries(&right, rect)[0].divider.x,
            original[0].divider.x + 1
        );
        assert_coverage(&right, rect);
    }

    #[test]
    fn pointer_native_ratio_round_trips_keep_exact_representable_boundaries() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for length in 6..80 {
                let rect = match axis {
                    Axis::Horizontal => area(length, 8),
                    Axis::Vertical => area(8, length),
                };
                let tree = split(axis, 500, leaf("a"), leaf("b"));
                let old = boundaries(&tree, rect).remove(0);
                // Vertical leaves retain both header and content.
                let low = if axis == Axis::Vertical { 2 } else { 1 };
                let high = length - 1 - low;
                for target in low..=high {
                    let (x, y) = match axis {
                        Axis::Horizontal => (rect.x + target, rect.y),
                        Axis::Vertical => (rect.x, rect.y + target),
                    };
                    let changed = drag(&tree, &old, x, y).unwrap_or_else(|| tree.clone());
                    let persisted = serde_json::from_value::<LayoutNode>(
                        serde_json::to_value(&changed).unwrap(),
                    )
                    .unwrap();
                    let divider = boundaries(&persisted, rect).remove(0);
                    assert!(divider.divider.contains(x, y));
                    assert!(drag(&persisted, &divider, x, y).is_none());
                    assert_coverage(&persisted, rect);
                }
            }
        }
    }

    #[test]
    fn drag_and_large_nudges_clamp_to_useful_nested_subtree_minima() {
        let rect = area(21, 15);
        let tree = nested();
        let root = boundaries(&tree, rect).remove(0);
        let left = drag(&tree, &root, 0, 0).unwrap();
        let panes = projected(&left, rect);
        assert_eq!(panes[0].1.width, 1);
        let end = nudge(&tree, rect, "a", Direction::Right, u16::MAX).unwrap();
        let panes = projected(&end, rect);
        assert_eq!(panes[2].1.width, 1);
        assert_eq!(panes[3].1.width, 1);
        assert!(nudge(&end, rect, "a", Direction::Right, 1).is_none());
        let top = nudge(&tree, rect, "a", Direction::Up, u16::MAX).unwrap();
        assert_eq!(projected(&top, rect)[0].1.height, 2);
        assert!(nudge(&top, rect, "b", Direction::Up, 1).is_none());
        let bottom = nudge(&tree, rect, "b", Direction::Down, u16::MAX).unwrap();
        assert_eq!(projected(&bottom, rect)[1].1.height, 2);
        assert!(nudge(&bottom, rect, "a", Direction::Down, 1).is_none());
        assert_coverage(&end, rect);
    }

    #[test]
    fn tiny_collapsed_parent_preserves_child_path_and_never_exposes_hidden_panes() {
        let tree = split(
            Axis::Horizontal,
            500,
            split(Axis::Vertical, 500, leaf("a"), leaf("b")),
            leaf("hidden"),
        );
        let rect = area(2, 12);
        let splits = boundaries(&tree, rect);
        assert_eq!(splits.len(), 1);
        assert_eq!(splits[0].path, [false]);
        assert_eq!(
            projected(&tree, rect)
                .iter()
                .map(|p| p.0.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        let moved = drag(&tree, &splits[0], rect.x, rect.y + 8).unwrap();
        assert_eq!(boundaries(&moved, rect)[0].divider.y, rect.y + 8);
        assert!(nudge(&tree, rect, "hidden", Direction::Down, 1).is_none());
        assert!(nudge(&tree, rect, "a", Direction::Left, 1).is_none());
        assert_coverage(&moved, rect);
    }

    #[test]
    fn existing_tight_layout_does_not_reverse_arrows_or_snap_an_unmoved_pointer() {
        let rect = area(40, 15);
        for (ratio, direction) in [(1, Direction::Up), (999, Direction::Down)] {
            let tree = split(Axis::Vertical, ratio, leaf("a"), leaf("b"));
            let divider = boundaries(&tree, rect).remove(0);
            assert!(drag(&tree, &divider, divider.divider.x, divider.divider.y).is_none());
            assert!(nudge(&tree, rect, "a", direction, 1).is_none());
        }
    }

    #[test]
    fn no_change_missing_boundary_and_stale_target_do_not_mutate() {
        let tree = split(Axis::Horizontal, 500, leaf("a"), leaf("b"));
        let rect = area(40, 15);
        let boundary = boundaries(&tree, rect).remove(0);
        assert!(drag(&tree, &boundary, boundary.divider.x, boundary.divider.y).is_none());
        assert!(nudge(&tree, rect, "a", Direction::Up, 1).is_none());
        assert!(nudge(&tree, rect, "missing", Direction::Right, 1).is_none());
        assert!(nudge(&tree, rect, "a", Direction::Right, 0).is_none());
        assert!(nudge(&leaf("a"), rect, "a", Direction::Left, 1).is_none());
        let changed = nudge(&tree, rect, "a", Direction::Right, 1).unwrap();
        assert!(
            drag(
                &changed,
                &boundary,
                boundary.divider.x + 5,
                boundary.divider.y
            )
            .is_none()
        );
        let mut invalid = boundary;
        invalid.path.push(true);
        assert!(drag(&tree, &invalid, 0, 0).is_none());
    }

    #[test]
    fn extreme_coordinates_ratios_and_quantized_native_boundaries_never_wrap() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for ratio in [0, 1, 500, 999, u16::MAX] {
                let rect = Rect {
                    x: 0,
                    y: 0,
                    width: u16::MAX,
                    height: u16::MAX,
                };
                let (a, divider, b) = partition(rect, axis, ratio).unwrap();
                assert_eq!(
                    u32::from(length(a, axis))
                        + u32::from(length(divider, axis))
                        + u32::from(length(b, axis)),
                    u32::from(u16::MAX)
                );
            }
        }
        assert!(
            partition(
                Rect {
                    x: u16::MAX,
                    y: 0,
                    width: 10,
                    height: 10
                },
                Axis::Horizontal,
                500
            )
            .is_none()
        );
        let rect = Rect {
            x: u16::MAX - 8,
            y: u16::MAX - 5,
            width: 100,
            height: 100,
        };
        assert_coverage(&nested(), rect);
        let rect = Rect {
            x: 0,
            y: 0,
            width: u16::MAX,
            height: 5,
        };
        let tree = split(Axis::Horizontal, 500, leaf("a"), leaf("b"));
        let initial = boundaries(&tree, rect)[0].divider.x;
        let moved = nudge(&tree, rect, "a", Direction::Right, 1).unwrap();
        assert!(boundaries(&moved, rect)[0].divider.x > initial);
        let moved = nudge(&tree, rect, "a", Direction::Left, 1).unwrap();
        assert!(boundaries(&moved, rect)[0].divider.x < initial);
    }
}
