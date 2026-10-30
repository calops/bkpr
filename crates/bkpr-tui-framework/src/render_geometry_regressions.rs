use super::*;
use bkpr_terminal_engine::Terminal;

fn scene() -> Frame {
    Frame {
        size: Size {
            cols: 12,
            rows: 8,
            cell_width: 8,
            cell_height: 16,
        },
        rows: Vec::new(),
        cursor: Cursor {
            x: 0,
            y: 0,
            visible: false,
            blinking: false,
            shape: CursorShape::Block,
            color: None,
        },
        foreground: [255; 3],
        background: [0; 3],
        palette: vec![[0; 3]; 256],
        images: Vec::new(),
        placements: Vec::new(),
        alternate_screen: false,
        scrollback_total: 0,
        scrollback_offset: 0,
    }
}
fn image(id: u32, width: u32, height: u32, pixels: Vec<u8>) -> Arc<Image> {
    Arc::new(Image {
        id,
        generation: 1,
        width,
        height,
        format: PixelFormat::Rgba,
        pixels,
    })
}
fn placement(id: u32, width: u32, height: u32) -> Placement {
    Placement {
        image_id: id,
        generation: 1,
        placement_id: id,
        col: 1,
        row: 1,
        offset_x: 0,
        offset_y: 0,
        width,
        height,
        source_x: 0,
        source_y: 0,
        source_width: width,
        source_height: height,
        layer: ImageLayer::AboveText,
        z: 0,
    }
}
fn consumer(frame: &Frame) -> Terminal {
    Terminal::new(Size {
        rows: frame.size.rows + 1,
        ..frame.size
    })
    .unwrap()
}
fn consume(frame: &Frame, graphics: &mut GraphicsState, terminal: &mut Terminal) -> Frame {
    let mut output = Vec::new();
    draw(
        &mut output,
        frame,
        frame.size,
        "",
        &ColorProbe::default(),
        graphics,
    )
    .unwrap();
    terminal.feed(&output).unwrap();
    terminal.snapshot().unwrap()
}
fn placed_image<'a>(frame: &'a Frame, p: &Placement) -> &'a Image {
    frame
        .images
        .iter()
        .find(|image| image.id == p.image_id && image.generation == p.generation)
        .unwrap()
}
fn pixel(image: &Image, x: u32, y: u32) -> [u8; 4] {
    assert_eq!(image.format, PixelFormat::Rgba);
    let index = (y as usize * image.width as usize + x as usize) * 4;
    image.pixels[index..index + 4].try_into().unwrap()
}
fn corners() -> Vec<u8> {
    let mut pixels = vec![0; 4 * 4 * 4];
    for (x, y, rgba) in [
        (1, 1, [255, 0, 0, 255]),
        (2, 1, [0, 255, 0, 255]),
        (1, 2, [0, 0, 255, 255]),
        (2, 2, [255, 255, 0, 255]),
    ] {
        let index = (y * 4 + x) * 4;
        pixels[index..index + 4].copy_from_slice(&rgba);
    }
    pixels
}

#[test]
fn below_background_depth_changes_preserve_cached_resource_ordering() {
    let mut frame = scene();
    frame.images = vec![
        image(11, 2, 2, [255, 0, 0, 255].repeat(4)),
        image(22, 2, 2, [0, 0, 255, 255].repeat(4)),
    ];
    for (id, z) in [(11, i32::MIN + 100), (22, i32::MIN + 200)] {
        let mut p = placement(id, 2, 2);
        p.layer = ImageLayer::BelowBackground;
        p.z = z;
        frame.placements.push(p);
    }
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let first = consume(&frame, &mut graphics, &mut terminal);
    assert!(
        first
            .placements
            .iter()
            .all(|p| p.layer == ImageLayer::BelowBackground)
    );
    let top = first.placements.iter().max_by_key(|p| p.z).unwrap();
    assert_eq!(top.z, i32::MIN + 200);
    assert_eq!(pixel(placed_image(&first, top), 0, 0), [0, 0, 255, 255]);
    let resource_ids = graphics.image_ids.clone();
    let mut first_keys = first
        .images
        .iter()
        .map(|image| image.key())
        .collect::<Vec<_>>();
    first_keys.sort_by_key(|key| key.id);
    frame.placements[0].z = i32::MIN + 300;
    let second = consume(&frame, &mut graphics, &mut terminal);
    let top = second.placements.iter().max_by_key(|p| p.z).unwrap();
    assert_eq!(top.z, i32::MIN + 300);
    assert_eq!(pixel(placed_image(&second, top), 0, 0), [255, 0, 0, 255]);
    assert_eq!(graphics.image_ids, resource_ids);
    assert!(graphics.derived_ids.is_empty());
    let mut second_keys = second
        .images
        .iter()
        .map(|image| image.key())
        .collect::<Vec<_>>();
    second_keys.sort_by_key(|key| key.id);
    assert_eq!(first_keys, second_keys);
}

#[test]
fn below_text_wire_depth_cannot_cross_into_background_layer() {
    let mut frame = scene();
    frame.images.push(image(1, 1, 1, vec![255, 0, 255, 255]));
    let mut p = placement(1, 1, 1);
    p.layer = ImageLayer::BelowText;
    p.z = i32::MIN;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    assert_eq!(actual.placements[0].layer, ImageLayer::BelowText);
    assert_eq!(actual.placements[0].z, i32::MIN / 2);
}

#[test]
fn natural_single_pixel_extent_and_subcell_offset_are_not_cell_scaled() {
    let mut frame = scene();
    frame.images.push(image(1, 1, 1, vec![10, 20, 30, 255]));
    let mut p = placement(1, 1, 1);
    p.offset_x = 3;
    p.offset_y = 5;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    let p = &actual.placements[0];
    assert_eq!(
        (p.col, p.row, p.offset_x, p.offset_y, p.width, p.height),
        (1, 1, 3, 5, 1, 1)
    );
    assert_eq!(pixel(placed_image(&actual, p), 0, 0), [10, 20, 30, 255]);
    assert!(graphics.derived_ids.is_empty());
    assert_eq!(frame.images[0].pixels, vec![10, 20, 30, 255]);
}

#[test]
fn natural_cropped_extent_preserves_original_resource_pixels() {
    let mut frame = scene();
    frame.images.push(image(1, 4, 4, corners()));
    let mut p = placement(1, 2, 2);
    p.source_x = 1;
    p.source_y = 1;
    p.offset_x = 3;
    p.offset_y = 5;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    let p = &actual.placements[0];
    assert_eq!(
        (
            p.width,
            p.height,
            p.source_x,
            p.source_y,
            p.source_width,
            p.source_height
        ),
        (2, 2, 1, 1, 2, 2)
    );
    assert_eq!(placed_image(&actual, p).pixels, frame.images[0].pixels);
    assert!(graphics.derived_ids.is_empty());
}

#[test]
fn exact_cell_endpoint_scaling_with_offsets_keeps_original_resource() {
    let mut frame = scene();
    frame.images.push(image(1, 4, 4, corners()));
    let mut p = placement(1, 13, 27);
    p.source_x = 1;
    p.source_y = 1;
    p.source_width = 2;
    p.source_height = 2;
    p.offset_x = 3;
    p.offset_y = 5;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    let p = &actual.placements[0];
    assert_eq!((p.offset_x, p.offset_y, p.width, p.height), (3, 5, 13, 27));
    assert_eq!(placed_image(&actual, p).pixels, frame.images[0].pixels);
    assert!(graphics.derived_ids.is_empty());
}

#[test]
fn subcell_scaled_crop_is_exact_cached_and_viewport_clipping_keeps_sampling_phase() {
    let mut frame = scene();
    frame.images.push(image(1, 4, 4, corners()));
    let mut p = placement(1, 11, 9);
    p.source_x = 1;
    p.source_y = 1;
    p.source_width = 2;
    p.source_height = 2;
    p.offset_x = 3;
    p.offset_y = 5;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let first = consume(&frame, &mut graphics, &mut terminal);
    let p = &first.placements[0];
    assert_eq!(
        (p.col, p.row, p.offset_x, p.offset_y, p.width, p.height),
        (1, 1, 3, 5, 11, 9)
    );
    let raster = placed_image(&first, p);
    assert_eq!((raster.width, raster.height), (11, 9));
    assert_eq!(pixel(raster, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(raster, 10, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(raster, 0, 8), [0, 0, 255, 255]);
    assert_eq!(pixel(raster, 10, 8), [255, 255, 0, 255]);
    assert_eq!(pixel(raster, 5, 4), [255, 255, 0, 255]);
    let full_raster = raster;
    let key = raster.key();
    assert_eq!(graphics.derived_ids.len(), 1);
    assert!(graphics.image_ids.is_empty());
    let repeated = consume(&frame, &mut graphics, &mut terminal);
    assert_eq!(placed_image(&repeated, &repeated.placements[0]).key(), key);
    frame.placements[0].col = -1;
    frame.placements[0].offset_x = 5;
    let clipped = consume(&frame, &mut graphics, &mut terminal);
    let p = &clipped.placements[0];
    assert_eq!(
        (
            p.col,
            p.offset_x,
            p.width,
            p.height,
            p.source_x,
            p.source_width
        ),
        (0, 0, 8, 9, 0, 8)
    );
    let raster = placed_image(&clipped, p);
    assert_ne!(raster.key(), key);
    assert_eq!((raster.width, raster.height), (8, 9));
    for y in 0..9 {
        for x in 0..8 {
            assert_eq!(pixel(raster, x, y), pixel(full_raster, x + 3, y));
        }
    }
    let clipped_key = raster.key();
    let repeated = consume(&frame, &mut graphics, &mut terminal);
    assert_eq!(
        placed_image(&repeated, &repeated.placements[0]).key(),
        clipped_key
    );
    assert_eq!(frame.images[0].pixels, corners());
    frame.images.clear();
    frame.placements.clear();
    let cleared = consume(&frame, &mut graphics, &mut terminal);
    assert!(cleared.images.is_empty());
    assert!(cleared.placements.is_empty());
    assert!(graphics.derived_ids.is_empty());
}

#[test]
fn oversized_offscreen_placement_uploads_only_visible_pixels_with_full_sampling_phase() {
    let mut frame = scene();
    frame
        .images
        .push(image(1, 2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]));
    let mut p = placement(1, 10008, 16);
    p.col = -625;
    p.row = 0;
    p.source_width = 2;
    p.source_height = 1;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    let p = &actual.placements[0];
    assert_eq!((p.col, p.row, p.width, p.height), (0, 0, 96, 16));
    let raster = placed_image(&actual, p);
    assert_eq!(
        (raster.width, raster.height, raster.pixels.len()),
        (96, 16, 96 * 16 * 4)
    );
    assert_eq!(pixel(raster, 3, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(raster, 4, 0), [0, 0, 255, 255]);
    frame.placements[0].col = 0;
    frame.placements[0].width = u32::MAX;
    let huge = consume(&frame, &mut graphics, &mut terminal);
    let raster = placed_image(&huge, &huge.placements[0]);
    assert_eq!(
        (raster.width, raster.height, raster.pixels.len()),
        (96, 16, 96 * 16 * 4)
    );
    assert_eq!(pixel(raster, 95, 15), [255, 0, 0, 255]);
}

#[test]
fn visible_rasters_wider_than_host_limit_are_exact_bounded_tiles() {
    let mut frame = scene();
    frame.size.cols = 417;
    frame.size.cell_width = 24;
    frame.images.push(image(1, 1, 1, vec![255, 0, 255, 255]));
    let mut p = placement(1, 10009, 16);
    p.col = 0;
    p.row = 0;
    p.source_width = 1;
    p.source_height = 1;
    frame.placements.push(p);
    let mut graphics = GraphicsState::new(100);
    let mut terminal = consumer(&frame);
    let actual = consume(&frame, &mut graphics, &mut terminal);
    let mut bounds = actual
        .placements
        .iter()
        .map(|p| (p.col, p.offset_x, p.width, p.height))
        .collect::<Vec<_>>();
    bounds.sort_unstable();
    assert_eq!(bounds, vec![(0, 0, 10000, 16), (416, 16, 8, 16)]);
    assert_eq!(graphics.derived_ids.len(), 2);
    for p in &actual.placements {
        let raster = placed_image(&actual, p);
        assert!(raster.width <= 10000 && raster.height <= 10000);
        assert_eq!(pixel(raster, 0, 0), [255, 0, 255, 255]);
        assert_eq!(
            pixel(raster, raster.width - 1, raster.height - 1),
            [255, 0, 255, 255]
        );
    }
}

#[test]
fn aggregate_visible_pixels_are_bounded_before_allocation_or_output() {
    let mut frame = scene();
    frame.size.cols = 512;
    frame.size.rows = 128;
    frame.images.push(image(1, 1, 1, vec![255; 4]));
    for width in 4095..4100 {
        let mut p = placement(1, width, 2047);
        p.col = 0;
        p.row = 0;
        p.source_width = 1;
        p.source_height = 1;
        frame.placements.push(p);
    }
    let mut graphics = GraphicsState::new(100);
    let mut output = Vec::new();
    assert!(
        draw(
            &mut output,
            &frame,
            frame.size,
            "",
            &ColorProbe::default(),
            &mut graphics
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(graphics.next_image, 100);
    assert!(graphics.derived_ids.is_empty());
}
