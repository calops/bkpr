//! Sparse raised material for a rounded union of exactly two physical rectangles.
//!
//! Adapted from bkpr-zellij 18890b231bc3dd29039dd6ca141ec0af49cf332e,
//! zellij-server/src/output/recessed_frames/{joined,contour,patches,depth,strips}.rs.
//! The Zellij contributors' MIT license is reproduced in decoration.rs and
//! joined_contour.rs. All style values and semantic exclusions are caller-owned.
use crate::Rect;
use crate::decoration::{DecorationKind, DecorationStyle};
use crate::joined_contour::{Contour, Feature, Point, Rect as PixelRect, distance};
use bkpr_protocol::{Frame, Image, ImageLayer, PixelFormat, Placement};
use std::{collections::VecDeque, sync::Arc};

const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_LAYOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_LAYOUTS: usize = 16;
const MAX_PARTS: usize = 2048;

/// Physical half-open bounds [left, top, right, bottom]. Positive-area overlap
/// is required; disconnected, excessively large or unsupported requests fall
/// back atomically rather than publishing an incomplete outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinedSurface {
    pub rects_px: [[i32; 4]; 2],
    pub style: DecorationStyle,
}

/// A half-open row of cells wholly inside the rounded union. Prepared once on
/// the worker; useful for compositing opaque text backgrounds without changing
/// native glyphs, images, cursor positions or terminal geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpaqueSpan {
    pub row: u16,
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Geometry {
    Line {
        horizontal: bool,
        inward: i32,
        phase: i32,
    },
    Tile(Vec<Feature>),
}
#[derive(Clone, PartialEq, Eq)]
struct PatchKey {
    style: DecorationStyle,
    cell: [u32; 2],
    geometry: Geometry,
}
#[derive(Clone)]
struct Patch {
    key: PatchKey,
    body: Arc<Image>,
    shadow: Arc<Image>,
    body_visible: bool,
    shadow_visible: bool,
}
impl Patch {
    fn bytes(&self) -> usize {
        self.body.pixels.len() + self.shadow.pixels.len()
    }
}
#[derive(Clone)]
struct Part {
    patch: usize,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    source_x: u32,
    source_y: u32,
}

pub(crate) struct PreparedJoined {
    pub request: JoinedSurface,
    pub cell: [u32; 2],
    patches: Vec<Patch>,
    parts: Vec<Part>,
    pub spans: Vec<OpaqueSpan>,
}
impl PreparedJoined {
    /// Borrow exactly the chrome pixel resources this layout may place.
    pub(crate) fn images(&self) -> impl Iterator<Item = &Arc<Image>> {
        self.patches.iter().flat_map(|patch| {
            [
                patch.body_visible.then_some(&patch.body),
                patch.shadow_visible.then_some(&patch.shadow),
            ]
            .into_iter()
            .flatten()
        })
    }

    fn bytes(&self) -> usize {
        self.patches.iter().map(Patch::bytes).sum::<usize>()
            + self.parts.len() * std::mem::size_of::<Part>()
            + self.spans.len() * std::mem::size_of::<OpaqueSpan>()
    }

    pub fn place(&self, frame: &mut Frame, base: u32, exclusions: &[Rect]) -> bool {
        if [frame.size.cell_width, frame.size.cell_height] != self.cell
            || base.checked_add(8191).is_none()
            || exclusions.len() > 32
        {
            return false;
        }
        // Validate the bounded fragmentation before changing the caller frame.
        let fragments = |part: &Part| -> Option<([[u32; 4]; 128], usize)> {
            let mut pieces = [[0; 4]; 128];
            pieces[0] = [part.x, part.y, part.width, part.height];
            let mut count = 1;
            for rect in exclusions {
                let [cw, ch] = self.cell;
                let l = u32::from(rect.x) * cw;
                let t = u32::from(rect.y) * ch;
                let r = (u32::from(rect.x) + u32::from(rect.width)) * cw;
                let b = (u32::from(rect.y) + u32::from(rect.height)) * ch;
                let mut next = [[0; 4]; 128];
                let mut next_count = 0;
                for &[x, y, w, h] in &pieces[..count] {
                    let left = x.max(l);
                    let top = y.max(t);
                    let right = (x + w).min(r);
                    let bottom = (y + h).min(b);
                    let split = if left >= right || top >= bottom {
                        [[x, y, w, h], [0; 4], [0; 4], [0; 4]]
                    } else {
                        [
                            [x, y, w, top - y],
                            [x, bottom, w, y + h - bottom],
                            [x, top, left - x, bottom - top],
                            [right, top, x + w - right, bottom - top],
                        ]
                    };
                    for piece in split {
                        if piece[2] > 0 && piece[3] > 0 {
                            if next_count == next.len() {
                                return None;
                            }
                            next[next_count] = piece;
                            next_count += 1;
                        }
                    }
                }
                pieces = next;
                count = next_count;
            }
            Some((pieces, count))
        };
        let mut count = self.parts.len() * 2;
        for part in &self.parts {
            let Some((_, length)) = fragments(part) else {
                return false;
            };
            count += length;
            if count > 8192 {
                return false;
            }
        }
        let mut id = base;
        for part in &self.parts {
            let patch = &self.patches[part.patch];
            for (image, visible) in [
                (&patch.body, patch.body_visible),
                (&patch.shadow, patch.shadow_visible),
            ] {
                if visible && !frame.images.iter().any(|old| old.key() == image.key()) {
                    frame.images.push(Arc::clone(image));
                }
            }
            // Partial-cell surface underneath glyphs; the same contour mask
            // above glyphs only outside explicitly supplied glyph rectangles.
            let entire = [[part.x, part.y, part.width, part.height]];
            let (clipped, length) = fragments(part).unwrap();
            for (image, layer, pieces, visible) in [
                (
                    &patch.body,
                    ImageLayer::BelowText,
                    entire.as_slice(),
                    patch.body_visible,
                ),
                (
                    &patch.shadow,
                    ImageLayer::AboveText,
                    entire.as_slice(),
                    patch.shadow_visible,
                ),
                (
                    &patch.body,
                    ImageLayer::AboveText,
                    &clipped[..length],
                    patch.body_visible,
                ),
            ] {
                if !visible {
                    continue;
                }
                for &[x, y, w, h] in pieces {
                    let right = (x + w).min(u32::from(frame.size.cols) * self.cell[0]);
                    let bottom = (y + h).min(u32::from(frame.size.rows) * self.cell[1]);
                    if right <= x || bottom <= y {
                        continue;
                    }
                    let horizontal = matches!(
                        patch.key.geometry,
                        Geometry::Line {
                            horizontal: true,
                            ..
                        }
                    );
                    let vertical = matches!(
                        patch.key.geometry,
                        Geometry::Line {
                            horizontal: false,
                            ..
                        }
                    );
                    frame.placements.push(Placement {
                        image_id: image.id,
                        generation: image.generation,
                        placement_id: id,
                        col: (x / self.cell[0]) as i32,
                        row: (y / self.cell[1]) as i32,
                        offset_x: x % self.cell[0],
                        offset_y: y % self.cell[1],
                        width: right - x,
                        height: bottom - y,
                        source_x: part.source_x + if horizontal { 0 } else { x - part.x },
                        source_y: part.source_y + if vertical { 0 } else { y - part.y },
                        source_width: if horizontal { 1 } else { right - x },
                        source_height: if vertical { 1 } else { bottom - y },
                        layer,
                        z: 0,
                    });
                    id = id.saturating_add(1);
                }
            }
        }
        true
    }
}

/// Worker-only bounded LRU. Local contour keys exclude global position and
/// straight-edge length; translated/reopened/resized surfaces share immutable
/// pixels and image identities. Layouts retain at most 4 MiB, pixels 8 MiB.
#[derive(Default)]
pub(crate) struct JoinedCache {
    patches: VecDeque<Patch>,
    layouts: VecDeque<Arc<PreparedJoined>>,
    patch_bytes: usize,
    layout_bytes: usize,
}
impl JoinedCache {
    fn patch(&mut self, key: PatchKey, generation: u64, next_id: &mut u32) -> Option<&Patch> {
        if let Some(index) = self.patches.iter().position(|patch| patch.key == key) {
            let patch = self.patches.remove(index)?;
            self.patches.push_back(patch);
        } else {
            let patch = rasterize(key, generation, next_id)?;
            if patch.bytes() > MAX_PATCH_BYTES {
                return None;
            }
            while self.patch_bytes + patch.bytes() > MAX_PATCH_BYTES || self.patches.len() >= 512 {
                self.patch_bytes -= self.patches.pop_front()?.bytes();
            }
            self.patch_bytes += patch.bytes();
            self.patches.push_back(patch);
        }
        self.patches.back()
    }

    pub(crate) fn get(
        &mut self,
        request: JoinedSurface,
        cell: [u32; 2],
    ) -> Option<Arc<PreparedJoined>> {
        let index = self
            .layouts
            .iter()
            .position(|entry| entry.request == request && entry.cell == cell)?;
        let layout = self.layouts.remove(index)?;
        self.layouts.push_back(Arc::clone(&layout));
        Some(layout)
    }

    pub(crate) fn remember(&mut self, prepared: &Arc<PreparedJoined>) -> bool {
        let bytes = prepared.bytes();
        if bytes > MAX_LAYOUT_BYTES {
            return false;
        }
        if let Some(index) = self
            .layouts
            .iter()
            .position(|entry| entry.request == prepared.request && entry.cell == prepared.cell)
        {
            self.layout_bytes -= self.layouts.remove(index).unwrap().bytes();
        }
        while self.layouts.len() >= MAX_LAYOUTS || self.layout_bytes + bytes > MAX_LAYOUT_BYTES {
            self.layout_bytes -= self.layouts.pop_front().unwrap().bytes();
        }
        self.layout_bytes += bytes;
        self.layouts.push_back(Arc::clone(prepared));
        true
    }

    pub fn prepare(
        &mut self,
        request: JoinedSurface,
        cell: [u32; 2],
        generation: u64,
        next_id: &mut u32,
    ) -> Option<Arc<PreparedJoined>> {
        if let Some(layout) = self.get(request, cell) {
            return Some(layout);
        }
        if cell.contains(&0)
            || cell.iter().any(|side| *side > 512)
            || request.style.kind != DecorationKind::RaisedSurface
            || request.style.inset_px != [0, 0]
            || request.style.outset_px != [0, 0]
            || request.style.sigma_milli == 0
            || request.style.radius_px > 512
            || request
                .rects_px
                .iter()
                .flatten()
                .any(|value| value.unsigned_abs() > 1_000_000)
        {
            return None;
        }
        let rects = request.rects_px.map(|[x, y, right, bottom]| PixelRect {
            x,
            y,
            right,
            bottom,
        });
        if rects
            .iter()
            .any(|rect| rect.x >= rect.right || rect.y >= rect.bottom)
        {
            return None;
        }
        let contour = Contour::new(rects, request.style.radius_px as i32)?;
        let reach = reach(request.style)?;
        let [w, h] = cell.map(|v| v as i32);
        let bounds = contour.bounds;
        let left = (bounds.x - reach).max(0) / w;
        let top = (bounds.y - reach).max(0) / h;
        let right = ((bounds.right + reach).max(0) + w - 1) / w;
        let bottom = ((bounds.bottom + reach).max(0) + h - 1) / h;
        if right > i32::from(u16::MAX)
            || bottom > i32::from(u16::MAX)
            || i64::from(right - left) * i64::from(bottom - top) > 262_144
        {
            return None;
        }
        // Short contours can have no bare straight run yet. Prepare their
        // normal/phase strips now so lengthening an edge needs no new pixels.
        for feature in &contour.features {
            if let Feature::Edge { from, to, nx, ny } = feature {
                let horizontal = from.y == to.y;
                let geometry = Geometry::Line {
                    horizontal,
                    inward: if horizontal { *ny } else { *nx },
                    phase: if horizontal {
                        from.y.rem_euclid(h)
                    } else {
                        from.x.rem_euclid(w)
                    },
                };
                self.patch(
                    PatchKey {
                        style: request.style,
                        cell,
                        geometry,
                    },
                    generation,
                    next_id,
                )?;
            }
        }
        let mut patches: Vec<Patch> = Vec::new();
        let mut retained_bytes = 0;
        let mut parts: Vec<Part> = Vec::new();
        let mut spans = Vec::new();
        for y in top..bottom {
            let mut full_start = None;
            for x in left..right {
                let origin = Point { x: x * w, y: y * h };
                let full = contour.full_cell(PixelRect {
                    x: origin.x,
                    y: origin.y,
                    right: origin.x + w,
                    bottom: origin.y + h,
                });
                if full && full_start.is_none() {
                    full_start = Some(x);
                }
                if !full && let Some(start) = full_start.take() {
                    spans.push(OpaqueSpan {
                        row: y as u16,
                        start: start as u16,
                        end: x as u16,
                    });
                }
                let clip = PixelRect {
                    x: origin.x - reach - 1,
                    y: origin.y - reach - 1,
                    right: origin.x + w + reach + 1,
                    bottom: origin.y + h + reach + 1,
                };
                let mut features: Vec<_> = contour
                    .features
                    .iter()
                    .filter(|feature| feature.bounds().intersects(clip))
                    .map(|feature| feature.local(origin, clip))
                    .collect();
                if features.is_empty() {
                    continue;
                }
                features.sort();
                let line = match &features[..] {
                    [
                        Feature::Edge {
                            from,
                            to,
                            nx: _,
                            ny,
                        },
                    ] if from.y == to.y
                        && from.x.min(to.x) <= -reach
                        && from.x.max(to.x) >= w + reach =>
                    {
                        Some((true, *ny, from.y))
                    }
                    [
                        Feature::Edge {
                            from,
                            to,
                            nx,
                            ny: _,
                        },
                    ] if from.x == to.x
                        && from.y.min(to.y) <= -reach
                        && from.y.max(to.y) >= h + reach =>
                    {
                        Some((false, *nx, from.x))
                    }
                    _ => None,
                };
                let (geometry, source_x, source_y) = if let Some((horizontal, inward, edge)) = line
                {
                    let unit = if horizontal { h } else { w };
                    let phase = edge.rem_euclid(unit);
                    // The localized edge can lie a full cell beyond the
                    // footprint; reserve that cell on both sides of the strip.
                    let center = (((reach + 1 + unit - 1) / unit) + 1) * unit + phase;
                    let source = u32::try_from(center - edge).ok()?;
                    (
                        Geometry::Line {
                            horizontal,
                            inward,
                            phase,
                        },
                        if horizontal { 1 } else { source },
                        if horizontal { source } else { 1 },
                    )
                } else {
                    (Geometry::Tile(features), 0, 0)
                };
                let key = PatchKey {
                    style: request.style,
                    cell,
                    geometry,
                };
                let index = if let Some(index) = patches.iter().position(|patch| patch.key == key) {
                    index
                } else {
                    let patch = self.patch(key, generation, next_id)?.clone();
                    retained_bytes += patch.bytes();
                    if retained_bytes > MAX_LAYOUT_BYTES {
                        return None;
                    }
                    patches.push(patch);
                    patches.len() - 1
                };
                let patch = &patches[index];
                if source_x
                    + if line.is_some_and(|line| line.0) {
                        1
                    } else {
                        cell[0]
                    }
                    > patch.body.width
                    || source_y
                        + if line.is_some_and(|line| !line.0) {
                            1
                        } else {
                            cell[1]
                        }
                        > patch.body.height
                {
                    return None;
                }
                parts.push(Part {
                    patch: index,
                    x: origin.x as u32,
                    y: origin.y as u32,
                    width: cell[0],
                    height: cell[1],
                    source_x,
                    source_y,
                });
                if parts.len() > 65536 {
                    return None;
                }
            }
            if let Some(start) = full_start {
                spans.push(OpaqueSpan {
                    row: y as u16,
                    start: start as u16,
                    end: right as u16,
                });
            }
        }
        // Group by immutable patch/normal-axis coordinate before coalescing;
        // there is never a per-cell straight-edge placement in published output.
        parts.sort_by_key(|part| {
            if matches!(
                patches[part.patch].key.geometry,
                Geometry::Line {
                    horizontal: false,
                    ..
                }
            ) {
                (part.patch, part.x, part.source_x, part.y)
            } else {
                (part.patch, part.y, part.source_y, part.x)
            }
        });
        let mut merged: Vec<Part> = Vec::new();
        for part in parts {
            if let Some(previous) = merged.last_mut()
                && previous.patch == part.patch
            {
                match patches[part.patch].key.geometry {
                    Geometry::Line {
                        horizontal: true, ..
                    } if previous.y == part.y
                        && previous.source_y == part.source_y
                        && previous.x + previous.width == part.x =>
                    {
                        previous.width += part.width;
                        continue;
                    }
                    Geometry::Line {
                        horizontal: false, ..
                    } if previous.x == part.x
                        && previous.source_x == part.source_x
                        && previous.y + previous.height == part.y =>
                    {
                        previous.height += part.height;
                        continue;
                    }
                    _ => {}
                }
            }
            merged.push(part);
        }
        if merged.len() > MAX_PARTS {
            return None;
        }
        let prepared = Arc::new(PreparedJoined {
            request,
            cell,
            patches,
            parts: merged,
            spans,
        });
        self.remember(&prepared).then_some(prepared)
    }
}

fn reach(style: DecorationStyle) -> Option<i32> {
    let support = (f64::from(style.sigma_milli) * 0.005).ceil();
    let offset = style
        .offset_milli
        .iter()
        .map(|v| f64::from(*v).abs() / 1000.0)
        .fold(0.0, f64::max);
    let reach = (support + offset + f64::from(style.lip_width_milli) / 1000.0 + 2.0).ceil();
    (reach <= 512.0).then_some(reach as i32)
}

fn bevel(style: DecorationStyle, d: f64, nx: f64, ny: f64) -> [f64; 3] {
    let width = f64::from(style.lip_width_milli) / 1000.0;
    let weight = if width > 0.0 {
        (1.0 - d / width).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let light = ((nx + ny) * std::f64::consts::FRAC_1_SQRT_2 / nx.hypot(ny).max(f64::EPSILON))
        .clamp(-1.0, 1.0);
    std::array::from_fn(|c| {
        let base = f64::from(style.background[c]);
        if light > 0.0 {
            let highlight = f64::from(
                style.foreground[c]
                    .max(style.background[c])
                    .max(style.surface[c]),
            );
            base + (highlight - base) * f64::from(style.lip_highlight_milli) / 1000.0
                * light
                * weight
        } else {
            base * (1.0 + f64::from(style.lip_shade_milli) / 1000.0 * light * weight)
        }
    })
}

fn rasterize(key: PatchKey, generation: u64, next_id: &mut u32) -> Option<Patch> {
    let pad = reach(key.style)? as usize;
    let [cw, ch] = key.cell.map(|side| side as usize);
    let (width, height, features) = match &key.geometry {
        Geometry::Tile(features) => (cw, ch, features.clone()),
        Geometry::Line {
            horizontal,
            inward,
            phase,
        } => {
            let unit = if *horizontal { ch } else { cw };
            let support_cells = (pad + 1).div_ceil(unit);
            let normal = (2 * support_cells + 3) * unit;
            let edge = ((support_cells + 1) * unit) as i32 + phase;
            let feature = if *horizontal {
                Feature::Edge {
                    from: Point {
                        x: -1_000_000,
                        y: edge,
                    },
                    to: Point {
                        x: 1_000_000,
                        y: edge,
                    },
                    nx: 0,
                    ny: *inward,
                }
            } else {
                Feature::Edge {
                    from: Point {
                        x: edge,
                        y: -1_000_000,
                    },
                    to: Point {
                        x: edge,
                        y: 1_000_000,
                    },
                    nx: *inward,
                    ny: 0,
                }
            };
            if *horizontal {
                (1, normal, vec![feature])
            } else {
                (normal, 1, vec![feature])
            }
        }
    };
    let pw = width + 2 * pad;
    let ph = height + 2 * pad;
    if pw.checked_mul(ph)? > 1_048_576 {
        return None;
    }
    let columns = width.div_ceil(cw);
    let rows = height.div_ceil(ch);
    let mut partial_cells = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        for column in 0..columns {
            partial_cells.push(!Contour::features_full_cell(
                &features,
                PixelRect {
                    x: (column * cw) as i32,
                    y: (row * ch) as i32,
                    right: ((column + 1) * cw) as i32,
                    bottom: ((row + 1) * ch) as i32,
                },
            ));
        }
    }
    // The exterior shadow is zero where complete cells have coverage one.
    let shadow_samples = if key.style.shadow_opacity_milli != 0
        && partial_cells.iter().any(|partial| *partial)
    {
        let (scan_width, scan_height) = match key.geometry {
            Geometry::Line {
                horizontal: true, ..
            } => (1, ph),
            Geometry::Line {
                horizontal: false, ..
            } => (pw, 1),
            Geometry::Tile(_) => (pw, ph),
        };
        // Straight features are exactly invariant along their tangent. Repeat the
        // result, preserving the existing kernel and floating-point summation order.
        let repeat_axis = |values: &mut [f64]| {
            if scan_width == 1 {
                for row in values.chunks_exact_mut(pw) {
                    row.fill(row[0]);
                }
            } else if scan_height == 1 {
                for y in 1..ph {
                    values.copy_within(..pw, y * pw);
                }
            }
        };
        let mut mask = vec![0.0; pw * ph];
        for y in 0..scan_height {
            for x in 0..scan_width {
                let pixel = PixelRect {
                    x: x as i32 - pad as i32,
                    y: y as i32 - pad as i32,
                    right: x as i32 - pad as i32 + 1,
                    bottom: y as i32 - pad as i32 + 1,
                };
                if let Some(inside) = Contour::pixel_coverage(&features, pixel) {
                    mask[y * pw + x] = f64::from(u8::from(inside));
                    continue;
                }
                let mut inside = 0.0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        if distance(
                            &features,
                            x as f64 - pad as f64 + (sx as f64 + 0.5) / 4.0,
                            y as f64 - pad as f64 + (sy as f64 + 0.5) / 4.0,
                        )
                        .0 >= 0.0
                        {
                            inside += 1.0;
                        }
                    }
                }
                mask[y * pw + x] = inside / 16.0;
            }
        }
        repeat_axis(&mut mask);
        let sigma = f64::from(key.style.sigma_milli) / 1000.0;
        let support = (sigma * 5.0).ceil() as isize;
        let mut kernel: Vec<_> = (-support..=support)
            .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
            .collect();
        let total: f64 = kernel.iter().sum();
        for weight in &mut kernel {
            *weight /= total;
        }
        let mut horizontal = vec![0.0; mask.len()];
        let mut blurred = vec![0.0; mask.len()];
        for y in 0..scan_height {
            for x in 0..scan_width {
                let mut sum = 0.0;
                for (i, weight) in kernel.iter().enumerate() {
                    let px = (x as isize + i as isize - support).clamp(0, pw as isize - 1) as usize;
                    sum += mask[y * pw + px] * weight;
                }
                horizontal[y * pw + x] = sum;
            }
        }
        repeat_axis(&mut horizontal);
        for y in 0..scan_height {
            for x in 0..scan_width {
                let mut sum = 0.0;
                for (i, weight) in kernel.iter().enumerate() {
                    let py = (y as isize + i as isize - support).clamp(0, ph as isize - 1) as usize;
                    sum += horizontal[py * pw + x] * weight;
                }
                blurred[y * pw + x] = sum;
            }
        }
        repeat_axis(&mut blurred);
        Some((mask, blurred))
    } else {
        None
    };
    // A terminal may linearly filter beyond a one-texel texture edge. Crop
    // the middle of three identical texels instead of fading into that border.
    // Rasterize the constant axis once; only duplicate its final RGBA bytes.
    let (repeat_x, repeat_y) = match key.geometry {
        Geometry::Line {
            horizontal: true, ..
        } => (3, 1),
        Geometry::Line {
            horizontal: false, ..
        } => (1, 3),
        Geometry::Tile(_) => (1, 1),
    };
    let bytes = width * height * 4;
    let mut body = Vec::with_capacity(bytes * repeat_x * repeat_y);
    let mut shadow = Vec::with_capacity(bytes * repeat_x * repeat_y);
    let mut body_visible = false;
    let mut shadow_visible = false;
    for y in 0..height {
        for x in 0..width {
            let partial = partial_cells[(y / ch) * columns + x / cw];
            let mut rgba = [0.0; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let (d, nx, ny) = distance(
                        &features,
                        x as f64 + (sx as f64 + 0.5) / 4.0,
                        y as f64 + (sy as f64 + 0.5) / 4.0,
                    );
                    if d >= 0.0 && (partial || d < f64::from(key.style.lip_width_milli) / 1000.0) {
                        let rgb = bevel(key.style, d, nx, ny);
                        for c in 0..3 {
                            rgba[c] += rgb[c];
                        }
                        rgba[3] += 1.0;
                    }
                }
            }
            let pixel = if rgba[3] > 0.0 {
                [
                    (rgba[0] / rgba[3]).round() as u8,
                    (rgba[1] / rgba[3]).round() as u8,
                    (rgba[2] / rgba[3]).round() as u8,
                    (rgba[3] * 255.0 / 16.0).round() as u8,
                ]
            } else {
                [0; 4]
            };
            body_visible |= pixel[3] != 0;
            for _ in 0..repeat_x {
                body.extend_from_slice(&pixel);
            }
            let alpha = if let Some((mask, blurred)) = &shadow_samples {
                let fx = (x as f64 + pad as f64 - f64::from(key.style.offset_milli[0]) / 1000.0)
                    .clamp(0.0, (pw - 2) as f64);
                let fy = (y as f64 + pad as f64 - f64::from(key.style.offset_milli[1]) / 1000.0)
                    .clamp(0.0, (ph - 2) as f64);
                let ix = fx.floor() as usize;
                let iy = fy.floor() as usize;
                let dx = fx - ix as f64;
                let dy = fy - iy as f64;
                let a = blurred[iy * pw + ix] * (1.0 - dx) + blurred[iy * pw + ix + 1] * dx;
                let b =
                    blurred[(iy + 1) * pw + ix] * (1.0 - dx) + blurred[(iy + 1) * pw + ix + 1] * dx;
                ((a * (1.0 - dy) + b * dy)
                    * (1.0 - mask[(y + pad) * pw + x + pad])
                    * f64::from(key.style.shadow_opacity_milli)
                    / 1000.0
                    * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            } else {
                0
            };
            shadow_visible |= alpha != 0;
            for _ in 0..repeat_x {
                shadow.extend_from_slice(&[0, 0, 0, alpha]);
            }
        }
    }
    for _ in 1..repeat_y {
        body.extend_from_within(..bytes);
        shadow.extend_from_within(..bytes);
    }
    let id = *next_id;
    *next_id = next_id.checked_add(2)?;
    let image = |id, pixels| {
        Arc::new(Image {
            id,
            generation,
            width: (width * repeat_x) as u32,
            height: (height * repeat_y) as u32,
            format: PixelFormat::Rgba,
            pixels,
        })
    };
    Some(Patch {
        key,
        body: image(id, body),
        shadow: image(id + 1, shadow),
        body_visible,
        shadow_visible,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::Size;

    fn request() -> JoinedSurface {
        JoinedSurface {
            rects_px: [[60, 16, 180, 112], [20, 48, 66, 64]],
            style: DecorationStyle {
                kind: DecorationKind::RaisedSurface,
                background: [70, 80, 90],
                surface: [90, 100, 110],
                foreground: [230; 3],
                inset_px: [0, 0],
                outset_px: [0, 0],
                radius_px: 6,
                sigma_milli: 1200,
                offset_milli: [1400; 2],
                shadow_opacity_milli: 450,
                lip_width_milli: 750,
                lip_highlight_milli: 240,
                lip_shade_milli: 240,
            },
        }
    }

    #[test]
    fn certified_pixels_preserve_subpixel_coverage_at_convex_and_concave_corners() {
        let request = request();
        let pad = reach(request.style).unwrap();
        let mut cache = JoinedCache::default();
        let mut next_id = 1;
        for cell in [[8, 16], [9, 18]] {
            let prepared = cache.prepare(request, cell, 1, &mut next_id).unwrap();
            for patch in &prepared.patches {
                let Geometry::Tile(features) = &patch.key.geometry else {
                    continue;
                };
                for y in -pad..cell[1] as i32 + pad {
                    for x in -pad..cell[0] as i32 + pad {
                        let rect = PixelRect {
                            x,
                            y,
                            right: x + 1,
                            bottom: y + 1,
                        };
                        let Some(covered) = Contour::pixel_coverage(features, rect) else {
                            continue;
                        };
                        for dy in [0.125, 0.375, 0.625, 0.875] {
                            for dx in [0.125, 0.375, 0.625, 0.875] {
                                assert_eq!(
                                    distance(features, f64::from(x) + dx, f64::from(y) + dy).0
                                        >= 0.0,
                                    covered,
                                    "cell={cell:?}, pixel={x},{y}, sample={dx},{dy}, features={features:?}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn straight_material_matches_a_two_dimensional_cell_on_both_axes() {
        let style = request().style;
        let pad = reach(style).unwrap() as u32;
        for cell in [[8, 16], [9, 18]] {
            for horizontal in [false, true] {
                let unit = if horizontal { cell[1] } else { cell[0] };
                let start = ((pad + 1).div_ceil(unit) + 1) * unit;
                for inward in [-1, 1] {
                    for phase in [0, unit as i32 / 2] {
                        let mut next_id = 1;
                        let line = rasterize(
                            PatchKey {
                                style,
                                cell,
                                geometry: Geometry::Line {
                                    horizontal,
                                    inward,
                                    phase,
                                },
                            },
                            1,
                            &mut next_id,
                        )
                        .unwrap();
                        let feature = if horizontal {
                            Feature::Edge {
                                from: Point {
                                    x: -1_000_000,
                                    y: phase,
                                },
                                to: Point {
                                    x: 1_000_000,
                                    y: phase,
                                },
                                nx: 0,
                                ny: inward,
                            }
                        } else {
                            Feature::Edge {
                                from: Point {
                                    x: phase,
                                    y: -1_000_000,
                                },
                                to: Point {
                                    x: phase,
                                    y: 1_000_000,
                                },
                                nx: inward,
                                ny: 0,
                            }
                        };
                        let tile = rasterize(
                            PatchKey {
                                style,
                                cell,
                                geometry: Geometry::Tile(vec![feature]),
                            },
                            1,
                            &mut next_id,
                        )
                        .unwrap();
                        for (line, tile) in [(&line.body, &tile.body), (&line.shadow, &tile.shadow)]
                        {
                            for y in 0..cell[1] {
                                for x in 0..cell[0] {
                                    let (sx, sy) = if horizontal {
                                        (1, start + y)
                                    } else {
                                        (start + x, 1)
                                    };
                                    let a = ((sy * line.width + sx) * 4) as usize;
                                    let b = ((y * tile.width + x) * 4) as usize;
                                    assert_eq!(
                                        line.pixels[a..a + 4],
                                        tile.pixels[b..b + 4],
                                        "{cell:?}, horizontal={horizontal}, inward={inward}, phase={phase}, pixel={x},{y}",
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn contour(request: JoinedSurface) -> Contour {
        Contour::new(
            request.rects_px.map(|[x, y, right, bottom]| PixelRect {
                x,
                y,
                right,
                bottom,
            }),
            request.style.radius_px as i32,
        )
        .unwrap()
    }

    fn pixel(prepared: &PreparedJoined, x: u32, y: u32, shadow: bool) -> [u8; 4] {
        let part = prepared
            .parts
            .iter()
            .find(|part| {
                x >= part.x && x < part.x + part.width && y >= part.y && y < part.y + part.height
            })
            .unwrap();
        let patch = &prepared.patches[part.patch];
        let image = if shadow { &patch.shadow } else { &patch.body };
        let sx = part.source_x
            + if matches!(
                patch.key.geometry,
                Geometry::Line {
                    horizontal: true,
                    ..
                }
            ) {
                0
            } else {
                x - part.x
            };
        let sy = part.source_y
            + if matches!(
                patch.key.geometry,
                Geometry::Line {
                    horizontal: false,
                    ..
                }
            ) {
                0
            } else {
                y - part.y
            };
        let offset = ((sy * image.width + sx) * 4) as usize;
        image.pixels[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn one_contour_has_both_joining_arcs_and_no_internal_attachment_edge() {
        let contour = contour(request());
        let concave = contour
            .features
            .iter()
            .filter(|feature| matches!(feature, Feature::Arc { convex: false, .. }))
            .count();
        let convex = contour
            .features
            .iter()
            .filter(|feature| matches!(feature, Feature::Arc { convex: true, .. }))
            .count();
        assert_eq!((concave, convex), (2, 6));
        assert!(distance(&contour.features, 60.0, 56.0).0 > 6.0);
        assert!(distance(&contour.features, 59.0, 65.0).0 > 0.0);
        assert!(distance(&contour.features, 59.0, 47.0).0 > 0.0);
        assert!(distance(&contour.features, 56.0, 68.0).0 < 0.0);
        assert!(distance(&contour.features, 56.0, 44.0).0 < 0.0);
        assert!(!contour.full_cell(PixelRect {
            x: 56,
            y: 64,
            right: 60,
            bottom: 72
        }));
        assert!(contour.full_cell(PixelRect {
            x: 60,
            y: 48,
            right: 64,
            bottom: 56
        }));
    }

    #[test]
    fn complete_top_left_and_directional_shadow_are_rasterized() {
        let mut cache = JoinedCache::default();
        let prepared = cache.prepare(request(), [4, 8], 3, &mut 10).unwrap();
        assert!(pixel(&prepared, 100, 16, false)[3] > 0);
        assert!(pixel(&prepared, 60, 88, false)[3] > 0);
        assert!(pixel(&prepared, 20, 56, false)[3] > 0);
        assert!(pixel(&prepared, 181, 88, true)[3] > pixel(&prepared, 58, 88, true)[3]);
        assert!(pixel(&prepared, 100, 113, true)[3] > pixel(&prepared, 100, 14, true)[3]);
        let upper = pixel(&prepared, 100, 16, false);
        let lower = pixel(&prepared, 100, 111, false);
        assert!(upper[0] > lower[0]);
    }

    #[test]
    fn movement_reopening_and_resizing_share_pixels_and_merge_straight_edges() {
        let mut cache = JoinedCache::default();
        let mut next = 10;
        let original = cache.prepare(request(), [4, 8], 3, &mut next).unwrap();
        let after_first = next;
        let mut moved = request();
        for rect in &mut moved.rects_px {
            rect[0] += 8;
            rect[2] += 8;
            rect[1] += 16;
            rect[3] += 16;
        }
        let translated = cache.prepare(moved, [4, 8], 3, &mut next).unwrap();
        assert_eq!(next, after_first);
        assert!(translated.patches.iter().all(|patch| {
            original
                .patches
                .iter()
                .any(|old| Arc::ptr_eq(&old.body, &patch.body))
        }));
        let reopened = cache.prepare(request(), [4, 8], 3, &mut next).unwrap();
        assert!(Arc::ptr_eq(&original, &reopened));
        let mut resized = request();
        resized.rects_px[0][2] += 80;
        resized.rects_px[0][3] += 32;
        let resized = cache.prepare(resized, [4, 8], 3, &mut next).unwrap();
        assert_eq!(next, after_first);
        assert!(resized.parts.iter().any(|part| part.width > 40));
        assert!(resized.parts.iter().any(|part| part.height > 16));
        for step in 1..=20 {
            let mut shifted = request();
            for rect in &mut shifted.rects_px {
                rect[0] += 4 * step;
                rect[2] += 4 * step;
                rect[1] += 8 * step;
                rect[3] += 8 * step;
            }
            cache.prepare(shifted, [4, 8], 3, &mut next).unwrap();
        }
        assert_eq!(next, after_first);
        assert_eq!(cache.layouts.len(), MAX_LAYOUTS);
        assert!(cache.patch_bytes <= MAX_PATCH_BYTES);
        assert!(cache.layout_bytes <= MAX_LAYOUT_BYTES);
    }

    #[test]
    fn cell_spans_and_glyph_cutouts_preserve_canonical_content() {
        let mut cache = JoinedCache::default();
        let prepared = cache.prepare(request(), [4, 8], 3, &mut 10).unwrap();
        let mut frame = crate::blank(Size {
            cols: 60,
            rows: 20,
            cell_width: 4,
            cell_height: 8,
        });
        crate::text(&mut frame, 5, 7, "native glyphs", false);
        frame.cursor = bkpr_protocol::Cursor {
            x: 8,
            y: 7,
            visible: true,
            blinking: true,
            shape: bkpr_protocol::CursorShape::Bar,
            color: Some([240; 3]),
        };
        let native = Arc::new(Image {
            id: 1,
            generation: 99,
            width: 4,
            height: 8,
            format: PixelFormat::Rgba,
            pixels: vec![255; 4 * 8 * 4],
        });
        frame.images.push(Arc::clone(&native));
        let native_placement = Placement {
            image_id: 1,
            generation: 99,
            placement_id: 1,
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            width: 4,
            height: 8,
            source_x: 0,
            source_y: 0,
            source_width: 4,
            source_height: 8,
            layer: ImageLayer::AboveText,
            z: 0,
        };
        frame.placements.push(native_placement.clone());
        let rows = frame.rows.clone();
        let cursor = frame.cursor.clone();
        let glyphs = Rect {
            x: 5,
            y: 6,
            width: 11,
            height: 2,
        };
        assert!(prepared.place(&mut frame, 100, &[glyphs]));
        assert_eq!(frame.rows, rows);
        assert_eq!(frame.cursor, cursor);
        assert!(Arc::ptr_eq(&frame.images[0], &native));
        assert_eq!(frame.placements[0], native_placement);
        assert!(
            frame
                .placements
                .iter()
                .filter(|placement| placement.layer == ImageLayer::AboveText
                    && placement.image_id % 2 == 0)
                .all(|placement| {
                    let l = placement.col as u32 * 4 + placement.offset_x;
                    let t = placement.row as u32 * 8 + placement.offset_y;
                    l >= 64 || l + placement.width <= 20 || t >= 64 || t + placement.height <= 48
                })
        );
        let contour = contour(request());
        assert!(
            prepared
                .spans
                .iter()
                .all(
                    |span| (span.start..span.end).all(|x| contour.full_cell(PixelRect {
                        x: i32::from(x) * 4,
                        y: i32::from(span.row) * 8,
                        right: i32::from(x + 1) * 4,
                        bottom: i32::from(span.row + 1) * 8,
                    }))
                )
        );
        let before = frame.placements.clone();
        assert!(!prepared.place(&mut frame, u32::MAX, &[]));
        assert_eq!(frame.placements, before);
    }

    #[test]
    fn unsupported_or_disconnected_geometry_never_publishes_partial_material() {
        let mut cache = JoinedCache::default();
        let mut invalid = request();
        invalid.rects_px[1] = [0, 0, 8, 8];
        assert!(cache.prepare(invalid, [4, 8], 3, &mut 10).is_none());
        invalid = request();
        invalid.rects_px[0][0] = i32::MIN;
        assert!(cache.prepare(invalid, [4, 8], 3, &mut 10).is_none());
        invalid = request();
        invalid.style.sigma_milli = u32::MAX;
        assert!(cache.prepare(invalid, [4, 8], 3, &mut 10).is_none());
    }
}
