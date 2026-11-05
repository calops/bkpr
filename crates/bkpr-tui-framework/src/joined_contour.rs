//! Rounded union geometry adapted from bkpr-zellij 18890b231bc3dd29039dd6ca141ec0af49cf332e
//! zellij-server/src/output/recessed_frames/contour.rs.
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
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Rect {
    pub x: i32,
    pub y: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x)
            && x <= f64::from(self.right)
            && y >= f64::from(self.y)
            && y <= f64::from(self.bottom)
    }
    pub fn intersects(self, other: Self) -> bool {
        self.x <= other.right
            && other.x <= self.right
            && self.y <= other.bottom
            && other.y <= self.bottom
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Feature {
    Edge {
        from: Point,
        to: Point,
        nx: i32,
        ny: i32,
    },
    Arc {
        center: Point,
        radius: i32,
        sx: i32,
        sy: i32,
        convex: bool,
    },
}

impl Feature {
    pub fn bounds(&self) -> Rect {
        match *self {
            Self::Edge { from, to, .. } => Rect {
                x: from.x.min(to.x),
                y: from.y.min(to.y),
                right: from.x.max(to.x),
                bottom: from.y.max(to.y),
            },
            Self::Arc {
                center,
                radius,
                sx,
                sy,
                ..
            } => Rect {
                x: center.x.min(center.x + sx * radius),
                y: center.y.min(center.y + sy * radius),
                right: center.x.max(center.x + sx * radius),
                bottom: center.y.max(center.y + sy * radius),
            },
        }
    }

    pub fn local(&self, origin: Point, clip: Rect) -> Self {
        let shift = |point: Point| Point {
            x: point.x - origin.x,
            y: point.y - origin.y,
        };
        match *self {
            Self::Edge {
                mut from,
                mut to,
                nx,
                ny,
            } => {
                if from.x == to.x {
                    from.y = from.y.clamp(clip.y, clip.bottom);
                    to.y = to.y.clamp(clip.y, clip.bottom);
                } else {
                    from.x = from.x.clamp(clip.x, clip.right);
                    to.x = to.x.clamp(clip.x, clip.right);
                }
                Self::Edge {
                    from: shift(from),
                    to: shift(to),
                    nx,
                    ny,
                }
            }
            Self::Arc {
                center,
                radius,
                sx,
                sy,
                convex,
            } => Self::Arc {
                center: shift(center),
                radius,
                sx,
                sy,
                convex,
            },
        }
    }

    // Closest point and inward normal. Arc endpoint normals agree with the
    // adjacent straight segment, including at inward (concave) fillets.
    pub fn nearest(&self, x: f64, y: f64) -> (f64, f64, f64, f64) {
        match *self {
            Self::Edge { from, to, nx, ny } => {
                let px = x.clamp(f64::from(from.x.min(to.x)), f64::from(from.x.max(to.x)));
                let py = y.clamp(f64::from(from.y.min(to.y)), f64::from(from.y.max(to.y)));
                (px, py, f64::from(nx), f64::from(ny))
            }
            Self::Arc {
                center,
                radius,
                sx,
                sy,
                convex,
            } => {
                let dx = (x - f64::from(center.x)) * f64::from(sx);
                let dy = (y - f64::from(center.y)) * f64::from(sy);
                let (ux, uy) = if dx >= 0.0 && dy >= 0.0 {
                    let length = dx.hypot(dy);
                    if length > 0.0 {
                        (dx / length, dy / length)
                    } else {
                        (1.0, 0.0)
                    }
                } else if dx >= dy {
                    (1.0, 0.0)
                } else {
                    (0.0, 1.0)
                };
                let rx = ux * f64::from(sx);
                let ry = uy * f64::from(sy);
                let sign = if convex { -1.0 } else { 1.0 };
                (
                    f64::from(center.x) + rx * f64::from(radius),
                    f64::from(center.y) + ry * f64::from(radius),
                    sign * rx,
                    sign * ry,
                )
            }
        }
    }
}

pub(super) fn distance(features: &[Feature], x: f64, y: f64) -> (f64, f64, f64) {
    let mut result = (f64::NEG_INFINITY, 0.0, 0.0);
    let mut nearest = f64::INFINITY;
    for feature in features {
        let (px, py, nx, ny) = feature.nearest(x, y);
        let dx = x - px;
        let dy = y - py;
        let squared = dx * dx + dy * dy;
        if squared < nearest {
            nearest = squared;
            let sign = if dx * nx + dy * ny >= 0.0 { 1.0 } else { -1.0 };
            result = (squared.sqrt() * sign, nx, ny);
        }
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Contour {
    pub features: Vec<Feature>,
    pub bounds: Rect,
}

impl Contour {
    pub fn new(rects: [Rect; 2], radius: i32) -> Option<Self> {
        let [a, b] = rects;
        if a.x.max(b.x) >= a.right.min(b.right) || a.y.max(b.y) >= a.bottom.min(b.bottom) {
            return None;
        }
        let mut xs = vec![a.x, a.right, b.x, b.right];
        xs.sort();
        xs.dedup();
        let mut ys = vec![a.y, a.bottom, b.y, b.bottom];
        ys.sort();
        ys.dedup();
        let filled = |ix: usize, iy: usize| {
            rects.iter().any(|rect| {
                rect.contains(
                    f64::from(xs[ix] + xs[ix + 1]) / 2.0,
                    f64::from(ys[iy] + ys[iy + 1]) / 2.0,
                )
            })
        };
        let mut edges = BTreeMap::new();
        for iy in 0..ys.len() - 1 {
            for ix in 0..xs.len() - 1 {
                if !filled(ix, iy) {
                    continue;
                }
                let p = [
                    Point {
                        x: xs[ix],
                        y: ys[iy],
                    },
                    Point {
                        x: xs[ix + 1],
                        y: ys[iy],
                    },
                    Point {
                        x: xs[ix + 1],
                        y: ys[iy + 1],
                    },
                    Point {
                        x: xs[ix],
                        y: ys[iy + 1],
                    },
                ];
                if iy == 0 || !filled(ix, iy - 1) {
                    edges.insert(p[0], p[1]);
                }
                if ix + 2 == xs.len() || !filled(ix + 1, iy) {
                    edges.insert(p[1], p[2]);
                }
                if iy + 2 == ys.len() || !filled(ix, iy + 1) {
                    edges.insert(p[2], p[3]);
                }
                if ix == 0 || !filled(ix - 1, iy) {
                    edges.insert(p[3], p[0]);
                }
            }
        }
        let first = *edges.keys().next()?;
        let mut points = vec![];
        let mut current = first;
        loop {
            points.push(current);
            current = edges.remove(&current)?;
            if current == first {
                break;
            }
        }
        loop {
            let count = points.len();
            let index = (0..count).find(|i| {
                let p = points[(i + count - 1) % count];
                let q = points[*i];
                let r = points[(i + 1) % count];
                i64::from(q.x - p.x) * i64::from(r.y - q.y)
                    - i64::from(q.y - p.y) * i64::from(r.x - q.x)
                    == 0
            });
            if let Some(index) = index {
                points.remove(index);
            } else {
                break;
            }
        }
        let count = points.len();
        let mut corners = vec![];
        for i in 0..count {
            let prev = points[(i + count - 1) % count];
            let v = points[i];
            let next = points[(i + 1) % count];
            let incoming = Point {
                x: (v.x - prev.x).signum(),
                y: (v.y - prev.y).signum(),
            };
            let outgoing = Point {
                x: (next.x - v.x).signum(),
                y: (next.y - v.y).signum(),
            };
            let r = radius
                .min(((v.x - prev.x).abs() + (v.y - prev.y).abs()) / 2)
                .min(((next.x - v.x).abs() + (next.y - v.y).abs()) / 2);
            let from = Point {
                x: v.x - incoming.x * r,
                y: v.y - incoming.y * r,
            };
            let to = Point {
                x: v.x + outgoing.x * r,
                y: v.y + outgoing.y * r,
            };
            let center = Point {
                x: from.x + outgoing.x * r,
                y: from.y + outgoing.y * r,
            };
            let convex = incoming.x * outgoing.y - incoming.y * outgoing.x > 0;
            let sx = (v.x - center.x).signum();
            let sy = (v.y - center.y).signum();
            corners.push((from, to, center, r, sx, sy, convex, outgoing));
        }
        let mut features = vec![];
        for i in 0..count {
            let (from, to, center, r, sx, sy, convex, outgoing) = corners[i];
            if r > 0 {
                features.push(Feature::Arc {
                    center,
                    radius: r,
                    sx,
                    sy,
                    convex,
                });
            }
            let next = corners[(i + 1) % count].0;
            if to != next {
                features.push(Feature::Edge {
                    from: to,
                    to: next,
                    nx: -outgoing.y,
                    ny: outgoing.x,
                });
            }
            let _ = from;
        }
        Some(Self {
            features,
            bounds: Rect {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
                right: a.right.max(b.right),
                bottom: a.bottom.max(b.bottom),
            },
        })
    }

    pub fn full_cell(&self, rect: Rect) -> bool {
        Self::features_full_cell(&self.features, rect)
    }

    pub fn features_full_cell(features: &[Feature], rect: Rect) -> bool {
        if [rect.x, rect.right].into_iter().any(|x| {
            [rect.y, rect.bottom]
                .into_iter()
                .any(|y| distance(features, f64::from(x), f64::from(y)).0 < -1e-6)
        }) {
            return false;
        }
        // Corners alone suffice for convex pieces. An inward arc can cut the
        // middle of an otherwise covered cell, so check its excluded disk too.
        !Self::arc_cuts_rect(features, rect, false)
    }

    /// Certify uniform coverage; uncertain boundary pixels retain subpixel sampling.
    pub fn pixel_coverage(features: &[Feature], rect: Rect) -> Option<bool> {
        let mut inside = true;
        let mut outside = true;
        for x in [rect.x, rect.right] {
            for y in [rect.y, rect.bottom] {
                let d = distance(features, f64::from(x), f64::from(y)).0;
                inside &= d >= 0.0;
                outside &= d <= 0.0;
                if !inside && !outside {
                    return None;
                }
            }
        }
        if inside && !Self::arc_cuts_rect(features, rect, false) {
            Some(true)
        } else if outside && !Self::arc_cuts_rect(features, rect, true) {
            Some(false)
        } else {
            None
        }
    }

    fn arc_cuts_rect(features: &[Feature], rect: Rect, convex: bool) -> bool {
        features.iter().any(|feature| {
            let Feature::Arc {
                center,
                radius,
                convex: orientation,
                ..
            } = *feature
            else {
                return false;
            };
            if orientation != convex {
                return false;
            }
            let bounds = feature.bounds();
            let left = rect.x.max(bounds.x);
            let right = rect.right.min(bounds.right);
            let top = rect.y.max(bounds.y);
            let bottom = rect.bottom.min(bounds.bottom);
            if left >= right || top >= bottom {
                return false;
            }
            let dx = center.x.clamp(left, right) - center.x;
            let dy = center.y.clamp(top, bottom) - center.y;
            dx * dx + dy * dy < radius * radius
        })
    }
}
