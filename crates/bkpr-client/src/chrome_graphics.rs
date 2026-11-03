//! Bkpr's embedded decorative artwork. Generic preparation and scene resources
//! belong to the terminal UI framework; this wrapper owns only marker policy.
use bkpr_protocol::{ColorSource, Frame, Size};
use bkpr_tui_framework::{
    ColorProbe, Rect,
    decoration::{DecorationGeometry, DecorationStyle},
    graphics::{SvgAsset, SvgGraphics},
};
use std::{io, os::fd::RawFd};

#[used]
static NOTICES: &str = concat!(
    include_str!("../assets/NOTICE"),
    include_str!("../assets/MaterialDesign-LICENSE"),
    include_str!("../assets/Apache-2.0.txt"),
);
static ASSETS: &[SvgAsset] = &[
    SvgAsset {
        id: 1,
        svg: include_str!("../assets/bee.svg"),
    },
    SvgAsset {
        id: 2,
        svg: include_str!("../assets/hive.svg"),
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Artwork {
    Bee,
    BeeHidden,
    Hive,
}

type DecorationEvidence = (
    bool,
    Option<[u8; 3]>,
    Option<[u8; 3]>,
    u32,
    u32,
    [Option<[u8; 3]>; 5],
);

#[derive(Default)]
struct PaneGeometry {
    cell: [u32; 2],
    rects: Vec<Rect>,
    placements: Vec<DecorationGeometry>,
}

impl PaneGeometry {
    fn prepare(&mut self, size: Size, rects: &[(String, Rect)]) {
        let cell = [size.cell_width, size.cell_height];
        if self.cell == cell
            && self.rects.len() == rects.len()
            && self
                .rects
                .iter()
                .zip(rects)
                .all(|(old, (_, rect))| old == rect)
        {
            return;
        }
        self.cell = cell;
        self.rects.clear();
        self.rects.extend(rects.iter().map(|(_, rect)| *rect));
        self.placements.clear();
        let [cw, ch] = cell;
        let excess = i64::from(ch.saturating_sub(cw));
        for rect in &self.rects {
            let left = u32::from(rect.x);
            let top = u32::from(rect.y);
            let right = left + u32::from(rect.width);
            let bottom = top + u32::from(rect.height);
            let mut shared = [false; 4];
            for other in &self.rects {
                if std::ptr::eq(rect, other) {
                    continue;
                }
                let ol = u32::from(other.x);
                let ot = u32::from(other.y);
                let or = ol + u32::from(other.width);
                let ob = ot + u32::from(other.height);
                // Native split frames overlap by one cell. A corner-only
                // contact does not own any part of the straight separator.
                if bottom.min(ob).saturating_sub(top.max(ot)) > 1 {
                    shared[0] |= or == left + 1;
                    shared[2] |= ol + 1 == right;
                }
                if right.min(or).saturating_sub(left.max(ol)) > 1 {
                    shared[1] |= ob == top + 1;
                    shared[3] |= ot + 1 == bottom;
                }
            }
            let [left, top, right, bottom] = [left * cw, top * ch, right * cw, bottom * ch];
            self.placements.push(DecorationGeometry {
                bounds_px: [
                    i64::from(left),
                    i64::from(top) - if shared[1] { (excess + 1) / 2 } else { excess },
                    i64::from(right),
                    i64::from(bottom) + if shared[3] { excess / 2 } else { excess },
                ],
                clip_px: [
                    left + if shared[0] { cw / 2 } else { 0 },
                    top + if shared[1] { ch / 2 } else { 0 },
                    right - if shared[2] { cw - cw / 2 } else { 0 },
                    bottom - if shared[3] { ch - ch / 2 } else { 0 },
                ],
            });
        }
    }
}
pub(crate) struct ChromeGraphics(
    SvgGraphics,
    Option<[DecorationStyle; 11]>,
    Option<DecorationEvidence>,
    PaneGeometry,
);
impl ChromeGraphics {
    pub(crate) fn new() -> io::Result<Self> {
        std::hint::black_box(NOTICES);
        SvgGraphics::new(ASSETS, 2, 1)
            .map(|graphics| Self(graphics, None, None, PaneGeometry::default()))
    }
    pub(crate) fn wake_fd(&self) -> RawFd {
        self.0.wake_fd()
    }
    pub(crate) fn drain(&mut self) -> io::Result<bool> {
        self.0.drain()
    }
    /// Select decorative presentation only; native pane images and actual host
    /// capability evidence are independent. Call before preparing each frame.
    pub(crate) fn set_enabled(&mut self, enabled: bool) -> bool {
        self.0.set_enabled(enabled)
    }
    pub(crate) fn prepare(&mut self, host: &ColorProbe, size: Size, sources: &[ColorSource]) {
        if !self.0.is_enabled() {
            return;
        }
        let evidence = (
            host.truecolor,
            host.foreground,
            host.background,
            size.cell_width,
            size.cell_height,
            [1, 2, 3, 4, 6].map(|index| host.palette.get(&index).copied()),
        );
        if self.2 != Some(evidence) {
            self.1 = crate::appearance::decoration_styles(host, size).map(|[pane, popup]| {
                let guide = |accent| DecorationStyle {
                    background: crate::appearance::guide_background(host, accent)
                        .unwrap_or(popup.background),
                    ..popup
                };
                let region = |accent| DecorationStyle {
                    background: crate::appearance::region_background(host, accent)
                        .unwrap_or(popup.background),
                    ..popup
                };
                [
                    pane,
                    popup,
                    guide(None),
                    guide(Some(4)),
                    guide(Some(3)),
                    region(None),
                    region(Some(6)),
                    region(Some(1)),
                    region(Some(2)),
                    region(Some(3)),
                    region(Some(4)),
                ]
            });
            self.2 = Some(evidence);
        }
        self.0.prepare_with_decorations(
            host,
            size,
            sources,
            self.1.as_ref().map_or(&[], |styles| styles.as_slice()),
        );
    }

    /// Cache complete native split geometry; unchanged renders allocate nothing.
    pub(crate) fn prepare_panes(&mut self, size: Size, rects: &[(String, Rect)]) {
        if self.0.is_enabled() {
            self.3.prepare(size, rects);
        }
    }

    /// Request the single main-like passive item/details material. Geometry
    /// changes do not invalidate finder/guide/perimeter raster caches.
    pub(crate) fn prepare_joined(&mut self, size: Size, popup: Option<(Rect, Rect)>) {
        let request = popup.and_then(|(popup, item)| {
            crate::appearance::joined_surface(size, popup, item, self.1?[1])
        });
        self.0.prepare_joined(request);
    }

    pub(crate) fn joined_ready(&self) -> bool {
        self.0.joined_ready()
    }

    pub(crate) fn joined_opaque_spans(&self) -> &[bkpr_tui_framework::joined::OpaqueSpan] {
        self.0.joined_opaque_spans()
    }

    pub(crate) fn decorate_joined(
        &self,
        frame: &mut Frame,
        base: u32,
        exclusions: &[Rect],
    ) -> bool {
        self.0.decorate_joined(frame, base, exclusions)
    }

    pub(crate) fn decorate_pane(
        &self,
        frame: &mut Frame,
        rect: Rect,
        index: usize,
        base: u32,
    ) -> bool {
        self.3
            .rects
            .get(index)
            .is_some_and(|cached| *cached == rect)
            && self.3.placements.get(index).is_some_and(|geometry| {
                self.decorate_window_with_geometry(frame, rect, false, base, &[], *geometry)
            })
    }
    /// Add static material without changing text/cursor/native geometry.
    /// Raised output needs a root-sized graphics-only frame for its outer halo.
    /// Callers provide semantic title/caption cutouts and reserve 64 IDs.
    /// A false result leaves their complete text-border fallback untouched.
    pub(crate) fn decorate_window_in_clipped(
        &self,
        frame: &mut Frame,
        rect: Rect,
        raised: bool,
        base: u32,
        exclusions: &[Rect],
    ) -> bool {
        self.1.is_some_and(|styles| {
            self.0.decorate_window_clipped(
                frame,
                rect,
                styles[usize::from(raised)],
                base,
                exclusions,
            )
        })
    }

    /// Place a pane contour without moving its native content rectangle.
    /// Pixel bounds translate cached curves; the clip owns shared separators.
    pub(crate) fn decorate_window_with_geometry(
        &self,
        frame: &mut Frame,
        rect: Rect,
        raised: bool,
        base: u32,
        exclusions: &[Rect],
        geometry: DecorationGeometry,
    ) -> bool {
        self.1.is_some_and(|styles| {
            self.0.decorate_window_with_geometry(
                frame,
                rect,
                styles[usize::from(raised)],
                base,
                exclusions,
                geometry,
            )
        })
    }

    /// Paint main's rounded guide surface around its retained text body.
    /// The logical outer cells remain available for overlay hit testing only.
    pub(crate) fn decorate_guide(
        &self,
        frame: &mut Frame,
        outer: Rect,
        body: Rect,
        base: u32,
        accent: Option<u8>,
    ) -> bool {
        if !self.0.is_enabled() {
            return false;
        }
        let Some(styles) = self.1 else {
            return false;
        };
        let cw = frame.size.cell_width;
        let ch = frame.size.cell_height;
        let padding = cw.div_ceil(2).min(ch);
        let mx = i64::from(2 * cw - padding);
        let my = i64::from(ch - padding);
        let geometry = DecorationGeometry {
            bounds_px: [
                i64::from(outer.x) * i64::from(cw) + mx,
                i64::from(outer.y) * i64::from(ch) + my,
                (i64::from(outer.x) + i64::from(outer.width)) * i64::from(cw) - mx,
                (i64::from(outer.y) + i64::from(outer.height)) * i64::from(ch) - my,
            ],
            clip_px: [
                0,
                0,
                u32::from(frame.size.cols) * cw,
                u32::from(frame.size.rows) * ch,
            ],
        };
        let index = match accent {
            Some(4) => 3,
            Some(3) => 4,
            _ => 2,
        };
        self.0
            .decorate_window_with_geometry(frame, outer, styles[index], base, &[body], geometry)
    }

    /// Place an independently shaded rounded card with a half-cell-width margin
    /// on both physical axes. The caller projects only `text_body` as opaque
    /// cells; the retained atlas fills the rest without a square cell underlay.
    /// Both rectangles use the destination frame's screen coordinates.
    pub(crate) fn decorate_region(
        &self,
        frame: &mut Frame,
        card: Rect,
        text_body: Rect,
        base: u32,
        accent: Option<u8>,
    ) -> bool {
        if !self.0.is_enabled()
            || text_body.width == 0
            || text_body.height == 0
            || text_body.x < card.x
            || text_body.y < card.y
            || u32::from(text_body.x) + u32::from(text_body.width)
                > u32::from(card.x) + u32::from(card.width)
            || u32::from(text_body.y) + u32::from(text_body.height)
                > u32::from(card.y) + u32::from(card.height)
        {
            return false;
        }
        let Some(styles) = self.1 else {
            return false;
        };
        let index = match accent {
            None => 5,
            Some(6) => 6,
            Some(index @ 1..=4) => 6 + usize::from(index),
            _ => return false,
        };
        let cw = i64::from(frame.size.cell_width);
        let ch = i64::from(frame.size.cell_height);
        let margin = (cw + 1) / 2;
        let geometry = DecorationGeometry {
            bounds_px: [
                i64::from(card.x) * cw + margin,
                i64::from(card.y) * ch + margin,
                (i64::from(card.x) + i64::from(card.width)) * cw - margin,
                (i64::from(card.y) + i64::from(card.height)) * ch - margin,
            ],
            clip_px: [
                0,
                0,
                u32::from(frame.size.cols) * frame.size.cell_width,
                u32::from(frame.size.rows) * frame.size.cell_height,
            ],
        };
        self.0.decorate_window_with_geometry(
            frame,
            card,
            styles[index],
            base,
            &[text_body],
            geometry,
        )
    }

    /// Call after fitting an existing two-cell decorative marker, never a label
    /// or semantic status glyph. Unready/unsupported restores its text fallback.
    pub(crate) fn decorate(
        &self,
        frame: &mut Frame,
        artwork: Artwork,
        col: u16,
        row: u16,
        source: Option<ColorSource>,
        placement_id: u32,
    ) -> bool {
        if col.checked_add(2).is_none_or(|end| end > frame.size.cols)
            || row >= frame.size.rows
            || (artwork == Artwork::Hive && col == 0)
        {
            return false;
        }
        let range = usize::from(col)..usize::from(col) + 2;
        let Some(cells) = frame
            .rows
            .get(usize::from(row))
            .and_then(|r| r.cells.get(range.clone()))
        else {
            return false;
        };
        if cells.iter().any(|cell| cell.width != 1) {
            return false;
        }
        let marker = match artwork {
            Artwork::Bee | Artwork::Hive => ["●", " "],
            Artwork::BeeHidden => ["◌", " "],
        };
        if !cells
            .iter()
            .zip(marker)
            .all(|(cell, marker)| cell.text == marker)
            && !cells.iter().all(|cell| cell.text == " ")
        {
            return false;
        }
        // Retained UI cells may still contain blanks from the previous icon.
        // Restore the product's marker before a resize/color/capability fallback.
        for (cell, marker) in frame.rows[usize::from(row)].cells[range.clone()]
            .iter_mut()
            .zip(marker)
        {
            cell.text.clear();
            cell.text.push_str(marker);
        }
        let Some(source) = source else { return false };
        let (id, x, offset_x) = match artwork {
            Artwork::Bee | Artwork::BeeHidden => (1, i32::from(col), 0),
            // MAIN positions the hive half a cell left into its tree gutter.
            Artwork::Hive => (2, i32::from(col) - 1, frame.size.cell_width / 2),
        };
        if !self.0.place(
            frame,
            id,
            [x, i32::from(row)],
            [offset_x, 0],
            source,
            placement_id,
        ) {
            return false;
        }
        for cell in &mut frame.rows[usize::from(row)].cells[range] {
            cell.text.clear();
            cell.text.push(' ');
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_toggle_restores_markers_and_disables_every_material_path() {
        let mut host = ColorProbe::default();
        let query = host.graphics.begin();
        let query = std::str::from_utf8(&query).unwrap();
        let id = query.split("i=").nth(1).unwrap().split(',').next().unwrap();
        host.feed(format!("graphics;Gi={id};OK").as_bytes());
        host.feed(b"10;rgb:ffff/ffff/ffff");
        host.feed(b"11;rgb:2222/2222/2222");
        host.truecolor = true;
        let size = Size {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        let geometry = DecorationGeometry {
            bounds_px: [0, 0, 800, 480],
            clip_px: [0, 0, 800, 480],
        };
        let guide = Rect {
            x: 10,
            y: 4,
            width: 40,
            height: 12,
        };
        let body = Rect {
            x: 12,
            y: 5,
            width: 36,
            height: 10,
        };
        let mut chrome = ChromeGraphics::new().unwrap();
        let prepared = |chrome: &mut ChromeGraphics| {
            let mut poll = libc::pollfd {
                fd: chrome.wake_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 5000) }, 1);
            assert!(chrome.drain().unwrap());
        };
        chrome.prepare(&host, size, &[]);
        chrome.prepare_panes(size, &[("pane".into(), rect)]);
        prepared(&mut chrome);
        let mut markers = Vec::new();
        for artwork in [Artwork::Hive, Artwork::Bee, Artwork::BeeHidden] {
            let mut frame = bkpr_tui_framework::blank(size);
            let marker = if artwork == Artwork::BeeHidden {
                "◌"
            } else {
                "●"
            };
            bkpr_tui_framework::text(&mut frame, 1, 1, &format!("{marker} label"), false);
            let original = frame.clone();
            assert!(chrome.decorate(&mut frame, artwork, 1, 1, Some(ColorSource::Default), 1));
            let image = std::sync::Arc::clone(&frame.images[0]);
            markers.push((artwork, frame, original, image));
        }
        let mut rich = bkpr_tui_framework::blank(size);
        assert!(chrome.decorate_pane(&mut rich, rect, 0, 64));
        for raised in [false, true] {
            assert!(chrome.decorate_window_in_clipped(&mut rich, rect, raised, 128, &[]));
            assert!(chrome.decorate_window_with_geometry(
                &mut rich,
                rect,
                raised,
                192,
                &[],
                geometry,
            ));
        }
        for accent in [None, Some(4), Some(3)] {
            assert!(chrome.decorate_guide(&mut rich, guide, body, 256, accent));
        }

        chrome.set_enabled(false);
        chrome.prepare(&host, size, &[]);
        chrome.prepare_panes(size, &[]);
        for (artwork, frame, original, _) in &mut markers {
            frame.images.clear();
            frame.placements.clear();
            assert!(!chrome.decorate(frame, *artwork, 1, 1, Some(ColorSource::Default), 1));
            assert_eq!(frame, original);
        }
        let mut plain = bkpr_tui_framework::blank(size);
        bkpr_tui_framework::text(&mut plain, 1, 1, "retained title and content", false);
        let original = plain.clone();
        assert!(!chrome.decorate_pane(&mut plain, rect, 0, 64));
        for raised in [false, true] {
            assert!(!chrome.decorate_window_in_clipped(&mut plain, rect, raised, 128, &[]));
            assert!(!chrome.decorate_window_with_geometry(
                &mut plain,
                rect,
                raised,
                192,
                &[],
                geometry,
            ));
        }
        for accent in [None, Some(4), Some(3)] {
            assert!(!chrome.decorate_guide(&mut plain, guide, body, 256, accent));
        }
        assert_eq!(plain, original);
        assert!(host.graphics.supported());

        chrome.set_enabled(true);
        chrome.prepare(&host, size, &[]);
        prepared(&mut chrome);
        for (artwork, frame, _, image) in &mut markers {
            assert!(chrome.decorate(frame, *artwork, 1, 1, Some(ColorSource::Default), 1));
            assert!(std::sync::Arc::ptr_eq(image, &frame.images[0]));
        }
        assert!(chrome.decorate_pane(&mut plain, rect, 0, 64));
    }

    #[test]
    fn compact_geometry_caches_nested_t_neighbors_and_odd_pixel_ownership() {
        let size = Size {
            cols: 40,
            rows: 24,
            cell_width: 9,
            cell_height: 20,
        };
        let rects = vec![
            (
                "left".into(),
                Rect {
                    x: 0,
                    y: 0,
                    width: 21,
                    height: 24,
                },
            ),
            (
                "upper".into(),
                Rect {
                    x: 20,
                    y: 0,
                    width: 20,
                    height: 13,
                },
            ),
            (
                "lower".into(),
                Rect {
                    x: 20,
                    y: 12,
                    width: 20,
                    height: 12,
                },
            ),
        ];
        let mut cache = PaneGeometry::default();
        cache.prepare(size, &rects);
        assert_eq!(
            cache.placements,
            [
                DecorationGeometry {
                    bounds_px: [0, -11, 189, 491],
                    clip_px: [0, 0, 184, 480]
                },
                DecorationGeometry {
                    bounds_px: [180, -11, 360, 265],
                    clip_px: [184, 0, 360, 250]
                },
                DecorationGeometry {
                    bounds_px: [180, 234, 360, 491],
                    clip_px: [184, 250, 360, 480]
                },
            ]
        );
        let rect_storage = cache.rects.as_ptr();
        let placement_storage = cache.placements.as_ptr();
        cache.prepare(size, &rects);
        assert_eq!(cache.rects.as_ptr(), rect_storage);
        assert_eq!(cache.placements.as_ptr(), placement_storage);
        // The fork saturates excess height: unusually wide cells never produce
        // inverse contour translations or unsigned subtraction underflow.
        cache.prepare(
            Size {
                cell_width: 20,
                cell_height: 9,
                ..size
            },
            &rects,
        );
        assert_eq!(cache.placements[1].bounds_px, [400, 0, 800, 117]);
        assert_eq!(cache.placements[2].bounds_px, [400, 108, 800, 216]);
    }

    #[test]
    fn corner_only_contacts_do_not_translate_or_clip_a_straight_edge() {
        let size = Size {
            cols: 20,
            rows: 20,
            cell_width: 9,
            cell_height: 20,
        };
        let rects = vec![
            (
                "upper".into(),
                Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
            ),
            (
                "corner".into(),
                Rect {
                    x: 9,
                    y: 9,
                    width: 10,
                    height: 10,
                },
            ),
        ];
        let mut cache = PaneGeometry::default();
        cache.prepare(size, &rects);
        assert_eq!(
            cache.placements[0],
            DecorationGeometry {
                bounds_px: [0, -11, 90, 211],
                clip_px: [0, 0, 90, 200],
            }
        );
    }

    #[test]
    fn unready_artwork_preserves_marker_label_and_cursor() {
        let chrome = ChromeGraphics::new().unwrap();
        let mut frame = bkpr_tui_framework::blank(Size {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        });
        bkpr_tui_framework::text(&mut frame, 1, 0, "● label", false);
        let original = frame.clone();
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Bee,
            1,
            0,
            Some(ColorSource::Default),
            1
        ));
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Hive,
            1,
            0,
            Some(ColorSource::Default),
            2
        ));
        assert_eq!(frame, original);
    }
    #[test]
    fn hierarchy_gutters_preserve_text_markers_until_artwork_is_ready() {
        let chrome = ChromeGraphics::new().unwrap();
        let mut frame = bkpr_tui_framework::blank(Size {
            cols: 28,
            rows: 3,
            cell_width: 10,
            cell_height: 20,
        });
        bkpr_tui_framework::text(&mut frame, 0, 0, " ● Workspace", false);
        bkpr_tui_framework::text(&mut frame, 0, 1, "    ● Terminal", false);
        bkpr_tui_framework::text(&mut frame, 0, 2, "    ◌ Hidden", false);
        let original = frame.clone();
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Hive,
            1,
            0,
            Some(ColorSource::Default),
            1
        ));
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Bee,
            4,
            1,
            Some(ColorSource::Default),
            2
        ));
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::BeeHidden,
            4,
            2,
            Some(ColorSource::Default),
            3
        ));
        assert_eq!(frame, original);
    }
}
