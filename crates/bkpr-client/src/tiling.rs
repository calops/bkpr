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

struct SplitRef<'a> {
    path: &'a [bool],
    axis: Axis,
    area: Rect,
    divider: Rect,
    ratio: u16,
}

impl<'a> From<&'a Split> for SplitRef<'a> {
    fn from(split: &'a Split) -> Self {
        Self {
            path: &split.path,
            axis: split.axis,
            area: split.area,
            divider: split.divider,
            ratio: split.ratio,
        }
    }
}

/// A split identity and the distance from the initial pointer to its divider.
/// Ratios and rectangles are deliberately re-resolved after every accepted CAS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DragTarget {
    path: Vec<bool>,
    axis: Axis,
    grab_offset: i32,
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
    ((u32::from(usable) * u32::from(ratio)) / 1000).clamp(2, u32::from(usable - 2)) as u16
}

/// Adjacent frame rectangles overlap on one shared divider cell. Their inset
/// native bodies remain disjoint; no extra gutter sits between the borders.
/// Unrepresentable splits collapse to their first subtree in every consumer.
pub(crate) fn partition(rect: Rect, axis: Axis, ratio: u16) -> Option<(Rect, Rect, Rect)> {
    let rect = visible_area(rect);
    if rect.width < 3 || rect.height < 3 || length(rect, axis) < 5 {
        return None;
    }
    let usable = length(rect, axis) - 1;
    let first = boundary(usable, ratio);
    Some(match axis {
        Axis::Horizontal => (
            Rect {
                width: first + 1,
                ..rect
            },
            Rect {
                x: rect.x + first,
                width: 1,
                ..rect
            },
            Rect {
                x: rect.x + first,
                width: usable - first + 1,
                ..rect
            },
        ),
        Axis::Vertical => (
            Rect {
                height: first + 1,
                ..rect
            },
            Rect {
                y: rect.y + first,
                height: 1,
                ..rect
            },
            Rect {
                y: rect.y + first,
                height: usable - first + 1,
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

fn split_at<'a>(layout: &LayoutNode, area: Rect, path: &'a [bool]) -> Option<SplitRef<'a>> {
    let mut node = layout;
    let mut area = visible_area(area);
    for second_child in path {
        let LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } = node
        else {
            return None;
        };
        if let Some((a, _, b)) = partition(area, *axis, *ratio) {
            area = if *second_child { b } else { a };
        } else if *second_child {
            return None;
        }
        node = if *second_child { second } else { first };
    }
    let LayoutNode::Split { axis, ratio, .. } = node else {
        return None;
    };
    let (_, divider, _) = partition(area, *axis, *ratio)?;
    Some(SplitRef {
        path,
        axis: *axis,
        area,
        divider,
        ratio: *ratio,
    })
}

/// Match the retained Zellij pane-frame policy: the pointer's pane quadrant
/// selects its nearest horizontal AND vertical edge, without a distance band.
/// Only internal, movable boundaries qualify; application cells never do.
/// Source: bkpr-zellij d7b40a37, tab/mod.rs::get_edge_at_position and
/// panes/tiled_panes/resize_feedback_grid.rs::resize_feedback_segment.
pub(crate) fn resize_targets(layout: &LayoutNode, area: Rect, x: u16, y: u16) -> Vec<DragTarget> {
    fn frame_at(
        node: &LayoutNode,
        area: Rect,
        x: u16,
        y: u16,
        path: &mut Vec<bool>,
    ) -> Option<Rect> {
        if !area.contains(x, y) {
            return None;
        }
        let LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } = node
        else {
            return Some(area);
        };
        let Some((a, _, b)) = partition(area, *axis, *ratio) else {
            path.push(false);
            let found = frame_at(first, area, x, y, path);
            if found.is_none() {
                path.pop();
            }
            return found;
        };
        path.push(false);
        if let Some(frame) = frame_at(first, a, x, y, path) {
            return Some(frame);
        }
        *path.last_mut().unwrap() = true;
        if let Some(frame) = frame_at(second, b, x, y, path) {
            return Some(frame);
        }
        path.pop();
        None
    }
    let area = visible_area(area);
    let mut pane_path = Vec::new();
    let Some(frame) = frame_at(layout, area, x, y, &mut pane_path) else {
        return Vec::new();
    };
    if crate::controller::content(frame).contains(x, y) {
        return Vec::new();
    }
    let edge_x = if x < frame.x + frame.width / 2 {
        frame.x
    } else {
        frame.x + frame.width - 1
    };
    let edge_y = if y < frame.y + frame.height / 2 {
        frame.y
    } else {
        frame.y + frame.height - 1
    };
    let mut targets = Vec::new();
    // Visit ancestors before descendants: moving an ancestor changes the
    // rectangle in which a selected descendant must subsequently be resolved.
    for depth in 0..pane_path.len() {
        let Some(split) = split_at(layout, area, &pane_path[..depth]) else {
            continue;
        };
        let selected = match split.axis {
            Axis::Horizontal => split.divider.x == edge_x,
            Axis::Vertical => split.divider.y == edge_y,
        };
        if selected && movable(layout, &split) {
            targets.push(DragTarget {
                path: split.path.to_vec(),
                axis: split.axis,
                grab_offset: match split.axis {
                    Axis::Horizontal => i32::from(x) - i32::from(split.divider.x),
                    Axis::Vertical => i32::from(y) - i32::from(split.divider.y),
                },
            });
        }
    }
    targets
}

pub(crate) fn resize_feedback(
    layout: &LayoutNode,
    area: Rect,
    targets: &[DragTarget],
) -> Vec<Rect> {
    targets
        .iter()
        .filter_map(|target| {
            let split = split_at(layout, area, &target.path)?;
            (split.axis == target.axis && movable(layout, &split)).then_some(split.divider)
        })
        .collect()
}

// Reserve the client-selected frame and one application cell per leaf.
// Adjacent subtrees share their border cell; saturation handles deep trees.
fn minimum(layout: &LayoutNode, axis: Axis) -> u16 {
    match layout {
        LayoutNode::Leaf { .. } => crate::appearance::PANE_BORDER
            .saturating_mul(2)
            .saturating_add(1),
        LayoutNode::Split {
            axis: split_axis,
            first,
            second,
            ..
        } => {
            let a = minimum(first, axis);
            let b = minimum(second, axis);
            if *split_axis == axis {
                a.saturating_add(b.saturating_sub(1))
            } else {
                a.max(b)
            }
        }
    }
}

/// Minimum extent preserving the existing ratios below a moved split, not
/// merely the sum of leaf minima. Otherwise an uneven descendant can lose its
/// last native cell while its parent still satisfies that aggregate minimum.
fn minimum_at_ratios(layout: &LayoutNode, axis: Axis) -> Option<u16> {
    let LayoutNode::Split {
        axis: split_axis,
        ratio,
        first,
        second,
    } = layout
    else {
        return Some(minimum(layout, axis));
    };
    let a = minimum_at_ratios(first, axis)?;
    let b = minimum_at_ratios(second, axis)?;
    if *split_axis != axis {
        return Some(a.max(b));
    }
    let fits = |extent: u16| {
        let usable = extent - 1;
        let first = boundary(usable, *ratio);
        first + 1 >= a && usable - first + 1 >= b
    };
    if !fits(u16::MAX) {
        return None;
    }
    let mut low = 5;
    let mut high = u16::MAX;
    while low < high {
        let mid = low + (high - low) / 2;
        if fits(mid) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    Some(low)
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

fn resized_ratio(
    layout: &LayoutNode,
    split: &SplitRef<'_>,
    target: i32,
    forward: Option<bool>,
    preserve_subtrees: bool,
) -> Option<u16> {
    let LayoutNode::Split {
        axis,
        ratio,
        first,
        second,
    } = node_at(layout, split.path)?
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
    let a = if preserve_subtrees {
        minimum_at_ratios(first, *axis)?
    } else {
        minimum(first, *axis)
    }
    .saturating_sub(1);
    let b = if preserve_subtrees {
        minimum_at_ratios(second, *axis)?
    } else {
        minimum(second, *axis)
    }
    .saturating_sub(1);
    let (low, high) = if u32::from(a) + u32::from(b) <= u32::from(usable) {
        (a, usable - b)
    } else if preserve_subtrees {
        return None;
    } else {
        (2, usable - 2)
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
    let min_ratio = if low == 2 {
        1
    } else {
        ceil_ratio(u32::from(low), usable).max(1)
    };
    let max_ratio = if high == usable - 2 {
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
    Some(value)
}

fn resize(
    layout: &LayoutNode,
    split: &Split,
    target: i32,
    forward: Option<bool>,
) -> Option<LayoutNode> {
    let value = resized_ratio(layout, &split.into(), target, forward, false)?;
    let mut result = layout.clone();
    set_ratio(&mut result, &split.path, value);
    Some(result)
}

fn movable(layout: &LayoutNode, split: &SplitRef<'_>) -> bool {
    resized_ratio(layout, split, i32::MIN, None, true).is_some()
        || resized_ratio(layout, split, i32::MAX, None, true).is_some()
}

/// Materialize a whole pointer position atomically, with at most one tree copy.
pub(crate) fn drag_targets(
    layout: &LayoutNode,
    area: Rect,
    targets: &[DragTarget],
    x: u16,
    y: u16,
) -> Option<LayoutNode> {
    // Reject all of a stale target set rather than applying a partial corner.
    for target in targets {
        if split_at(layout, area, &target.path)?.axis != target.axis {
            return None;
        }
    }
    let mut result = None;
    for target in targets {
        let current = result.as_ref().unwrap_or(layout);
        let split = split_at(current, area, &target.path)?;
        let offset = match split.axis {
            Axis::Horizontal => i32::from(x) - i32::from(split.area.x),
            Axis::Vertical => i32::from(y) - i32::from(split.area.y),
        } - target.grab_offset;
        if let Some(value) = resized_ratio(current, &split, offset, None, true) {
            set_ratio(
                result.get_or_insert_with(|| layout.clone()),
                &target.path,
                value,
            );
        }
    }
    result
}

#[cfg(test)]
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

    fn assert_native_ownership(layout: &LayoutNode, area: Rect) {
        let area = visible_area(area);
        let panes = projected(layout, area);
        let splits = boundaries(layout, area);
        let mut covered = HashSet::new();
        for (_, frame) in panes {
            let body = crate::controller::content(frame);
            for y in body.y..body.y + body.height {
                for x in body.x..body.x + body.width {
                    assert!(
                        area.contains(x, y),
                        "native projection escapes the layout area"
                    );
                    assert!(covered.insert((x, y)), "two applications own one cell");
                    assert!(
                        !splits.iter().any(|split| split.divider.contains(x, y)),
                        "resize hit geometry covers native input"
                    );
                }
            }
        }
    }

    #[test]
    fn native_bodies_and_resize_hits_stay_disjoint_in_nested_and_tiny_layouts() {
        for tree in [
            nested(),
            split(Axis::Horizontal, 1, nested(), leaf("e")),
            split(Axis::Vertical, 999, leaf("e"), nested()),
        ] {
            for width in 0..25 {
                for height in 0..15 {
                    assert_native_ownership(&tree, area(width, height));
                }
            }
        }
    }

    #[test]
    fn both_split_orientations_have_one_shared_cell_between_native_bodies() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for ratio in [1, 250, 500, 750, 999] {
                let rect = area(80, 36);
                let tree = split(axis, ratio, leaf("a"), leaf("b"));
                let panes = projected(&tree, rect);
                let a = crate::controller::content(panes[0].1);
                let b = crate::controller::content(panes[1].1);
                match axis {
                    Axis::Horizontal => assert_eq!(a.x + a.width + 1, b.x),
                    Axis::Vertical => assert_eq!(a.y + a.height + 1, b.y),
                }
                assert_native_ownership(&tree, rect);
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
        assert_native_ownership(&right, rect);
    }

    #[test]
    fn pointer_native_ratio_round_trips_keep_exact_representable_boundaries() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for length in 7..80 {
                let rect = match axis {
                    Axis::Horizontal => area(length, 8),
                    Axis::Vertical => area(8, length),
                };
                let tree = split(axis, 500, leaf("a"), leaf("b"));
                let old = boundaries(&tree, rect).remove(0);
                let low = minimum(&leaf("a"), axis);
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
                    assert_native_ownership(&persisted, rect);
                }
            }
        }
    }

    #[test]
    fn drag_and_large_nudges_clamp_to_useful_nested_subtree_minima() {
        let assert_framed_cell = |pane: Rect, axis: Axis| {
            let body = crate::controller::content(pane);
            assert!(body.x > pane.x && body.y > pane.y);
            assert!(body.x + body.width < pane.x + pane.width);
            assert!(body.y + body.height < pane.y + pane.height);
            assert!(body.width > 0 && body.height > 0);
            assert_eq!(length(body, axis), 1);
        };
        let rect = area(21, 15);
        let tree = nested();
        let root = boundaries(&tree, rect).remove(0);
        let left = drag(&tree, &root, 0, 0).unwrap();
        let panes = projected(&left, rect);
        assert_framed_cell(panes[0].1, Axis::Horizontal);
        let end = nudge(&tree, rect, "a", Direction::Right, u16::MAX).unwrap();
        let panes = projected(&end, rect);
        assert_framed_cell(panes[2].1, Axis::Horizontal);
        assert_framed_cell(panes[3].1, Axis::Horizontal);
        assert!(nudge(&end, rect, "a", Direction::Right, 1).is_none());
        let top = nudge(&tree, rect, "a", Direction::Up, u16::MAX).unwrap();
        assert_framed_cell(projected(&top, rect)[0].1, Axis::Vertical);
        assert!(nudge(&top, rect, "b", Direction::Up, 1).is_none());
        let bottom = nudge(&tree, rect, "b", Direction::Down, u16::MAX).unwrap();
        assert_framed_cell(projected(&bottom, rect)[1].1, Axis::Vertical);
        assert!(nudge(&bottom, rect, "a", Direction::Down, 1).is_none());
        assert_native_ownership(&end, rect);
    }

    #[test]
    fn tiny_collapsed_parent_preserves_child_path_and_never_exposes_hidden_panes() {
        let tree = split(
            Axis::Horizontal,
            500,
            split(Axis::Vertical, 500, leaf("a"), leaf("b")),
            leaf("hidden"),
        );
        let rect = area(crate::appearance::PANE_BORDER * 2 + 1, 12);
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
        assert_native_ownership(&moved, rect);
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
                let a = crate::controller::content(a);
                let b = crate::controller::content(b);
                assert_eq!(
                    u32::from(length(a, axis)) + u32::from(length(b, axis)) + 3,
                    u32::from(u16::MAX)
                );
                assert!(!divider.contains(a.x, a.y));
                assert!(!divider.contains(b.x, b.y));
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
        assert_native_ownership(&nested(), rect);
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

#[cfg(test)]
mod pointer_tests {
    use super::*;
    use crate::controller::{content, layout_rects};

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

    fn cross(axis: Axis) -> LayoutNode {
        let other = match axis {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        };
        split(
            axis,
            500,
            split(other, 500, leaf("a"), leaf("b")),
            split(other, 500, leaf("c"), leaf("d")),
        )
    }

    fn area() -> Rect {
        Rect {
            x: 7,
            y: 3,
            width: 81,
            height: 41,
        }
    }

    fn splits(layout: &LayoutNode, area: Rect) -> Vec<Split> {
        let mut splits = Vec::new();
        dividers(Some(layout), area, &mut splits);
        splits
    }

    #[test]
    fn frame_quadrant_proximity_and_grab_offsets_match_the_actual_pane() {
        let layout = cross(Axis::Horizontal);
        let area = area();
        let before = splits(&layout, area);
        let mut panes = Vec::new();
        layout_rects(Some(&layout), area, &mut panes);
        let frame = panes[0].1;
        let x = frame.x + frame.width / 2;
        let y = frame.y + frame.height - 1;
        assert!(before[0].divider.x - x > 1, "not a distance-band corner");
        let targets = resize_targets(&layout, area, x, y);
        assert_eq!(
            resize_feedback(&layout, area, &targets),
            [before[0].divider, before[1].divider]
        );
        assert!(drag_targets(&layout, area, &targets, x, y).is_none());
        let moved = drag_targets(&layout, area, &targets, x + 4, y + 3).unwrap();
        let after = splits(&moved, area);
        assert_eq!(after[0].divider.x, before[0].divider.x + 4);
        assert_eq!(after[1].divider.y, before[1].divider.y + 3);
        assert_eq!(after[2].ratio, before[2].ratio);

        // The midpoint belongs to the right quadrant; the preceding column
        // instead selects the pane's immovable outer-left edge.
        let left = resize_targets(&layout, area, x - 1, y);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, [false]);
        let body = content(frame);
        assert!(resize_targets(&layout, area, body.x, body.y).is_empty());
        assert!(resize_targets(&layout, area, area.x - 1, y).is_empty());
    }

    #[test]
    fn cross_corner_changes_both_axes_but_not_the_other_aligned_segment() {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let layout = cross(axis);
            let area = area();
            let before = splits(&layout, area);
            let (x, y) = match axis {
                Axis::Horizontal => (before[0].divider.x, before[1].divider.y),
                Axis::Vertical => (before[1].divider.x, before[0].divider.y),
            };
            let targets = resize_targets(&layout, area, x, y);
            assert_eq!(targets.len(), 2);
            let moved = drag_targets(&layout, area, &targets, x + 6, y + 4).unwrap();
            let after = splits(&moved, area);
            assert_eq!(after[0].path, before[0].path);
            assert_eq!(after[1].path, before[1].path);
            assert_eq!(after[2].ratio, before[2].ratio);
            match axis {
                Axis::Horizontal => {
                    assert_eq!(after[0].divider.x, x + 6);
                    assert_eq!(after[1].divider.y, y + 4);
                    assert_eq!(after[2].divider.y, y);
                }
                Axis::Vertical => {
                    assert_eq!(after[0].divider.y, y + 4);
                    assert_eq!(after[1].divider.x, x + 6);
                    assert_eq!(after[2].divider.x, x);
                }
            }
            assert_eq!(
                resize_feedback(&moved, area, &targets),
                [after[0].divider, after[1].divider]
            );
        }
    }

    #[test]
    fn nested_t_corner_keeps_target_identity_after_successive_authoritative_updates() {
        let layout = split(
            Axis::Vertical,
            500,
            split(Axis::Horizontal, 500, leaf("a"), leaf("b")),
            leaf("c"),
        );
        let area = area();
        let before = splits(&layout, area);
        let (x, y) = (before[1].divider.x + 3, before[0].divider.y);
        let targets = resize_targets(&layout, area, x, y);
        assert_eq!(targets.len(), 2);
        let first = drag_targets(&layout, area, &targets, x + 2, y + 3).unwrap();
        let accepted =
            serde_json::from_value::<LayoutNode>(serde_json::to_value(&first).unwrap()).unwrap();
        assert!(drag_targets(&accepted, area, &targets, x + 2, y + 3).is_none());
        let final_layout = drag_targets(&accepted, area, &targets, x + 7, y + 6).unwrap();
        let final_splits = splits(&final_layout, area);
        assert_eq!(final_splits[0].divider.y, before[0].divider.y + 6);
        assert_eq!(final_splits[1].divider.x, before[1].divider.x + 7);
        assert_eq!(
            resize_feedback(&final_layout, area, &targets),
            [final_splits[0].divider, final_splits[1].divider]
        );
    }

    #[test]
    fn corner_clamps_both_axes_without_starving_any_nested_subtree() {
        let layout = split(
            Axis::Horizontal,
            650,
            split(
                Axis::Horizontal,
                200,
                split(Axis::Horizontal, 500, leaf("a"), leaf("b")),
                leaf("c"),
            ),
            split(Axis::Vertical, 500, leaf("d"), leaf("e")),
        );
        let area = area();
        let before = splits(&layout, area);
        let (x, y) = (before[0].divider.x + 1, before[3].divider.y);
        let targets = resize_targets(&layout, area, x, y);
        assert_eq!(targets.len(), 2);
        for (x, y) in [(0, 0), (u16::MAX, u16::MAX)] {
            let moved = drag_targets(&layout, area, &targets, x, y).unwrap();
            let mut panes = Vec::new();
            layout_rects(Some(&moved), area, &mut panes);
            assert_eq!(panes.len(), 5);
            for (_, frame) in panes {
                let body = content(frame);
                assert!(body.width >= 1 && body.height >= 1);
                assert!(body.x > frame.x && body.y > frame.y);
                assert!(body.x + body.width < frame.x + frame.width);
                assert!(body.y + body.height < frame.y + frame.height);
            }
            assert!(drag_targets(&moved, area, &targets, x, y).is_none());
        }
    }

    #[test]
    fn immovable_outer_collapsed_and_stale_corner_targets_do_not_mutate() {
        let layout = cross(Axis::Horizontal);
        let area = area();
        assert!(resize_targets(&layout, area, area.x, area.y).is_empty());
        let tight = Rect {
            width: 5,
            height: 9,
            ..area
        };
        let pair = split(Axis::Horizontal, 500, leaf("a"), leaf("b"));
        assert!(resize_targets(&pair, tight, tight.x + 2, tight.y).is_empty());
        let collapsed = Rect { width: 4, ..tight };
        assert!(resize_targets(&pair, collapsed, collapsed.x + 2, collapsed.y).is_empty());
        let before = splits(&layout, area);
        let targets = resize_targets(&layout, area, before[0].divider.x, before[1].divider.y);
        assert!(drag_targets(&cross(Axis::Vertical), area, &targets, 80, 30).is_none());
        let mut invalid = targets;
        invalid[1].path.push(false);
        assert!(drag_targets(&layout, area, &invalid, 80, 30).is_none());
    }

    #[test]
    fn hover_resolution_clips_extreme_rectangles_before_quadrant_arithmetic() {
        let layout = cross(Axis::Horizontal);
        let area = Rect {
            x: u16::MAX - 20,
            y: u16::MAX - 20,
            width: u16::MAX,
            height: u16::MAX,
        };
        let before = splits(&layout, area);
        let x = before[0].divider.x;
        let y = before[1].divider.y;
        let targets = resize_targets(&layout, area, x, y);
        assert_eq!(targets.len(), 2);
        assert!(drag_targets(&layout, area, &targets, x, y).is_none());
        let moved = drag_targets(&layout, area, &targets, u16::MAX, u16::MAX).unwrap();
        for line in resize_feedback(&moved, area, &targets) {
            assert!(line.x.checked_add(line.width).is_some());
            assert!(line.y.checked_add(line.height).is_some());
        }
    }
}
