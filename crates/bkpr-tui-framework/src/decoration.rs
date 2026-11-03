//! Sparse rounded materials. Geometry and every material choice belong to the caller.
//!
//! The continuous rounded-mask convolution and supersampling are adapted from
//! bkpr-zellij d7b40a37a63d4934054516f6f445488348075a62, files
//! zellij-server/src/output/recessed_frames/{depth,raster,raised}.rs.
//! This engine runs on the existing graphics worker, not a second compositor.
//!
//! MIT License
//! Copyright (c) 2020 Zellij contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
use crate::Rect;
use bkpr_protocol::{Frame, Image, ImageLayer, Placement};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationKind {
    Recessed,
    Raised,
    /// Opaque rounded surface plus the same outer shadow as `Raised`.
    RaisedSurface,
}

/// Physical-pixel material, with millipixels for fractional blur/lighting values.
/// No radius, blur or offset is silently reduced to fit a cell. Unsupported
/// geometry/budgets return the caller to its text fallback instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationStyle {
    pub kind: DecorationKind,
    pub background: [u8; 3],
    pub surface: [u8; 3],
    pub foreground: [u8; 3],
    pub inset_px: [u32; 2],
    /// Output padding around the supplied logical rectangle, in physical pixels.
    pub outset_px: [u32; 2],
    pub radius_px: u32,
    pub sigma_milli: u32,
    pub offset_milli: [i32; 2],
    pub shadow_opacity_milli: u16,
    pub lip_width_milli: u32,
    pub lip_highlight_milli: u16,
    pub lip_shade_milli: u16,
}

/// Absolute physical-pixel bounds and a half-open ownership clip.
/// `bounds_px` is `[left, top, right, bottom]` before material outset; signed
/// edges allow translating a cached contour beyond its logical cell rectangle.
/// `clip_px` uses the same edge order and is intersected with the screen.
/// Callers may place disjoint clips with different bounds for partial neighbors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationGeometry {
    pub bounds_px: [i64; 4],
    pub clip_px: [u32; 4],
}

/// One 3x3 atlas supplies eight sparse fragments; its center is never placed.
/// Tiles cover the complete requested curve and five-sigma shadow support.
#[derive(Clone)]
pub(crate) struct PreparedDecoration {
    pub style: DecorationStyle,
    pub cell: [u32; 2],
    pub image: Arc<Image>,
}
impl PreparedDecoration {
    pub fn place(&self, frame: &mut Frame, rect: Rect, base: u32) -> bool {
        self.place_clipped(frame, rect, base, &[])
    }

    pub fn place_clipped(
        &self,
        frame: &mut Frame,
        rect: Rect,
        base: u32,
        exclusions: &[Rect],
    ) -> bool {
        let [cw, ch] = self.cell.map(i64::from);
        let geometry = DecorationGeometry {
            bounds_px: [
                i64::from(rect.x) * cw,
                i64::from(rect.y) * ch,
                (i64::from(rect.x) + i64::from(rect.width)) * cw,
                (i64::from(rect.y) + i64::from(rect.height)) * ch,
            ],
            clip_px: [
                0,
                0,
                u32::from(frame.size.cols) * self.cell[0],
                u32::from(frame.size.rows) * self.cell[1],
            ],
        };
        self.place_with_geometry(frame, rect, base, exclusions, geometry)
    }

    pub fn place_with_geometry(
        &self,
        frame: &mut Frame,
        rect: Rect,
        base: u32,
        exclusions: &[Rect],
        geometry: DecorationGeometry,
    ) -> bool {
        let size = frame.size;
        let [cw, ch] = self.cell;
        let tw = self.image.width / 3;
        let th = self.image.height / 3;
        let [ox, oy] = self.style.outset_px;
        let [left, top, right, bottom] = geometry.bounds_px;
        let Some(width) = right
            .checked_sub(left)
            .and_then(|width| u32::try_from(width).ok())
            .and_then(|width| ox.checked_mul(2).and_then(|pad| width.checked_add(pad)))
        else {
            return false;
        };
        let Some(height) = bottom
            .checked_sub(top)
            .and_then(|height| u32::try_from(height).ok())
            .and_then(|height| oy.checked_mul(2).and_then(|pad| height.checked_add(pad)))
        else {
            return false;
        };
        let Some(origin_x) = left.checked_sub(i64::from(ox)) else {
            return false;
        };
        let Some(origin_y) = top.checked_sub(i64::from(oy)) else {
            return false;
        };
        if rect.width == 0
            || rect.height == 0
            || geometry
                .bounds_px
                .iter()
                .any(|edge| *edge < -i64::from(u32::MAX) || *edge > i64::from(u32::MAX))
            || width == 0
            || height == 0
            || origin_x.checked_add(i64::from(width)).is_none()
            || origin_y.checked_add(i64::from(height)).is_none()
            || u64::from(width)
                < 2 * (u64::from(self.style.inset_px[0]) + u64::from(self.style.radius_px))
            || u64::from(height)
                < 2 * (u64::from(self.style.inset_px[1]) + u64::from(self.style.radius_px))
            || u32::from(rect.x) + u32::from(rect.width) > u32::from(size.cols)
            || u32::from(rect.y) + u32::from(rect.height) > u32::from(size.rows)
            || [size.cell_width, size.cell_height] != self.cell
            || base.checked_add(63).is_none()
        {
            return false;
        }
        let clip_left = i64::from(geometry.clip_px[0]);
        let clip_top = i64::from(geometry.clip_px[1]);
        let clip_right = i64::from(geometry.clip_px[2]).min(i64::from(size.cols) * i64::from(cw));
        let clip_bottom = i64::from(geometry.clip_px[3]).min(i64::from(size.rows) * i64::from(ch));
        if clip_left >= clip_right || clip_top >= clip_bottom {
            return true;
        }
        // Opposing blur supports may meet in a small window. Crop at their
        // midpoint, never alter the requested radius/blur as the old atlas did.
        let ew = tw.min(width / 2);
        let eh = th.min(height / 2);
        let parts = [
            (0, 0, 0, 0, ew, eh),
            (ew, 0, tw, 0, width - 2 * ew, eh),
            (width - ew, 0, 3 * tw - ew, 0, ew, eh),
            (0, eh, 0, th, ew, height - 2 * eh),
            (width - ew, eh, 3 * tw - ew, th, ew, height - 2 * eh),
            (0, height - eh, 0, 3 * th - eh, ew, eh),
            (ew, height - eh, tw, 3 * th - eh, width - 2 * ew, eh),
            (width - ew, height - eh, 3 * tw - ew, 3 * th - eh, ew, eh),
        ];
        // A bounded stack buffer keeps semantic caption cutouts allocation-free.
        // Source strips are constant along their stretched axis, so cuts retain
        // exact pixel coordinates without scaling an entire window-sized image.
        let mut fragments = [[0_u32; 8]; 64];
        let mut count = 0;
        for (index, (x, y, sx, sy, width, height)) in parts.into_iter().enumerate() {
            let left = (origin_x + i64::from(x)).max(clip_left);
            let top = (origin_y + i64::from(y)).max(clip_top);
            let right = (origin_x + i64::from(x + width)).min(clip_right);
            let bottom = (origin_y + i64::from(y + height)).min(clip_bottom);
            if left >= right || top >= bottom {
                continue;
            }
            let dx = (left - origin_x - i64::from(x)) as u32;
            let dy = (top - origin_y - i64::from(y)) as u32;
            let w = (right - left) as u32;
            let h = (bottom - top) as u32;
            let horizontal = index == 1 || index == 6;
            let vertical = index == 3 || index == 4;
            fragments[count] = [
                x + dx,
                y + dy,
                sx + if horizontal { 0 } else { dx },
                sy + if vertical { 0 } else { dy },
                w,
                h,
                if horizontal { 1 } else { w },
                if vertical { 1 } else { h },
            ];
            count += 1;
        }
        for exclusion in exclusions {
            let left = (i64::from(exclusion.x) * i64::from(cw) - origin_x)
                .clamp(0, i64::from(width)) as u32;
            let top = (i64::from(exclusion.y) * i64::from(ch) - origin_y)
                .clamp(0, i64::from(height)) as u32;
            let right = ((i64::from(exclusion.x) + i64::from(exclusion.width)) * i64::from(cw)
                - origin_x)
                .clamp(0, i64::from(width)) as u32;
            let bottom = ((i64::from(exclusion.y) + i64::from(exclusion.height)) * i64::from(ch)
                - origin_y)
                .clamp(0, i64::from(height)) as u32;
            if left >= right || top >= bottom {
                continue;
            }
            let mut next = [[0_u32; 8]; 64];
            let mut next_count = 0;
            for fragment in &fragments[..count] {
                let [x, y, sx, sy, w, h, sw, sh] = *fragment;
                let l = left.max(x);
                let r = right.min(x + w);
                let t = top.max(y);
                let b = bottom.min(y + h);
                let pieces = if l < r && t < b {
                    [
                        (x, y, w, t - y),
                        (x, b, w, y + h - b),
                        (x, t, l - x, b - t),
                        (r, t, x + w - r, b - t),
                    ]
                } else {
                    [(x, y, w, h), (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0)]
                };
                for (px, py, pw, ph) in pieces {
                    if pw == 0 || ph == 0 {
                        continue;
                    }
                    if next_count == next.len() {
                        return false;
                    }
                    next[next_count] = [
                        px,
                        py,
                        sx + if sw == 1 { 0 } else { px - x },
                        sy + if sh == 1 { 0 } else { py - y },
                        pw,
                        ph,
                        if sw == 1 { 1 } else { pw },
                        if sh == 1 { 1 } else { ph },
                    ];
                    next_count += 1;
                }
            }
            fragments = next;
            count = next_count;
        }
        if !frame
            .images
            .iter()
            .any(|image| image.key() == self.image.key())
        {
            frame.images.push(Arc::clone(&self.image));
        }
        for (index, &[x, y, sx, sy, width, height, source_width, source_height]) in
            fragments[..count].iter().enumerate()
        {
            let x = (origin_x + i64::from(x)) as u32;
            let y = (origin_y + i64::from(y)) as u32;
            frame.placements.push(Placement {
                image_id: self.image.id,
                generation: self.image.generation,
                placement_id: base + index as u32,
                col: (x / cw) as i32,
                row: (y / ch) as i32,
                offset_x: x % cw,
                offset_y: y % ch,
                width,
                height,
                source_x: sx,
                source_y: sy,
                source_width,
                source_height,
                layer: ImageLayer::AboveText,
                z: 0,
            });
        }
        true
    }
}

pub(crate) fn atlas_size(style: DecorationStyle, cell: [u32; 2]) -> Option<[u32; 2]> {
    if cell.contains(&0) || cell.iter().any(|&side| side > 512) || style.sigma_milli == 0 {
        return None;
    }
    let sigma = f64::from(style.sigma_milli) / 1000.0;
    let support = 5.0 * sigma;
    let axis = |index: usize| {
        let extent = f64::from(style.inset_px[index])
            + f64::from(style.radius_px)
            + f64::from(style.offset_milli[index]).abs() / 1000.0
            + support
            + 1.0;
        if extent > f64::from(u32::MAX / 3) {
            return None;
        }
        (extent.ceil() as u32)
            .div_ceil(cell[index])
            .checked_mul(cell[index])?
            .checked_mul(3)
    };
    Some([axis(0)?, axis(1)?])
}

struct Material {
    style: DecorationStyle,
    tile: [u32; 2],
    cell: [u32; 2],
}
impl Material {
    fn local(&self, tile: usize, mut x: f64, mut y: f64) -> (f64, f64) {
        if matches!(tile, 2 | 4 | 7) {
            x = f64::from(self.tile[0]) - x;
        }
        if matches!(tile, 5..=7) {
            y = f64::from(self.tile[1]) - y;
        }
        (x, y)
    }
    fn distance(&self, tile: usize, x: f64, y: f64) -> f64 {
        let (x, y) = self.local(tile, x, y);
        let [ix, iy] = self.style.inset_px.map(f64::from);
        match tile {
            1 | 6 => y - iy,
            3 | 4 => x - ix,
            _ => {
                let radius = f64::from(self.style.radius_px);
                let qx = ix + radius - x;
                let qy = iy + radius - y;
                radius - qx.max(0.0).hypot(qy.max(0.0)) - qx.max(qy).min(0.0)
            }
        }
    }
    fn blurred_outside(&self, tile: usize, x: f64, y: f64) -> f64 {
        let x = x - f64::from(self.style.offset_milli[0]) / 1000.0;
        let y = y - f64::from(self.style.offset_milli[1]) / 1000.0;
        let sigma = f64::from(self.style.sigma_milli) / 1000.0;
        let support = sigma * 5.0;
        let distance = self.distance(tile, x, y);
        if distance >= support {
            return 0.0;
        }
        if distance <= -support {
            return 1.0;
        }
        let (x, y) = self.local(tile, x, y);
        let [ix, iy] = self.style.inset_px.map(f64::from);
        let radius = f64::from(self.style.radius_px);
        let outside = match tile {
            1 | 6 => normal_cdf((iy - y) / sigma),
            3 | 4 => normal_cdf((ix - x) / sigma),
            _ if x >= ix + radius + support => normal_cdf((iy - y) / sigma),
            _ if y >= iy + radius + support => normal_cdf((ix - x) / sigma),
            _ => {
                let low = iy.max(y - support);
                let high = y + support;
                if high <= low {
                    return 1.0;
                }
                let step = (high - low) / 64.0;
                let mut inside = 0.0;
                for index in 0..64 {
                    let sample_y = low + (index as f64 + 0.5) * step;
                    let dy = (iy + radius - sample_y).max(0.0);
                    let boundary_x = ix + radius - (radius * radius - dy * dy).max(0.0).sqrt();
                    let z = (sample_y - y) / sigma;
                    let density =
                        (-0.5 * z * z).exp() / (sigma * (2.0 * std::f64::consts::PI).sqrt());
                    inside += density * normal_cdf((x - boundary_x) / sigma) * step;
                }
                1.0 - inside
            }
        };
        outside.clamp(0.0, 1.0)
    }
    fn bevel(&self, tile: usize, x: f64, y: f64, distance: f64) -> [f64; 3] {
        let raised = self.style.kind == DecorationKind::RaisedSurface;
        let base = if raised {
            self.style.background
        } else {
            self.style.surface
        };
        let width = f64::from(self.style.lip_width_milli) / 1000.0;
        let signed_distance = if raised { -distance } else { distance };
        let weight = if width == 0.0 {
            0.0
        } else {
            (1.0 + signed_distance / width).clamp(0.0, 1.0)
        };
        if weight == 0.0 {
            return base.map(f64::from);
        }
        let nx = self.distance(tile, x + 0.25, y) - self.distance(tile, x - 0.25, y);
        let ny = self.distance(tile, x, y + 0.25) - self.distance(tile, x, y - 0.25);
        let orientation = if raised { 1.0 } else { -1.0 };
        let light = (orientation * (nx + ny) * std::f64::consts::FRAC_1_SQRT_2
            / nx.hypot(ny).max(f64::EPSILON))
        .clamp(-1.0, 1.0);
        std::array::from_fn(|c| {
            let surface = f64::from(base[c]);
            if light > 0.0 {
                let highlight = f64::from(
                    self.style.foreground[c]
                        .max(self.style.background[c])
                        .max(base[c]),
                );
                surface
                    + (highlight - surface) * f64::from(self.style.lip_highlight_milli) / 1000.0
                        * light
                        * weight
            } else {
                surface * (1.0 + f64::from(self.style.lip_shade_milli) / 1000.0 * light * weight)
            }
        })
    }

    fn sample(&self, tile: usize, x: f64, y: f64) -> [f64; 4] {
        let distance = self.distance(tile, x, y);
        let (lx, ly) = self.local(tile, x, y);
        let gutter = match tile {
            1 | 6 => ly < f64::from(self.cell[1]),
            3 | 4 => lx < f64::from(self.cell[0]),
            _ => lx < f64::from(self.cell[0]) || ly < f64::from(self.cell[1]),
        };
        let opacity = f64::from(self.style.shadow_opacity_milli) / 1000.0;
        match self.style.kind {
            DecorationKind::Raised | DecorationKind::RaisedSurface => {
                if distance < 0.0 {
                    [
                        0.0,
                        0.0,
                        0.0,
                        (1.0 - self.blurred_outside(tile, x, y)) * opacity * 255.0,
                    ]
                } else if self.style.kind == DecorationKind::RaisedSurface {
                    let [r, g, b] = self.bevel(tile, x, y, distance);
                    [r, g, b, 255.0]
                } else {
                    [0.0; 4]
                }
            }
            DecorationKind::Recessed if distance >= 0.0 => {
                let shadow = opacity * self.blurred_outside(tile, x, y);
                if gutter {
                    let [r, g, b] = self.style.background.map(|c| f64::from(c) * (1.0 - shadow));
                    [r, g, b, 255.0]
                } else {
                    [0.0, 0.0, 0.0, shadow * 255.0]
                }
            }
            // Rounded corners may reach into native content cells. Their
            // outside mask stays opaque, as in the source recessed material.
            DecorationKind::Recessed => {
                let [r, g, b] = self.bevel(tile, x, y, distance);
                [r, g, b, 255.0]
            }
        }
    }
}

// Standard normal CDF; absolute error below 8e-8, as in the source material.
fn normal_cdf(value: f64) -> f64 {
    let x = value.abs();
    let t = 1.0 / (1.0 + 0.2316419 * x);
    let tail = (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt()
        * t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if value >= 0.0 { 1.0 - tail } else { tail }
}

/// Worker-only rasterization with 4x4 premultiplied supersampling. Straight
/// strips reuse their constant axis; no full-window transparent buffer exists.
pub(crate) fn rasterize(style: DecorationStyle, cell: [u32; 2]) -> Option<Vec<u8>> {
    let [width, height] = atlas_size(style, cell)?;
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if bytes > 8 * 1024 * 1024 {
        return None;
    }
    let tile = [width / 3, height / 3];
    let material = Material { style, tile, cell };
    let mut pixels = vec![0; bytes];
    for (index, (tx, ty)) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (0, 1),
        (2, 1),
        (0, 2),
        (1, 2),
        (2, 2),
    ]
    .into_iter()
    .enumerate()
    {
        for y in 0..tile[1] {
            for x in 0..tile[0] {
                let dest = (((ty * tile[1] + y) * width + tx * tile[0] + x) * 4) as usize;
                let reused = if matches!(index, 1 | 6) && x > 0 {
                    Some(dest - x as usize * 4)
                } else if matches!(index, 3 | 4) && y > 0 {
                    Some(dest - (y * width) as usize * 4)
                } else {
                    None
                };
                if let Some(source) = reused {
                    pixels.copy_within(source..source + 4, dest);
                    continue;
                }
                let mut sum = [0.0; 4];
                for sy in 0..4 {
                    for sx in 0..4 {
                        let sample = material.sample(
                            index,
                            f64::from(x) + (f64::from(sx) + 0.5) / 4.0,
                            f64::from(y) + (f64::from(sy) + 0.5) / 4.0,
                        );
                        for c in 0..3 {
                            sum[c] += sample[c] * sample[3] / 255.0;
                        }
                        sum[3] += sample[3];
                    }
                }
                if sum[3] > 0.0 {
                    for c in 0..3 {
                        pixels[dest + c] =
                            (sum[c] * 255.0 / sum[3]).round().clamp(0.0, 255.0) as u8;
                    }
                    pixels[dest + 3] = (sum[3] / 16.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::{PixelFormat, Size};
    pub(super) fn material() -> DecorationStyle {
        DecorationStyle {
            kind: DecorationKind::Recessed,
            background: [30; 3],
            surface: [20; 3],
            foreground: [200; 3],
            inset_px: [5, 5],
            radius_px: 10,
            sigma_milli: 3000,
            outset_px: [0, 0],
            offset_milli: [3500, 3500],
            shadow_opacity_milli: 720,
            lip_width_milli: 1200,
            lip_highlight_milli: 240,
            lip_shade_milli: 240,
        }
    }
    fn prepared() -> PreparedDecoration {
        let style = material();
        let cell = [10, 20];
        let [width, height] = atlas_size(style, cell).unwrap();
        PreparedDecoration {
            style,
            cell,
            image: Arc::new(Image {
                id: 1,
                generation: 1,
                width,
                height,
                format: PixelFormat::Rgba,
                pixels: vec![0; (width * height * 4) as usize],
            }),
        }
    }
    #[test]
    fn sparse_material_preserves_canonical_text_cursor_and_native_geometry() {
        let decoration = prepared();
        let mut frame = crate::blank(Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        });
        crate::text(&mut frame, 2, 2, "native content", false);
        let rows = frame.rows.clone();
        let cursor = frame.cursor.clone();
        assert!(decoration.place(
            &mut frame,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24
            },
            30
        ));
        assert_eq!(frame.rows, rows);
        assert_eq!(frame.cursor, cursor);
        assert_eq!(frame.placements.len(), 8);
        assert_eq!(frame.images.len(), 1);
        assert!(frame.images[0].pixels.len() < 80 * 24 * 10 * 20 * 4);
    }
    #[test]
    fn material_does_not_silently_shrink_to_fit_tiny_geometry() {
        let decoration = prepared();
        let mut frame = crate::blank(Size {
            cols: 2,
            rows: 1,
            cell_width: 10,
            cell_height: 20,
        });
        let original = frame.clone();
        assert!(!decoration.place(
            &mut frame,
            Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 1
            },
            1
        ));
        assert_eq!(frame, original);
        let larger = DecorationStyle {
            sigma_milli: 6000,
            ..decoration.style
        };
        assert!(atlas_size(larger, decoration.cell).unwrap()[0] > decoration.image.width);
    }
    #[test]
    fn inset_shadow_is_directional_continuous_and_has_no_outer_stroke() {
        let style = material();
        let size = atlas_size(style, [10, 20]).unwrap();
        let m = Material {
            style,
            tile: [size[0] / 3, size[1] / 3],
            cell: [10, 20],
        };
        let upper = m.sample(1, 0.5, 20.5)[3];
        let lower = m.sample(6, 0.5, f64::from(m.tile[1]) - 20.5)[3];
        assert!(upper > lower);
        assert_eq!(m.sample(0, 20.0, 20.0)[..3], [0.0; 3]);
        let pixels = rasterize(style, [10, 20]).unwrap();
        let center = ((size[1] / 2 * size[0] + size[0] / 2) * 4 + 3) as usize;
        assert_eq!(pixels[center], 0);
        assert!(pixels.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
    }
    #[test]
    fn rounded_aperture_masks_its_exterior_beyond_the_reserved_gutter() {
        let style = DecorationStyle {
            inset_px: [8, 16],
            radius_px: 12,
            sigma_milli: 2400,
            offset_milli: [2800; 2],
            ..material()
        };
        let size = atlas_size(style, [8, 16]).unwrap();
        let m = Material {
            style,
            tile: [size[0] / 3, size[1] / 3],
            cell: [8, 16],
        };
        // Both coordinates are in native content, past its reserved gutter.
        assert!(m.distance(0, 9.0, 17.0) < 0.0);
        assert_eq!(m.sample(0, 9.0, 17.0)[3], 255.0);
        assert!(m.distance(0, 15.0, 24.0) > 0.0);
        let inside = m.sample(0, 15.0, 24.0);
        assert_eq!(inside[..3], [0.0; 3]);
        assert!(inside[3] > 0.0 && inside[3] < 255.0);
    }
    #[test]
    fn raised_material_has_only_black_translucent_outer_shadow() {
        let style = DecorationStyle {
            kind: DecorationKind::Raised,
            ..material()
        };
        let size = atlas_size(style, [10, 20]).unwrap();
        let m = Material {
            style,
            tile: [size[0] / 3, size[1] / 3],
            cell: [10, 20],
        };
        let sample = m.sample(6, 0.5, f64::from(m.tile[1]) - 4.5);
        assert_eq!(sample[..3], [0.0; 3]);
        assert!(sample[3] > 0.0 && sample[3] < 255.0);
    }
    #[test]
    fn raised_surface_bevel_is_directional_and_leaves_center_and_outer_shadow_unchanged() {
        let cell = [10, 20];
        let style = DecorationStyle {
            kind: DecorationKind::RaisedSurface,
            background: [80, 100, 120],
            inset_px: [20, 20],
            outset_px: [20, 20],
            shadow_opacity_milli: 450,
            ..material()
        };
        let [width, height] = atlas_size(style, cell).unwrap();
        let tw = width / 3;
        let th = height / 3;
        let pixels = rasterize(style, cell).unwrap();
        let pixel = |pixels: &[u8], x, y| {
            let index = ((y * width + x) * 4) as usize;
            <[u8; 4]>::try_from(&pixels[index..index + 4]).unwrap()
        };
        let top = pixel(&pixels, tw, 20);
        let left = pixel(&pixels, 20, th);
        let bottom = pixel(&pixels, tw, 3 * th - 21);
        let right = pixel(&pixels, 3 * tw - 21, th);
        assert_eq!(top, left);
        assert_eq!(bottom, right);
        assert_eq!(top[3], 255);
        assert_eq!(bottom[3], 255);
        for channel in 0..3 {
            assert!(top[channel] > style.background[channel]);
            assert!(bottom[channel] < style.background[channel]);
        }
        assert_eq!(pixel(&pixels, tw, 24), [80, 100, 120, 255]);
        let plain = rasterize(
            DecorationStyle {
                lip_width_milli: 0,
                ..style
            },
            cell,
        )
        .unwrap();
        assert_eq!(pixel(&plain, tw, 20), [80, 100, 120, 255]);
        for (x, y) in [(0, th), (19, th), (3 * tw - 20, th)] {
            assert_eq!(pixel(&pixels, x, y), pixel(&plain, x, y));
        }
        let shadow = DecorationStyle {
            kind: DecorationKind::Raised,
            ..style
        };
        assert_eq!(
            rasterize(shadow, cell).unwrap(),
            rasterize(
                DecorationStyle {
                    lip_width_milli: 0,
                    ..shadow
                },
                cell
            )
            .unwrap(),
        );
    }
    #[test]
    fn raised_surface_raster_fills_only_the_opening_and_excludes_retained_body_cells() {
        let cell = [10, 20];
        let style = DecorationStyle {
            kind: DecorationKind::RaisedSurface,
            background: [80, 100, 120],
            inset_px: [20, 20],
            outset_px: [20, 20],
            shadow_opacity_milli: 450,
            lip_width_milli: 0,
            ..material()
        };
        let [width, height] = atlas_size(style, cell).unwrap();
        let image = Arc::new(Image {
            id: 12,
            generation: 1,
            width,
            height,
            format: PixelFormat::Rgba,
            pixels: rasterize(style, cell).unwrap(),
        });
        let tw = width / 3;
        let th = height / 3;
        let pixel = |x, y| {
            let index = ((y * width + x) * 4) as usize;
            <[u8; 4]>::try_from(&image.pixels[index..index + 4]).unwrap()
        };
        assert_eq!(pixel(22, th), [80, 100, 120, 255]);
        assert_eq!(pixel(0, th)[3], 0);
        let shadow = pixel(2 * tw + tw - 19, th);
        assert_eq!(shadow[..3], [0; 3]);
        assert!(shadow[3] > 0 && shadow[3] < 255);
        assert_eq!(pixel(3 * tw - 1, th)[3], 0);
        let decoration = PreparedDecoration { style, cell, image };
        let mut frame = crate::blank(Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        });
        crate::text(&mut frame, 7, 5, "retained guide body", false);
        let rows = frame.rows.clone();
        let cursor = frame.cursor.clone();
        let body = Rect {
            x: 7,
            y: 5,
            width: 26,
            height: 6,
        };
        assert!(decoration.place_clipped(
            &mut frame,
            Rect {
                x: 5,
                y: 4,
                width: 30,
                height: 8
            },
            128,
            &[body],
        ));
        assert_eq!(frame.rows, rows);
        assert_eq!(frame.cursor, cursor);
        assert!(frame.placements.iter().all(|p| {
            let x = p.col as u32 * 10 + p.offset_x;
            let y = p.row as u32 * 20 + p.offset_y;
            x + p.width <= 70 || x >= 330 || y + p.height <= 100 || y >= 220
        }));
        assert_eq!(frame.images.len(), 1);
        assert!(Arc::ptr_eq(&frame.images[0], &decoration.image));
    }
    #[test]
    fn caption_cutout_preserves_text_and_only_removes_its_material_pixels() {
        let decoration = prepared();
        let mut frame = crate::blank(Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        });
        crate::text(&mut frame, 1, 23, " pane title ", false);
        let original_rows = frame.rows.clone();
        let caption = Rect {
            x: 1,
            y: 23,
            width: 12,
            height: 1,
        };
        assert!(decoration.place_clipped(
            &mut frame,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24
            },
            64,
            &[caption]
        ));
        assert_eq!(frame.rows, original_rows);
        assert!(frame.placements.iter().all(|p| {
            let x = p.col as u32 * 10 + p.offset_x;
            let y = p.row as u32 * 20 + p.offset_y;
            x >= 130 || x + p.width <= 10 || y >= 480 || y + p.height <= 460
        }));
        assert!(
            frame
                .placements
                .iter()
                .any(|p| p.row == 22 && p.width > 100)
        );
    }
    #[test]
    fn raised_shadow_extends_outside_popup_and_clips_at_the_screen_origin() {
        let style = DecorationStyle {
            kind: DecorationKind::Raised,
            inset_px: [20, 20],
            outset_px: [20, 20],
            shadow_opacity_milli: 450,
            lip_width_milli: 0,
            lip_highlight_milli: 0,
            lip_shade_milli: 0,
            ..material()
        };
        let [width, height] = atlas_size(style, [10, 20]).unwrap();
        let decoration = PreparedDecoration {
            style,
            cell: [10, 20],
            image: Arc::new(Image {
                id: 2,
                generation: 1,
                width,
                height,
                format: PixelFormat::Rgba,
                pixels: rasterize(style, [10, 20]).unwrap(),
            }),
        };
        let size = Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut frame = crate::graphics_frame(size);
        assert!(decoration.place(
            &mut frame,
            Rect {
                x: 5,
                y: 4,
                width: 30,
                height: 8
            },
            0
        ));
        assert!(frame.rows.is_empty());
        assert!(frame.placements.iter().any(|p| p.col < 5 || p.row < 4));
        let mut edge = crate::graphics_frame(size);
        assert!(decoration.place(
            &mut edge,
            Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 8
            },
            0
        ));
        assert!(edge.placements.iter().all(|p| p.col >= 0 && p.row >= 0));
    }
    #[test]
    fn four_straight_edges_are_constant_axis_textures_not_per_cell_images() {
        let mut decoration = prepared();
        Arc::get_mut(&mut decoration.image).unwrap().pixels =
            rasterize(decoration.style, decoration.cell).unwrap();
        let image = &decoration.image;
        let tw = image.width / 3;
        let th = image.height / 3;
        let pixel = |x, y| {
            let start = ((y * image.width + x) * 4) as usize;
            &image.pixels[start..start + 4]
        };
        for (tx, ty) in [(1, 0), (1, 2)] {
            for y in 0..th {
                for x in 1..tw {
                    assert_eq!(pixel(tx * tw, ty * th + y), pixel(tx * tw + x, ty * th + y));
                }
            }
        }
        for (tx, ty) in [(0, 1), (2, 1)] {
            for x in 0..tw {
                for y in 1..th {
                    assert_eq!(pixel(tx * tw + x, ty * th), pixel(tx * tw + x, ty * th + y));
                }
            }
        }
        for cols in [80, 120, 240] {
            let size = Size {
                cols,
                rows: 40,
                cell_width: 10,
                cell_height: 20,
            };
            let mut frame = crate::graphics_frame(size);
            assert!(decoration.place(
                &mut frame,
                Rect {
                    x: 0,
                    y: 0,
                    width: cols,
                    height: 40
                },
                0
            ));
            assert_eq!(frame.placements.len(), 8);
            assert_eq!(
                frame
                    .placements
                    .iter()
                    .filter(|p| p.source_width == 1)
                    .count(),
                2
            );
            assert_eq!(
                frame
                    .placements
                    .iter()
                    .filter(|p| p.source_height == 1)
                    .count(),
                2
            );
            assert!(Arc::ptr_eq(&frame.images[0], image));
        }
    }
    #[test]
    fn physical_openings_match_native_columns_and_both_separator_axes() {
        for cell in [[8, 16], [9, 20], [14, 9]] {
            let [cw, ch] = cell;
            let style = DecorationStyle {
                inset_px: cell,
                radius_px: (cw * 3 / 2).min(2 * ch),
                shadow_opacity_milli: 0,
                lip_width_milli: 0,
                ..material()
            };
            let [width, height] = atlas_size(style, cell).unwrap();
            let decoration = PreparedDecoration {
                style,
                cell,
                image: Arc::new(Image {
                    id: 7,
                    generation: 1,
                    width,
                    height,
                    format: PixelFormat::Rgba,
                    pixels: rasterize(style, cell).unwrap(),
                }),
            };
            let size = Size {
                cols: 40,
                rows: 24,
                cell_width: cw,
                cell_height: ch,
            };
            let delta = i64::from(ch.saturating_sub(cw));
            let floor = delta.div_euclid(2);
            let ceil = delta - floor;
            for horizontal in [false, true] {
                let mut frame = crate::blank(size);
                crate::text(&mut frame, 2, 2, "native", false);
                let native_rows = frame.rows.clone();
                let native_cursor = frame.cursor.clone();
                let (rects, bounds, clips, start) = if horizontal {
                    let mid = 11 * ch + ch / 2;
                    (
                        [
                            Rect {
                                x: 0,
                                y: 0,
                                width: 40,
                                height: 12,
                            },
                            Rect {
                                x: 0,
                                y: 11,
                                width: 40,
                                height: 13,
                            },
                        ],
                        [
                            [0, -delta, i64::from(40 * cw), i64::from(12 * ch) + floor],
                            [
                                0,
                                i64::from(11 * ch) - ceil,
                                i64::from(40 * cw),
                                i64::from(24 * ch) + delta,
                            ],
                        ],
                        [[0, 0, 40 * cw, mid], [0, mid, 40 * cw, 24 * ch]],
                        (i64::from(11 * ch) + floor) as u32,
                    )
                } else {
                    let mid = 19 * cw + cw / 2;
                    (
                        [
                            Rect {
                                x: 0,
                                y: 0,
                                width: 20,
                                height: 24,
                            },
                            Rect {
                                x: 19,
                                y: 0,
                                width: 21,
                                height: 24,
                            },
                        ],
                        [
                            [0, -delta, i64::from(20 * cw), i64::from(24 * ch) + delta],
                            [
                                i64::from(19 * cw),
                                -delta,
                                i64::from(40 * cw),
                                i64::from(24 * ch) + delta,
                            ],
                        ],
                        [[0, 0, mid, 24 * ch], [mid, 0, 40 * cw, 24 * ch]],
                        19 * cw,
                    )
                };
                for i in 0..2 {
                    assert!(decoration.place_with_geometry(
                        &mut frame,
                        rects[i],
                        i as u32 * 64,
                        &[],
                        DecorationGeometry {
                            bounds_px: bounds[i],
                            clip_px: clips[i]
                        },
                    ));
                }
                let pixel = |coordinate| {
                    let (x, y) = if horizontal {
                        (20 * cw, coordinate)
                    } else {
                        (coordinate, 12 * ch)
                    };
                    let p = frame
                        .placements
                        .iter()
                        .find(|p| {
                            let left = p.col as u32 * cw + p.offset_x;
                            let top = p.row as u32 * ch + p.offset_y;
                            x >= left && x < left + p.width && y >= top && y < top + p.height
                        })
                        .unwrap();
                    let sx = p.source_x
                        + if p.source_width == 1 {
                            0
                        } else {
                            x - p.col as u32 * cw - p.offset_x
                        };
                    let sy = p.source_y
                        + if p.source_height == 1 {
                            0
                        } else {
                            y - p.row as u32 * ch - p.offset_y
                        };
                    let index = ((sy * width + sx) * 4) as usize;
                    <[u8; 4]>::try_from(&decoration.image.pixels[index..index + 4]).unwrap()
                };
                let thickness = if horizontal { cw.min(ch) } else { cw };
                for coordinate in start..start + thickness {
                    assert_eq!(
                        pixel(coordinate),
                        [20, 20, 20, 255],
                        "{cell:?}, horizontal={horizontal}"
                    );
                }
                for coordinate in [start - 1, start + thickness] {
                    assert_eq!(pixel(coordinate)[3], 0);
                }
                assert_eq!(frame.rows, native_rows);
                assert_eq!(frame.cursor, native_cursor);
                assert_eq!(frame.images.len(), 1);
                assert!(Arc::ptr_eq(&frame.images[0], &decoration.image));
            }
        }
    }
}
