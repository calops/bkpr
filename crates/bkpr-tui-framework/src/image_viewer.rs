//! Full-resource image viewing and canonical scene image picking.
use crate::{Rect, Role, RoleSpan, Theme, UiSurface, window};
use bkpr_protocol::{Frame, Image, ImageLayer, Placement, Size};
use std::sync::Arc;

/// A caller-controlled viewer retaining the original immutable image resource.
/// Input, visibility and surface ownership remain with the caller.
pub struct ImageViewer {
    image: Arc<Image>,
    title: String,
}

impl ImageViewer {
    /// Retain an image without decoding, resizing or copying its pixel bytes.
    pub fn new(image: Arc<Image>) -> Self {
        let title = format!("Image · {}×{}", image.width, image.height);
        Self { image, title }
    }

    /// Paint a rounded, themed viewer using the surface's current dimensions.
    /// The full image is centered and aspect-fit in physical pixels, including
    /// upscaling. Subpixel dimensions round down to at least one pixel.
    /// Below 3×3 cells the border and labels are omitted, except the close button.
    /// Empty images or zero-sized physical content produce no image placement.
    pub fn paint(&self, surface: &mut UiSurface, theme: &Theme) {
        let size = surface.frame.size;
        let decorated = size.cols >= 3 && size.rows >= 3;
        let mut lines = window::border_rows(size, "", window::Border::Rounded);
        let mut spans = window::theme_spans(size).to_vec();
        if decorated {
            let title = Rect {
                x: 1,
                y: 0,
                width: size.cols.saturating_sub(4),
                height: 1,
            };
            lines.push((title, window::fit(&self.title, title.width), false));
            spans.push(RoleSpan {
                rect: title,
                role: Role::Title,
            });
            let hint = Rect {
                x: 1,
                y: size.rows - 1,
                width: size.cols - 2,
                height: 1,
            };
            lines.push((hint, window::fit("Esc close", hint.width), false));
            spans.push(RoleSpan {
                rect: hint,
                role: Role::Muted,
            });
        }
        if let Some(button) = close_rect(size) {
            lines.push((button, "×".into(), false));
            spans.push(RoleSpan {
                rect: button,
                role: Role::Title,
            });
        }
        surface.paint(size, lines, None);
        surface.apply_theme(theme, Role::Panel, &spans);
        surface.frame.images.clear();
        surface.frame.placements.clear();
        if let Some(placement) = aspect_fit(&self.image, size) {
            surface.frame.images.push(Arc::clone(&self.image));
            surface.frame.placements.push(placement);
        }
    }
}

/// Pick the topmost image at fractional canonical frame-cell coordinates.
/// Off-frame/non-finite points, empty or invalid source rectangles, missing
/// generations and below-background placements are not selectable. Placement
/// offsets, destination extents and viewport clipping determine the hit area;
/// source crops select the resource, not a cropped copy. Layer bands and z values
/// match scene projection, with later placements winning equal-order ties.
/// This is rectangular picking; alpha and overlaid text do not mask image hits.
pub fn hit_test(frame: &Frame, x: f32, y: f32) -> Option<Arc<Image>> {
    let size = frame.size;
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f32::from(size.cols)
        || y >= f32::from(size.rows)
        || size.cell_width == 0
        || size.cell_height == 0
    {
        return None;
    }
    let x = f64::from(x) * f64::from(size.cell_width);
    let y = f64::from(y) * f64::from(size.cell_height);
    frame
        .placements
        .iter()
        .enumerate()
        .filter_map(|(index, p)| {
            let z = match p.layer {
                ImageLayer::BelowBackground => return None,
                ImageLayer::BelowText => p.z.clamp(i32::MIN / 2, -1),
                ImageLayer::AboveText => p.z.max(0),
            };
            let left = f64::from(p.col) * f64::from(size.cell_width) + f64::from(p.offset_x);
            let top = f64::from(p.row) * f64::from(size.cell_height) + f64::from(p.offset_y);
            if p.width == 0
                || p.height == 0
                || x < left
                || x >= left + f64::from(p.width)
                || y < top
                || y >= top + f64::from(p.height)
            {
                return None;
            }
            let image = frame
                .images
                .iter()
                .find(|image| image.id == p.image_id && image.generation == p.generation)?;
            if p.source_width == 0
                || p.source_height == 0
                || u64::from(p.source_x) + u64::from(p.source_width) > u64::from(image.width)
                || u64::from(p.source_y) + u64::from(p.source_height) > u64::from(image.height)
            {
                return None;
            }
            Some(((z, index), image))
        })
        .max_by_key(|(order, _)| *order)
        .map(|(_, image)| Arc::clone(image))
}

/// Return whether a local cell is the viewer's top-right close button.
/// Decorated views keep the corner intact, placing the button one cell left;
/// tiny views use the final column. Empty views have no button.
pub fn close_hit(size: Size, x: u16, y: u16) -> bool {
    close_rect(size).is_some_and(|rect| x == rect.x && y == rect.y)
}

fn close_rect(size: Size) -> Option<Rect> {
    if size.cols == 0 || size.rows == 0 {
        return None;
    }
    Some(Rect {
        x: size.cols
            - if size.cols >= 3 && size.rows >= 3 {
                2
            } else {
                1
            },
        y: 0,
        width: 1,
        height: 1,
    })
}

fn aspect_fit(image: &Image, size: Size) -> Option<Placement> {
    let content = window::content(size);
    let available_width = u32::from(content.width).checked_mul(size.cell_width)?;
    let available_height = u32::from(content.height).checked_mul(size.cell_height)?;
    if image.width == 0 || image.height == 0 || available_width == 0 || available_height == 0 {
        return None;
    }
    let (width, height) = if u64::from(available_width) * u64::from(image.height)
        <= u64::from(available_height) * u64::from(image.width)
    {
        (
            available_width,
            (u64::from(available_width) * u64::from(image.height) / u64::from(image.width)).max(1)
                as u32,
        )
    } else {
        (
            (u64::from(available_height) * u64::from(image.width) / u64::from(image.height)).max(1)
                as u32,
            available_height,
        )
    };
    let x = (available_width - width) / 2;
    let y = (available_height - height) / 2;
    Some(Placement {
        image_id: image.id,
        generation: image.generation,
        placement_id: 1,
        col: i32::from(content.x) + (x / size.cell_width) as i32,
        row: i32::from(content.y) + (y / size.cell_height) as i32,
        offset_x: x % size.cell_width,
        offset_y: y % size.cell_height,
        width,
        height,
        source_x: 0,
        source_y: 0,
        source_width: image.width,
        source_height: image.height,
        layer: ImageLayer::BelowText,
        z: -1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::PixelFormat;

    fn size() -> Size {
        Size {
            cols: 12,
            rows: 8,
            cell_width: 10,
            cell_height: 20,
        }
    }

    fn image(id: u32, format: PixelFormat) -> Arc<Image> {
        Arc::new(Image {
            id,
            generation: 7,
            width: 40,
            height: 20,
            format,
            pixels: vec![128; 40 * 20 * format.bytes_per_pixel()],
        })
    }

    #[test]
    fn physical_fit_preserves_original_resources_for_every_format() {
        for format in [
            PixelFormat::Rgb,
            PixelFormat::Rgba,
            PixelFormat::GrayAlpha,
            PixelFormat::Gray,
        ] {
            let image = image(1, format);
            let viewer = ImageViewer::new(Arc::clone(&image));
            let mut surface = UiSurface::new(size());
            viewer.paint(&mut surface, &Theme::default());
            assert!(Arc::ptr_eq(&surface.frame.images[0], &image));
            let p = &surface.frame.placements[0];
            assert_eq!((p.width, p.height), (100, 50));
            assert_eq!((p.col, p.row, p.offset_x, p.offset_y), (1, 2, 0, 15));
            assert_eq!(
                (p.source_x, p.source_y, p.source_width, p.source_height),
                (0, 0, 40, 20)
            );
            assert!(!surface.frame.cursor.visible);
        }
    }

    #[test]
    fn tiny_and_empty_views_stay_bounded() {
        let image = image(1, PixelFormat::Rgb);
        let viewer = ImageViewer::new(image);
        for cols in 0..4 {
            for rows in 0..4 {
                let size = Size {
                    cols,
                    rows,
                    ..size()
                };
                let mut surface = UiSurface::new(size);
                viewer.paint(&mut surface, &Theme::default());
                for p in &surface.frame.placements {
                    let right = p.col as u32 * size.cell_width + p.offset_x + p.width;
                    let bottom = p.row as u32 * size.cell_height + p.offset_y + p.height;
                    assert!(right <= u32::from(cols) * size.cell_width);
                    assert!(bottom <= u32::from(rows) * size.cell_height);
                }
                assert_eq!(close_rect(size).is_some(), cols > 0 && rows > 0);
                if let Some(rect) = close_rect(size) {
                    assert!(close_hit(size, rect.x, rect.y));
                    assert!(!close_hit(size, rect.x, 1));
                }
            }
        }
    }

    #[test]
    fn picking_clips_offsets_and_orders_visible_layers() {
        let mut frame = crate::blank(size());
        let first = image(1, PixelFormat::Rgb);
        let second = image(2, PixelFormat::Rgba);
        frame.images = vec![Arc::clone(&first), Arc::clone(&second)];
        let mut p = aspect_fit(&first, size()).unwrap();
        p.col = -1;
        p.row = 0;
        p.offset_x = 5;
        p.offset_y = 10;
        p.width = 20;
        p.height = 20;
        p.source_x = 10;
        p.source_width = 10;
        p.z = i32::MAX;
        frame.placements.push(p.clone());
        assert!(hit_test(&frame, -0.1, 0.75).is_none());
        assert!(hit_test(&frame, 0.0, 0.49).is_none());
        assert!(Arc::ptr_eq(&hit_test(&frame, 0.0, 0.5).unwrap(), &first));
        assert!(hit_test(&frame, 1.5, 0.75).is_none());
        p.image_id = second.id;
        p.layer = ImageLayer::AboveText;
        p.z = i32::MIN;
        frame.placements.push(p.clone());
        assert!(Arc::ptr_eq(&hit_test(&frame, 0.0, 0.75).unwrap(), &second));
        p.image_id = first.id;
        frame.placements.push(p.clone());
        assert!(Arc::ptr_eq(&hit_test(&frame, 0.0, 0.75).unwrap(), &first));
        p.image_id = second.id;
        p.layer = ImageLayer::BelowBackground;
        frame.placements.push(p);
        assert!(Arc::ptr_eq(&hit_test(&frame, 0.0, 0.75).unwrap(), &first));
        assert!(hit_test(&frame, f32::NAN, 0.75).is_none());
        assert!(hit_test(&frame, 0.0, f32::INFINITY).is_none());
    }
}
