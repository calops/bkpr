use super::*;

const MAX_CANONICAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_RASTER_WORKING_BYTES: usize = 128 * 1024 * 1024;
// Tagged Kitty 0.49.2 rejects uploaded image dimensions above 10000. Larger
// visible projections are represented by bounded, phase-preserving tiles.
const MAX_TILE_DIMENSION: u32 = 10000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct DerivedKey {
    pub image: ImageKey,
    pub source: Rect,
    pub width: u32,
    pub height: u32,
    pub visible: Rect,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Projection {
    pub image: ImageKey,
    pub derived: Option<DerivedKey>,
    pub source: Rect,
    pub col: u32,
    pub row: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub cells: Option<(u32, u32)>,
    pub z: i32,
    pub placement_id: usize,
}
pub(super) struct Prepared<'a> {
    pub images: HashMap<ImageKey, &'a Image>,
    pub original: HashSet<ImageKey>,
    pub derived: HashSet<DerivedKey>,
    pub placements: Vec<Projection>,
}
fn bytes(width: u32, height: u32, channels: usize) -> Result<usize> {
    usize::try_from(width)?
        .checked_mul(usize::try_from(height)?)
        .and_then(|count| count.checked_mul(channels))
        .context("image byte dimensions overflow")
}
fn scale(value: u32, destination_cell: u32, canonical_cell: u32) -> Result<i64> {
    i64::try_from(u64::from(value) * u64::from(destination_cell) / u64::from(canonical_cell))
        .context("projected image dimension overflow")
}

/// A one-pixel source axis is constant, so clipping it cannot change its
/// sampling phase. Keep straight material strips on the uploaded atlas rather
/// than expanding them into newly rasterized resources after occlusion.
/// Other resampling retains the existing phase-preserving derived path.
pub(super) fn reuse_constant_axes(projection: &mut Projection, viewport: Size) {
    let Some(key) = projection.derived else {
        return;
    };
    let axis = |start, source, full, visible_start, visible| {
        if source == 1 {
            Some((start, 1))
        } else if source == full {
            Some((start + visible_start, visible))
        } else {
            None
        }
    };
    let Some((x, width)) = axis(
        key.source.x,
        key.source.width,
        key.width,
        key.visible.x,
        key.visible.width,
    ) else {
        return;
    };
    let Some((y, height)) = axis(
        key.source.y,
        key.source.height,
        key.height,
        key.visible.y,
        key.visible.height,
    ) else {
        return;
    };
    let natural = width == key.visible.width && height == key.visible.height;
    let columns = u64::from(key.visible.width) + u64::from(projection.offset_x);
    let rows = u64::from(key.visible.height) + u64::from(projection.offset_y);
    if !natural
        && (!columns.is_multiple_of(u64::from(viewport.cell_width))
            || !rows.is_multiple_of(u64::from(viewport.cell_height)))
    {
        return;
    }
    projection.source = Rect {
        x,
        y,
        width,
        height,
    };
    projection.cells = (!natural).then_some((
        (columns / u64::from(viewport.cell_width)) as u32,
        (rows / u64::from(viewport.cell_height)) as u32,
    ));
    projection.derived = None;
}
fn project(
    p: &Placement,
    canonical: Size,
    viewport: Size,
    placement_id: usize,
) -> Result<Option<Projection>> {
    let width = scale(p.width, viewport.cell_width, canonical.cell_width)?;
    let height = scale(p.height, viewport.cell_height, canonical.cell_height)?;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let x = (i64::from(p.col) * i64::from(viewport.cell_width))
        .checked_add(scale(
            p.offset_x,
            viewport.cell_width,
            canonical.cell_width,
        )?)
        .context("projected image position overflow")?;
    let y = (i64::from(p.row) * i64::from(viewport.cell_height))
        .checked_add(scale(
            p.offset_y,
            viewport.cell_height,
            canonical.cell_height,
        )?)
        .context("projected image position overflow")?;
    let left = x.max(0);
    let top = y.max(0);
    let right = x
        .checked_add(width)
        .context("projected image edge overflow")?
        .min(i64::from(viewport.cols) * i64::from(viewport.cell_width));
    let bottom = y
        .checked_add(height)
        .context("projected image edge overflow")?
        .min(i64::from(viewport.rows) * i64::from(viewport.cell_height));
    if right <= left || bottom <= top {
        return Ok(None);
    }
    let visible_width = u32::try_from(right - left)?;
    let visible_height = u32::try_from(bottom - top)?;
    let clip_x = u32::try_from(left - x)?;
    let clip_y = u32::try_from(top - y)?;
    let full_width = u32::try_from(width)?;
    let full_height = u32::try_from(height)?;
    let offset_x = u32::try_from(left)? % viewport.cell_width;
    let offset_y = u32::try_from(top)? % viewport.cell_height;
    let source_x = u64::from(p.source_width) * u64::from(clip_x);
    let source_y = u64::from(p.source_height) * u64::from(clip_y);
    let source_width = u64::from(p.source_width) * u64::from(visible_width);
    let source_height = u64::from(p.source_height) * u64::from(visible_height);
    let exact_source = source_x.is_multiple_of(u64::from(full_width))
        && source_width.is_multiple_of(u64::from(full_width))
        && source_y.is_multiple_of(u64::from(full_height))
        && source_height.is_multiple_of(u64::from(full_height));
    let natural = exact_source
        && source_width / full_width as u64 == visible_width as u64
        && source_height / full_height as u64 == visible_height as u64;
    let columns = u64::from(visible_width) + u64::from(offset_x);
    let rows = u64::from(visible_height) + u64::from(offset_y);
    // Kitty's explicit cell rectangle ends at a cell boundary. X/Y reduce
    // its drawable width/height; ceil would silently enlarge other rectangles.
    let exact_cells = exact_source
        && columns.is_multiple_of(u64::from(viewport.cell_width))
        && rows.is_multiple_of(u64::from(viewport.cell_height));
    let image = ImageKey {
        id: p.image_id,
        generation: p.generation,
    };
    let derived = (!natural && !exact_cells).then_some(DerivedKey {
        image,
        source: Rect {
            x: p.source_x,
            y: p.source_y,
            width: p.source_width,
            height: p.source_height,
        },
        width: full_width,
        height: full_height,
        visible: Rect {
            x: clip_x,
            y: clip_y,
            width: visible_width,
            height: visible_height,
        },
    });
    let source = if derived.is_some() {
        Rect {
            x: 0,
            y: 0,
            width: visible_width,
            height: visible_height,
        }
    } else {
        Rect {
            x: p.source_x + u32::try_from(source_x / full_width as u64)?,
            y: p.source_y + u32::try_from(source_y / full_height as u64)?,
            width: u32::try_from(source_width / full_width as u64)?,
            height: u32::try_from(source_height / full_height as u64)?,
        }
    };
    let mut projection = Projection {
        image,
        derived,
        source,
        col: u32::try_from(left)? / viewport.cell_width,
        row: u32::try_from(top)? / viewport.cell_height,
        offset_x,
        offset_y,
        cells: if !natural && exact_cells {
            Some((
                u32::try_from(columns / viewport.cell_width as u64)?,
                u32::try_from(rows / viewport.cell_height as u64)?,
            ))
        } else {
            None
        },
        z: match p.layer {
            ImageLayer::BelowBackground => p.z.min(i32::MIN / 2 - 1),
            ImageLayer::BelowText => p.z.clamp(i32::MIN / 2, -1),
            ImageLayer::AboveText => p.z.max(0),
        },
        placement_id,
    };
    reuse_constant_axes(&mut projection, viewport);
    Ok(Some(projection))
}
pub(super) fn prepare(frame: &Frame, viewport: Size) -> Result<Prepared<'_>> {
    if frame.size.cell_width == 0
        || frame.size.cell_height == 0
        || viewport.cell_width == 0
        || viewport.cell_height == 0
    {
        bail!("zero image projection cell dimension");
    }
    let mut images = HashMap::with_capacity(frame.images.len());
    let mut canonical_bytes = 0usize;
    for image in &frame.images {
        if image.width == 0
            || image.height == 0
            || image.width > 8192
            || image.height > 8192
            || image.pixels.len()
                != bytes(image.width, image.height, image.format.bytes_per_pixel())?
        {
            bail!("invalid image resource");
        }
        canonical_bytes = canonical_bytes
            .checked_add(image.pixels.len())
            .context("image byte total overflow")?;
        if canonical_bytes > MAX_CANONICAL_BYTES {
            bail!("aggregate image byte limit");
        }
        if images.insert(image.key(), image.as_ref()).is_some() {
            bail!("duplicate image resource key");
        }
    }
    let mut original = HashSet::new();
    let mut derived = HashSet::new();
    let mut placements = Vec::with_capacity(frame.placements.len());
    let mut derived_bytes = 0usize;
    for (index, p) in frame.placements.iter().enumerate() {
        let key = ImageKey {
            id: p.image_id,
            generation: p.generation,
        };
        let image = images
            .get(&key)
            .context("placement references missing resource")?;
        if p.source_width == 0
            || p.source_height == 0
            || u64::from(p.source_x) + u64::from(p.source_width) > u64::from(image.width)
            || u64::from(p.source_y) + u64::from(p.source_height) > u64::from(image.height)
        {
            bail!("placement source rectangle outside image");
        }
        let Some(projected) = project(p, frame.size, viewport, index + 1)? else {
            continue;
        };
        let (columns, rows) = projected.derived.map_or((1, 1), |key| {
            (
                key.visible.width.div_ceil(MAX_TILE_DIMENSION),
                key.visible.height.div_ceil(MAX_TILE_DIMENSION),
            )
        });
        for tile_y in 0..rows {
            for tile_x in 0..columns {
                let mut tile = projected;
                if let Some(key) = tile.derived {
                    let dx = tile_x * MAX_TILE_DIMENSION;
                    let dy = tile_y * MAX_TILE_DIMENSION;
                    let visible = Rect {
                        x: key.visible.x + dx,
                        y: key.visible.y + dy,
                        width: (key.visible.width - dx).min(MAX_TILE_DIMENSION),
                        height: (key.visible.height - dy).min(MAX_TILE_DIMENSION),
                    };
                    let key = DerivedKey { visible, ..key };
                    tile.derived = Some(key);
                    tile.source = Rect {
                        x: 0,
                        y: 0,
                        width: visible.width,
                        height: visible.height,
                    };
                    let left = u64::from(projected.col) * u64::from(viewport.cell_width)
                        + u64::from(projected.offset_x)
                        + u64::from(dx);
                    let top = u64::from(projected.row) * u64::from(viewport.cell_height)
                        + u64::from(projected.offset_y)
                        + u64::from(dy);
                    tile.col = u32::try_from(left / u64::from(viewport.cell_width))?;
                    tile.row = u32::try_from(top / u64::from(viewport.cell_height))?;
                    tile.offset_x = u32::try_from(left % u64::from(viewport.cell_width))?;
                    tile.offset_y = u32::try_from(top % u64::from(viewport.cell_height))?;
                    if derived.insert(key) {
                        derived_bytes = derived_bytes
                            .checked_add(bytes(
                                visible.width,
                                visible.height,
                                image.format.bytes_per_pixel(),
                            )?)
                            .context("derived image byte total overflow")?;
                    }
                } else {
                    original.insert(key);
                }
                // Bound visible raster bytes plus planning metadata before any
                // pixel buffer allocation. The factor two covers Vec growth
                // and hash bucket slack; offscreen dimensions cost no raster.
                let metadata = placements
                    .capacity()
                    .max(placements.len() + 1)
                    .checked_mul(2 * std::mem::size_of::<Projection>())
                    .and_then(|total| {
                        total.checked_add((derived.capacity() + 1).checked_mul(
                            2 * (std::mem::size_of::<DerivedKey>() + std::mem::size_of::<usize>()),
                        )?)
                    })
                    .and_then(|total| {
                        total.checked_add(
                            (images.capacity() + original.capacity() + 2).checked_mul(
                                2 * (std::mem::size_of::<ImageKey>()
                                    + std::mem::size_of::<usize>()),
                            )?,
                        )
                    })
                    .context("projection metadata byte total overflow")?;
                let working_bytes = canonical_bytes
                    .checked_add(derived_bytes)
                    .and_then(|total| total.checked_add(metadata))
                    .context("raster working set overflow")?;
                if working_bytes > MAX_RASTER_WORKING_BYTES {
                    bail!("aggregate visible projection raster and metadata byte limit");
                }
                tile.placement_id = placements.len() + 1;
                placements.push(tile);
            }
        }
    }
    Ok(Prepared {
        images,
        original,
        derived,
        placements,
    })
}
fn sample_visible<P: image::Pixel<Subpixel = u8>>(
    image: &Image,
    key: DerivedKey,
) -> Result<Vec<u8>> {
    let pixels =
        image::ImageBuffer::<P, _>::from_raw(image.width, image.height, image.pixels.as_slice())
            .context("invalid borrowed image buffer")?;
    let cropped = image::imageops::crop_imm(
        &pixels,
        key.source.x,
        key.source.y,
        key.source.width,
        key.source.height,
    );
    let mut output = Vec::with_capacity(bytes(
        key.visible.width,
        key.visible.height,
        image.format.bytes_per_pixel(),
    )?);
    // Use the maintained sampler's normalized coordinates and nearest/tie
    // semantics. Global destination pixel centers preserve the full placement's
    // sampling phase across viewport clipping and tile boundaries. No full
    // offscreen image, crop copy, float raster or custom kernel is computed.
    for y in 0..key.visible.height {
        let v = ((f64::from(key.visible.y) + f64::from(y) + 0.5) / f64::from(key.height)) as f32;
        for x in 0..key.visible.width {
            let u = ((f64::from(key.visible.x) + f64::from(x) + 0.5) / f64::from(key.width)) as f32;
            let pixel = image::imageops::sample_nearest(&*cropped, u, v)
                .context("sampling visible image coordinate")?;
            output.extend_from_slice(pixel.channels());
        }
    }
    Ok(output)
}
pub(super) fn raster(image: &Image, key: DerivedKey) -> Result<Vec<u8>> {
    match image.format {
        PixelFormat::Rgb => sample_visible::<image::Rgb<u8>>(image, key),
        PixelFormat::Rgba => sample_visible::<image::Rgba<u8>>(image, key),
        PixelFormat::Gray => sample_visible::<image::Luma<u8>>(image, key),
        PixelFormat::GrayAlpha => sample_visible::<image::LumaA<u8>>(image, key),
    }
}

#[cfg(test)]
mod constant_axis_tests {
    use super::*;

    fn viewport() -> Size {
        Size {
            cols: 20,
            rows: 10,
            cell_width: 8,
            cell_height: 16,
        }
    }

    fn projection(key: DerivedKey) -> Projection {
        Projection {
            image: key.image,
            derived: Some(key),
            source: Rect {
                x: 0,
                y: 0,
                width: key.visible.width,
                height: key.visible.height,
            },
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            cells: None,
            z: 1,
            placement_id: 1,
        }
    }

    #[test]
    fn clipped_constant_axes_reuse_the_exact_original_pixels_in_both_directions() {
        let image = Image {
            id: 1,
            generation: 2,
            width: 32,
            height: 32,
            format: PixelFormat::Rgba,
            pixels: (0..32)
                .flat_map(|y| (0..32).flat_map(move |x| [x as u8, y as u8, 100, 255]))
                .collect(),
        };
        for (source, width, height, visible) in [
            (
                Rect {
                    x: 2,
                    y: 4,
                    width: 1,
                    height: 24,
                },
                96,
                24,
                Rect {
                    x: 7,
                    y: 3,
                    width: 80,
                    height: 16,
                },
            ),
            (
                Rect {
                    x: 4,
                    y: 2,
                    width: 24,
                    height: 1,
                },
                24,
                96,
                Rect {
                    x: 3,
                    y: 7,
                    width: 16,
                    height: 80,
                },
            ),
        ] {
            let key = DerivedKey {
                image: image.key(),
                source,
                width,
                height,
                visible,
            };
            let mut actual = projection(key);
            reuse_constant_axes(&mut actual, viewport());
            assert!(actual.derived.is_none());
            assert!(actual.cells.is_some());
            let from_atlas = DerivedKey {
                source: actual.source,
                width: visible.width,
                height: visible.height,
                visible: Rect {
                    x: 0,
                    y: 0,
                    ..visible
                },
                ..key
            };
            assert_eq!(
                raster(&image, key).unwrap(),
                raster(&image, from_atlas).unwrap()
            );
        }
    }

    #[test]
    fn fractional_other_axis_and_non_cell_ends_keep_phase_preserving_raster() {
        let key = DerivedKey {
            image: ImageKey {
                id: 1,
                generation: 2,
            },
            source: Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 24,
            },
            width: 96,
            height: 48,
            visible: Rect {
                x: 7,
                y: 3,
                width: 80,
                height: 16,
            },
        };
        let mut actual = projection(key);
        reuse_constant_axes(&mut actual, viewport());
        assert_eq!(actual.derived, Some(key));
        let key = DerivedKey {
            height: 24,
            visible: Rect {
                width: 79,
                ..key.visible
            },
            ..key
        };
        let mut actual = projection(key);
        reuse_constant_axes(&mut actual, viewport());
        assert_eq!(actual.derived, Some(key));
    }

    #[test]
    fn viewport_clipping_of_a_stretched_strip_needs_no_derived_resource() {
        let p = Placement {
            image_id: 1,
            generation: 2,
            placement_id: 1,
            col: -1,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            width: 96,
            height: 16,
            source_x: 4,
            source_y: 3,
            source_width: 1,
            source_height: 16,
            layer: ImageLayer::AboveText,
            z: 0,
        };
        let actual = project(&p, viewport(), viewport(), 1).unwrap().unwrap();
        assert!(actual.derived.is_none());
        assert_eq!(
            actual.source,
            Rect {
                x: 4,
                y: 3,
                width: 1,
                height: 16
            }
        );
        assert_eq!(actual.cells, Some((11, 1)));
    }
}
