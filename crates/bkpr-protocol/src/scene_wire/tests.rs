use super::*;
use crate::{ColorSource, Cursor, CursorShape, ImageLayer, PixelFormat, Placement, Style};

fn fixture() -> Frame {
    let style = Style {
        foreground: [1, 2, 3],
        background: [4, 5, 6],
        underline_color: [7, 8, 9],
        foreground_source: ColorSource::Default,
        background_source: ColorSource::Palette(255),
        underline_source: ColorSource::Rgb([10, 11, 12]),
        bold: true,
        italic: true,
        faint: true,
        blink: true,
        inverse: true,
        invisible: true,
        strikethrough: true,
        overline: true,
        underline: 5,
    };
    let texts = [
        ("a", 1),
        ("é", 1),
        ("e\u{301}", 1),
        ("👩‍💻", 2),
        ("", 0),
        ("\u{200b}", 0),
    ];
    let row = Row {
        cells: texts
            .into_iter()
            .map(|(text, width)| Cell {
                text: text.into(),
                width,
                style: style.clone(),
                hyperlink: Some("https://example.test/é".into()),
            })
            .collect(),
    };
    Frame {
        size: Size {
            cols: 6,
            rows: 2,
            cell_width: 9,
            cell_height: 18,
        },
        rows: vec![row.clone(), row],
        cursor: Cursor {
            x: 4,
            y: 1,
            visible: true,
            blinking: true,
            shape: CursorShape::Hollow,
            color: Some([0, 0, 0]),
        },
        foreground: [20, 30, 40],
        background: [50, 60, 70],
        palette: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]],
        images: [
            PixelFormat::Rgb,
            PixelFormat::Rgba,
            PixelFormat::GrayAlpha,
            PixelFormat::Gray,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, format)| {
            Arc::new(Image {
                id: index as u32,
                generation: u64::MAX - index as u64,
                width: 2,
                height: 1,
                format,
                pixels: vec![index as u8; 2 * format.bytes_per_pixel()],
            })
        })
        .collect(),
        placements: [
            ImageLayer::BelowBackground,
            ImageLayer::BelowText,
            ImageLayer::AboveText,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, layer)| Placement {
            image_id: index as u32,
            generation: u64::MAX - index as u64,
            placement_id: 17 + index as u32,
            col: -2,
            row: -1,
            offset_x: 1,
            offset_y: 2,
            width: 20,
            height: 30,
            source_x: 0,
            source_y: 0,
            source_width: 2,
            source_height: 1,
            layer,
            z: -7,
        })
        .collect(),
        alternate_screen: true,
        mouse_reporting: true,
        scrollback_total: u64::MAX,
        scrollback_offset: 42,
    }
}

fn schema(frame: &Frame) -> g::Scene {
    let bytes = encode(frame, 4, None, 0, &[]).unwrap();
    g::Scene::try_from(g::SceneRef::read_as_root(&bytes).unwrap()).unwrap()
}

fn binary(scene: &g::Scene) -> Vec<u8> {
    let mut builder = Builder::new();
    builder.finish(scene, None).to_vec()
}

#[test]
fn exact_unicode_styles_sources_images_and_diagnostic_bridge() {
    for shape in [
        CursorShape::Bar,
        CursorShape::Block,
        CursorShape::Underline,
        CursorShape::Hollow,
    ] {
        let mut expected = fixture();
        expected.cursor.shape = shape;
        expected.rows[1].cells[0].style.foreground_source = ColorSource::Rgb([90, 80, 70]);
        expected.rows[1].cells[1].style.background_source = ColorSource::Default;
        expected.rows[1].cells[2].style.underline_source = ColorSource::Palette(0);
        expected.rows[1].cells[3].hyperlink = None;
        let bytes = encode(&expected, 4, None, 0, &[]).unwrap();
        let mut frame = None;
        let mut revision = 0;
        decode(&bytes)
            .unwrap()
            .apply(&mut revision, &mut frame)
            .unwrap();
        assert_eq!(revision, 4);
        assert_eq!(frame.as_ref(), Some(&expected));
        let diagnostic: Frame = serde_json::from_value(to_json(&bytes).unwrap()).unwrap();
        assert_eq!(diagnostic, expected);
    }
}

#[test]
fn alternating_styles_and_links_preserve_empty_rows_and_cell_layouts() {
    let mut expected = fixture();
    expected.size.cols = 64;
    expected.size.rows = 4;
    let first = expected.rows[0].cells[0].clone();
    let mut second = first.clone();
    second.style.italic = false;
    second.hyperlink = None;
    let alternating = Row {
        cells: (0..64)
            .map(|index| {
                let mut cell = if (index / 2) % 2 == 0 {
                    first.clone()
                } else {
                    second.clone()
                };
                cell.hyperlink = if index % 2 == 0 {
                    first.hyperlink.clone()
                } else {
                    None
                };
                cell.text = if index % 3 == 0 { "" } else { "x" }.into();
                cell.width = if index % 3 == 0 { 0 } else { 1 };
                cell
            })
            .collect(),
    };
    let empty_same = Row {
        cells: vec![
            Cell {
                text: "".into(),
                width: 0,
                ..second
            };
            64
        ],
    };
    let empty_changed = Row {
        cells: vec![
            Cell {
                text: "".into(),
                width: 1,
                ..first.clone()
            };
            64
        ],
    };
    let cluster = format!("e{}", "\u{301}".repeat(40));
    let resumed = Row {
        cells: (0..64)
            .map(|index| Cell {
                text: if index % 2 == 0 {
                    cluster.as_str()
                } else {
                    "x"
                }
                .into(),
                ..first.clone()
            })
            .collect(),
    };
    expected.rows = vec![alternating, empty_same, empty_changed, resumed];
    let bytes = encode(&expected, 4, None, 0, &[]).unwrap();
    let mut actual = None;
    let mut revision = 0;
    decode(&bytes)
        .unwrap()
        .apply(&mut revision, &mut actual)
        .unwrap();
    assert_eq!(actual.as_ref(), Some(&expected));
}

#[test]
fn deltas_reuse_only_matching_baselines_and_generation_receipts() {
    let original = fixture();
    let baseline = (4, Arc::new(original.clone()));
    let mut changed = original.clone();
    changed.rows[1].cells[0].text = "z".into();
    changed.cursor.color = None;
    let known = original.image_keys();
    // Same image id, new generation: an old receipt cannot suppress its bytes.
    let mut image = (*changed.images[0]).clone();
    image.generation -= 100;
    changed.images[0] = Arc::new(image);
    let bytes = encode(&changed, 5, Some(&baseline), 4, &known).unwrap();
    let diagnostic = to_json(&bytes).unwrap();
    assert_eq!(diagnostic["base_revision"], 4);
    assert_eq!(diagnostic["rows"].as_array().unwrap().len(), 1);
    assert_eq!(diagnostic["rows"][0][0], 1);
    assert_eq!(diagnostic["frame"]["images"].as_array().unwrap().len(), 1);
    assert_eq!(
        diagnostic["frame"]["images"][0]["generation"],
        u64::MAX - 100
    );
    let mut actual = Some(original.clone());
    let mut revision = 4;
    decode(&bytes)
        .unwrap()
        .apply(&mut revision, &mut actual)
        .unwrap();
    // Resources are a response-local addition, not a cloned client image cache.
    let mut expected = changed.clone();
    expected
        .images
        .retain(|image| !known.contains(&image.key()));
    assert_eq!(actual, Some(expected));
    assert_eq!(revision, 5);
    for after in [0, 3, 5] {
        let bytes = encode(&changed, 6, Some(&baseline), after, &[]).unwrap();
        assert!(to_json(&bytes).unwrap()["base_revision"].is_null());
    }
    changed.size.cell_width += 1;
    let bytes = encode(&changed, 6, Some(&baseline), 4, &[]).unwrap();
    assert!(to_json(&bytes).unwrap()["base_revision"].is_null());
}

#[test]
fn rejected_apply_preserves_frame_and_revision() {
    let original = fixture();
    let baseline = (4, Arc::new(original.clone()));
    let bytes = encode(&original, 5, Some(&baseline), 4, &[]).unwrap();
    let cases = [
        (3, Some(original.clone())),
        (4, None),
        (4, {
            let mut resized = original.clone();
            resized.size.cols -= 1;
            Some(resized)
        }),
        (4, {
            let mut incomplete = original.clone();
            incomplete.rows.pop();
            Some(incomplete)
        }),
    ];
    for (mut revision, mut frame) in cases {
        let saved_revision = revision;
        let saved_frame = frame.clone();
        assert!(
            decode(&bytes)
                .unwrap()
                .apply(&mut revision, &mut frame)
                .is_err()
        );
        assert_eq!(revision, saved_revision);
        assert_eq!(frame, saved_frame);
    }
}

#[test]
fn malformed_tables_layouts_and_resources_are_rejected() {
    let valid = schema(&fixture());
    let corruptions: &[fn(&mut g::Scene)] = &[
        |s| s.version += 1,
        |s| s.base_revision = 1,
        |s| {
            s.has_base_revision = true;
            s.base_revision = s.revision;
        },
        |s| s.frame.size.cols = 0,
        |s| s.frame.size.cell_width = 0,
        |s| s.rows[0].index = u64::MAX,
        |s| s.rows[1].index = 0,
        |s| {
            s.rows.pop();
        },
        |s| s.rows[0].runs[0].style = u32::MAX,
        |s| s.rows[0].runs[0].link = u32::MAX,
        |s| {
            s.rows[0].runs[0].has_link = false;
            s.rows[0].runs[0].link = 1;
        },
        |s| s.rows[0].runs[0].layout.clear(),
        |s| s.rows[0].runs[0].layout[0].count = u32::MAX,
        |s| s.rows[0].runs[0].layout[0].count = 0,
        |s| s.rows[0].runs[0].layout[0].width = 3,
        |s| s.rows[0].runs[0].layout[0].length = 8193,
        // The first byte of é is not a complete cell: reject valid UTF-8 text
        // whose declared engine cell boundaries bisect that scalar.
        |s| s.rows[0].runs[0].layout[1].length = 1,
        |s| s.rows[0].runs[0].text.push('x'),
        |s| s.styles[0].flags = 0x100,
        |s| s.styles[0].foreground = 0xff00_0000,
        |s| s.styles[0].foreground_value = 1,
        |s| s.styles[0].background_value = 256,
        |s| s.styles[0].underline_value = 0xff00_0000,
        |s| s.frame.foreground = 0xff00_0000,
        |s| s.frame.palette.push(0xff00_0000),
        |s| s.frame.palette.resize(257, 0),
        |s| {
            s.frame.cursor.has_color = false;
            s.frame.cursor.color = 1;
        },
        |s| s.frame.images[0].width += 1,
        |s| s.frame.images[0].height = u32::MAX,
        |s| s.frame.images.resize(MAX_IMAGES + 1, g::Image::default()),
    ];
    for (index, mutate) in corruptions.iter().enumerate() {
        let mut corrupt = valid.clone();
        mutate(&mut corrupt);
        let bytes = binary(&corrupt);
        assert!(decode(&bytes).is_err(), "accepted corruption {index}");
        assert!(
            to_json(&bytes).is_err(),
            "diagnostic accepted corruption {index}"
        );
    }
}

#[test]
fn binary_offsets_utf8_and_required_string_terminators_are_checked() {
    let bytes = encode(&fixture(), 4, None, 0, &[]).unwrap();
    for length in 0..4 {
        assert!(decode(&bytes[..length]).is_err());
    }
    let mut corrupt = bytes.clone();
    corrupt[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&corrupt).is_err());
    let root = g::SceneRef::read_as_root(&bytes).unwrap();
    let text = root
        .rows()
        .unwrap()
        .get(0)
        .unwrap()
        .unwrap()
        .runs()
        .unwrap()
        .get(0)
        .unwrap()
        .unwrap()
        .text()
        .unwrap();
    let offset = text.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut corrupt = bytes.clone();
    corrupt[offset] = 0xff;
    assert!(decode(&corrupt).is_err());
    let mut corrupt = bytes.clone();
    corrupt[offset + text.len()] = 1;
    assert!(decode(&corrupt).is_err());
    let pixels = root
        .frame()
        .unwrap()
        .images()
        .unwrap()
        .get(0)
        .unwrap()
        .unwrap()
        .pixels()
        .unwrap();
    let offset = pixels.as_ptr() as usize - bytes.as_ptr() as usize;
    let mut corrupt = bytes.clone();
    corrupt[offset - 4..offset].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&corrupt).is_err());
}

#[test]
fn compact_aliases_cannot_bypass_expanded_link_or_image_budgets() {
    let mut scene = schema(&fixture());
    scene.frame.size.cols = 500;
    scene.frame.size.rows = 26;
    scene.hyperlinks = vec!["x".repeat(8192)];
    let run = g::Run {
        style: 0,
        has_link: true,
        link: 0,
        text: String::new(),
        layout: vec![g::Layout {
            length: 0,
            width: 1,
            count: 500,
        }],
    };
    scene.rows = (0..26)
        .map(|index| g::Row {
            index,
            runs: vec![run.clone()],
        })
        .collect();
    let error = match decode(&binary(&scene)) {
        Ok(_) => panic!("accepted amplified hyperlink text"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("expanded text limit"));

    // Reuse a single image offset 65 times. Encoded input is about 1 MiB,
    // but an allocating decoder would otherwise materialize 65 MiB.
    let scene = schema(&fixture());
    let mut builder = Builder::new();
    let pixels = vec![0u8; 1024 * 1024];
    let pixels = builder.create_vector(pixels.as_slice());
    let image = g::Image::create(&mut builder, 1, 1, 1024, 1024, g::PixelFormat::Gray, pixels);
    let images = vec![image; 65];
    let images = builder.create_vector(images.as_slice());
    let f = &scene.frame;
    let frame = g::Frame::create(
        &mut builder,
        f.size,
        f.cursor.as_ref(),
        f.foreground,
        f.background,
        f.palette.as_slice(),
        images,
        f.placements.as_slice(),
        f.alternate_screen,
        f.mouse_reporting,
        f.scrollback_total,
        f.scrollback_offset,
    );
    let root = g::Scene::create(
        &mut builder,
        scene.version,
        scene.revision,
        false,
        0,
        frame,
        scene.styles.as_slice(),
        scene.hyperlinks.as_slice(),
        scene.rows.as_slice(),
    );
    let error = match decode(builder.finish(root, None)) {
        Ok(_) => panic!("accepted amplified image resources"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("image byte limit"));
}

#[test]
fn empty_ascii_and_continuation_cells_keep_distinct_shapes() {
    for text in ["", " ", "a"] {
        let mut frame = fixture();
        for row in &mut frame.rows {
            for cell in &mut row.cells {
                cell.text = text.into();
                cell.width = 1;
            }
        }
        let bytes = encode(&frame, 1, None, 0, &[]).unwrap();
        let mut actual = None;
        decode(&bytes).unwrap().apply(&mut 0, &mut actual).unwrap();
        assert_eq!(actual, Some(frame));
    }
}
