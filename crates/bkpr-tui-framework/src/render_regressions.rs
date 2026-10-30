use super::*;
use bkpr_terminal_engine::Terminal;
fn size(cell_width: u32, cell_height: u32) -> Size {
    Size {
        cols: 80,
        rows: 24,
        cell_width,
        cell_height,
    }
}
fn frame() -> Frame {
    Frame {
        size: size(8, 16),
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
fn keys(bytes: &[u8]) -> Vec<KeyEvent> {
    let mut parser = InputParser::new();
    let mut demux = ReplyDemux::default();
    decode_input(&mut parser, demux.feed(bytes), false)
        .into_iter()
        .filter_map(|event| match event {
            ClientEvent::Legacy(InputEvent::Key(key)) => Some(key_event(key)),
            ClientEvent::Reply(reply) if reply.starts_with(b"key;") => {
                Some(kitty_key(&reply[4..]).unwrap())
            }
            _ => None,
        })
        .collect()
}
#[test]
fn ascii_csi_u_and_arrows_share_one_wire_order() {
    assert_eq!(
        keys(b"a\x1b[98;1uc\x1b[Dd")
            .iter()
            .map(|k| k.key.as_str())
            .collect::<Vec<_>>(),
        vec!["KeyA", "KeyB", "KeyC", "ArrowLeft", "KeyD"]
    );
    assert_eq!(
        keys(b"a\x1b[Db")
            .iter()
            .map(|k| k.key.as_str())
            .collect::<Vec<_>>(),
        vec!["KeyA", "ArrowLeft", "KeyB"]
    );
}
#[test]
fn mouse_maps_cells_to_canonical_not_physical_metrics_and_rejects_padding() {
    assert_eq!(
        mouse_position(10, 5, size(8, 16), size(16, 24)),
        Some((160.0, 120.0))
    );
    assert_eq!(
        mouse_position(10, 5, size(16, 24), size(8, 16)),
        Some((80.0, 80.0))
    );
    let mut narrow = size(8, 16);
    narrow.cols = 20;
    assert_eq!(mouse_position(21, 5, size(16, 24), narrow), None);
}
#[test]
fn virtual_image_rows_get_unique_physical_placement_ids_and_resources_are_cached() {
    let mut frame = frame();
    frame.images.push(Arc::new(Image {
        id: 7,
        generation: 3,
        width: 4,
        height: 8,
        format: PixelFormat::Rgba,
        pixels: [255, 0, 255, 255].repeat(4 * 8),
    }));
    for row in 0..4 {
        frame.placements.push(Placement {
            image_id: 7,
            generation: 3,
            placement_id: 42,
            col: 0,
            row,
            offset_x: 0,
            offset_y: 0,
            width: 32,
            height: 16,
            source_x: 0,
            source_y: row as u32 * 2,
            source_width: 4,
            source_height: 2,
            layer: ImageLayer::BelowText,
            z: -1,
        });
    }
    let mut graphics = GraphicsState::new(1000);
    let host = ColorProbe::default();
    let mut terminal = Terminal::new(frame.size).unwrap();
    let geometry = |frame: &Frame| {
        let mut placements = frame
            .placements
            .iter()
            .map(|p| {
                (
                    p.row,
                    p.col,
                    p.source_y,
                    p.width,
                    p.height,
                    p.source_width,
                    p.source_height,
                )
            })
            .collect::<Vec<_>>();
        placements.sort_unstable();
        placements
    };
    let mut output = Vec::new();
    draw(
        &mut output,
        &frame,
        frame.size,
        "proof",
        &host,
        &mut graphics,
    )
    .unwrap();
    terminal.feed(&output).unwrap();
    let first = terminal.snapshot().unwrap();
    assert_eq!(first.images[0].pixels, frame.images[0].pixels);
    assert_eq!(
        geometry(&first),
        [
            (0, 0, 0, 32, 16, 4, 2),
            (1, 0, 2, 32, 16, 4, 2),
            (2, 0, 4, 32, 16, 4, 2),
            (3, 0, 6, 32, 16, 4, 2),
        ]
    );
    let mut second = Vec::new();
    draw(
        &mut second,
        &frame,
        frame.size,
        "proof",
        &host,
        &mut graphics,
    )
    .unwrap();
    terminal.feed(&second).unwrap();
    let repeated = terminal.snapshot().unwrap();
    assert_eq!(repeated.images[0].key(), first.images[0].key());
    assert_eq!(repeated.images[0].pixels, first.images[0].pixels);
    assert_eq!(geometry(&repeated), geometry(&first));
    frame.images.clear();
    frame.placements.clear();
    let mut cleared = Vec::new();
    draw(
        &mut cleared,
        &frame,
        frame.size,
        "proof",
        &host,
        &mut graphics,
    )
    .unwrap();
    terminal.feed(&cleared).unwrap();
    let cleared = terminal.snapshot().unwrap();
    assert!(cleared.placements.is_empty());
    assert!(cleared.images.is_empty());
}
fn consent() -> Consent {
    Consent {
        id: "request-one".into(),
        request: ClipboardRequest {
            operation: ClipboardOperation::Read {
                mimes: vec!["text/plain".into()],
                list: false,
            },
            location: ClipboardLocation::Standard,
            name: String::new(),
            granted: false,
            can_remember: false,
            paste_offer_id: None,
        },
        started: Instant::now(),
        sent: false,
        allowed: false,
        contents: Vec::new(),
        available: Vec::new(),
        enhanced_supported: Some(false),
        paste: false,
        password: None,
    }
}
#[test]
fn uncorrelated_osc52_reads_are_never_issued_or_accepted() {
    let mut c = consent();
    let mut output = Vec::new();
    let answer = begin_clipboard(&mut output, &mut c).unwrap().unwrap();
    assert_eq!(answer.result, ClipboardResult::Unsupported);
    assert!(output.is_empty());
    assert!(
        clipboard_packet(&mut c, b"52;c;c3RhbGU=")
            .unwrap()
            .is_none()
    );
    c.enhanced_supported = Some(true);
    c.sent = false;
    assert!(begin_clipboard(&mut output, &mut c).unwrap().is_none());
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("5522;type=read:id=request-one"));
    assert!(!text.contains("52;c;?"));
}
#[test]
fn csi_and_mime_listing_accumulators_are_bounded() {
    let mut demux = ReplyDemux::default();
    let mut input = b"\x1b[".to_vec();
    input.extend([b'1'; 1024]);
    assert!(
        demux.feed(&input).iter().any(
            |part| matches!(part,InputPart::Reply(reply) if reply.starts_with(b"input-error;"))
        )
    );
    assert!(demux.buffer.is_empty());
    let mut c = consent();
    let payload = BASE64.encode("text/plain ".repeat(17));
    let packet = format!("5522;type=read:status=DATA:id=request-one:mime=Lg==;{payload}");
    assert!(clipboard_packet(&mut c, packet.as_bytes()).is_err());
}

#[test]
fn pending_capability_retains_consent_until_host_reports_support() {
    let mut request = consent();
    request.enhanced_supported = None;
    request.allowed = true;
    let mut output = Vec::new();
    assert!(
        begin_clipboard(&mut output, &mut request)
            .unwrap()
            .is_none()
    );
    assert!(!request.sent);
    assert!(output.is_empty());
    request.enhanced_supported = Some(true);
    assert!(
        begin_clipboard(&mut output, &mut request)
            .unwrap()
            .is_none()
    );
    assert!(request.sent);
}

#[test]
fn incremental_rows_preserve_wide_graphemes_styles_and_clear_removed_text() {
    let viewport = size(8, 16);
    let mut source = Terminal::new(viewport).unwrap();
    let mut physical = Terminal::new(Size {
        rows: viewport.rows + 1,
        ..viewport
    })
    .unwrap();
    let host = ColorProbe::default();
    let mut graphics = GraphicsState::new(1000);
    for input in [
        "unchanged first row\r\n\x1b[31mwide: 界e\u{301} trailing text\x1b[0m",
        "\x1b[2;1H\x1b[2Kshort",
        "\x1b[2;1H界界e\u{301}\x1b[K",
        "\x1b[2;2Hnarrow\x1b[K",
        "\x1b[2J\x1b[H",
    ] {
        source.feed(input.as_bytes()).unwrap();
        let frame = source.snapshot().unwrap();
        let mut output = Vec::new();
        draw(
            &mut output,
            &frame,
            viewport,
            "status",
            &host,
            &mut graphics,
        )
        .unwrap();
        physical.feed(&output).unwrap();
        let actual = physical.snapshot().unwrap();
        assert!(actual.rows.len() >= frame.rows.len());
        for (y, (actual, expected)) in actual.rows.iter().zip(&frame.rows).enumerate() {
            assert_eq!(actual.cells.len(), expected.cells.len());
            for (x, (actual, expected)) in actual.cells.iter().zip(&expected.cells).enumerate() {
                let normalize = |cell: &Cell| {
                    let mut cell = cell.clone();
                    if cell.width > 0 && cell.text.is_empty() {
                        cell.text.push(' ');
                    }
                    cell
                };
                assert_eq!(
                    normalize(actual),
                    normalize(expected),
                    "row {y}, cell {x}, input {input:?}"
                );
            }
        }
        assert_eq!(actual.cursor, frame.cursor);
    }
}

#[test]
fn cell_span_damage_preserves_static_sides_and_wide_linked_graphemes() {
    let viewport = Size {
        cols: 32,
        rows: 2,
        ..size(8, 16)
    };
    let mut source = Terminal::new(viewport).unwrap();
    let mut physical = Terminal::new(viewport).unwrap();
    let mut graphics = GraphicsState::new(1000);
    let host = ColorProbe::default();
    for (step, input) in [
        "SIDE|\x1b[31m界e\u{301}\x1b[0m|TAIL",
        // Overwriting a wide continuation must also clear its former lead.
        "\x1b[1;7Hx",
        "\x1b[1;8H\x1b]8;;https://example.test\x1b\\e\u{308}\x1b]8;;\x1b\\",
        "\x1b[1;6H\x1b[1;34m界\x1b[0m",
        "\x1b[1;8He\u{301}",
    ]
    .into_iter()
    .enumerate()
    {
        source.feed(input.as_bytes()).unwrap();
        let frame = source.snapshot().unwrap();
        let mut output = Vec::new();
        draw(&mut output, &frame, viewport, "", &host, &mut graphics).unwrap();
        if step > 0 {
            let text = std::str::from_utf8(&output).unwrap();
            assert!(!text.contains("SIDE|"), "unchanged sidebar was repainted");
            assert!(!text.contains("|TAIL"), "unchanged suffix was repainted");
        }
        physical.feed(&output).unwrap();
        let mut reference = Terminal::new(viewport).unwrap();
        let mut complete = Vec::new();
        draw(
            &mut complete,
            &frame,
            viewport,
            "",
            &host,
            &mut GraphicsState::new(2000),
        )
        .unwrap();
        reference.feed(&complete).unwrap();
        let actual = physical.snapshot().unwrap();
        let expected = reference.snapshot().unwrap();
        assert_eq!(actual.rows, expected.rows, "input {input:?}");
        assert_eq!(actual.cursor, expected.cursor, "input {input:?}");
    }
}

#[test]
fn cell_span_damage_clears_shortened_and_removed_rows_and_invalidates_palette() {
    let viewport = Size {
        cols: 32,
        rows: 2,
        ..size(8, 16)
    };
    let mut source = Terminal::new(viewport).unwrap();
    source.feed(b"SIDE|content and removed tail").unwrap();
    let mut frame = source.snapshot().unwrap();
    frame.rows[0].cells.truncate(28);
    let mut physical = Terminal::new(viewport).unwrap();
    let mut graphics = GraphicsState::new(1000);
    let host = ColorProbe::default();
    for step in 0..4 {
        match step {
            1 => frame.rows[0].cells.truncate(8),
            2 => frame.palette[1] = [12, 34, 56],
            3 => frame.rows.clear(),
            _ => {}
        }
        let mut output = Vec::new();
        draw(&mut output, &frame, viewport, "", &host, &mut graphics).unwrap();
        if step == 1 {
            assert!(!std::str::from_utf8(&output).unwrap().contains("SIDE|"));
        }
        if step == 2 {
            assert!(std::str::from_utf8(&output).unwrap().contains("SIDE|"));
        }
        physical.feed(&output).unwrap();
        let mut reference = Terminal::new(viewport).unwrap();
        let mut complete = Vec::new();
        draw(
            &mut complete,
            &frame,
            viewport,
            "",
            &host,
            &mut GraphicsState::new(2000),
        )
        .unwrap();
        reference.feed(&complete).unwrap();
        assert_eq!(
            physical.snapshot().unwrap().rows,
            reference.snapshot().unwrap().rows,
            "transition {step}"
        );
    }
}

#[test]
fn cursor_only_update_does_not_repaint_text_or_chrome() {
    let viewport = size(8, 16);
    let mut source = Terminal::new(viewport).unwrap();
    source.feed(b"stationary text\r\nsecond row").unwrap();
    let mut frame = source.snapshot().unwrap();
    let mut graphics = GraphicsState::new(1000);
    draw(
        &mut Vec::new(),
        &frame,
        viewport,
        "status",
        &ColorProbe::default(),
        &mut graphics,
    )
    .unwrap();
    frame.cursor.x = 2;
    let mut update = Vec::new();
    draw(
        &mut update,
        &frame,
        viewport,
        "status",
        &ColorProbe::default(),
        &mut graphics,
    )
    .unwrap();
    let update = String::from_utf8(update).unwrap();
    assert!(!update.contains("stationary text"));
    assert!(!update.contains("second row"));
    assert!(!update.contains("status"));
    assert!(!update.contains("\x1b[K"));
    assert!(!update.contains("\x1b[2K"));
}

#[test]
fn differential_styles_preserve_intensity_pairs_and_color_resets() {
    let viewport = size(8, 16);
    let mut source = Terminal::new(viewport).unwrap();
    let mut input = String::new();
    for from in 0..4 {
        for to in 0..4 {
            for flags in [from, to] {
                input.push_str("\x1b[22m");
                if flags & 1 != 0 {
                    input.push_str("\x1b[1m");
                }
                if flags & 2 != 0 {
                    input.push_str("\x1b[2m");
                }
                input.push('x');
            }
        }
    }
    input.push_str("\x1b[0;31;44;3;4;5;7;8;9;53mQ\x1b[23;24;25;27;28;29;55mR");
    input.push_str("\x1b[58;2;240;30;60m\x1b[4:3mU\x1b[24mN\x1b[59;4mD\x1b[0m");
    source.feed(input.as_bytes()).unwrap();
    let frame = source.snapshot().unwrap();
    let host = ColorProbe {
        truecolor: true,
        foreground: Some(frame.foreground),
        background: Some(frame.background),
        palette: frame.palette.iter().copied().enumerate().collect(),
        ..ColorProbe::default()
    };
    let mut physical = Terminal::new(Size {
        rows: viewport.rows + 1,
        ..viewport
    })
    .unwrap();
    let mut graphics = GraphicsState::new(1000);
    let mut bytes = Vec::new();
    draw(&mut bytes, &frame, viewport, "status", &host, &mut graphics).unwrap();
    physical.feed(&bytes).unwrap();
    let actual = physical.snapshot().unwrap();
    for (actual, expected) in actual.rows[0]
        .cells
        .iter()
        .zip(&frame.rows[0].cells)
        .take(37)
    {
        assert_eq!(actual, expected);
    }
    let mut repeated = Vec::new();
    draw(
        &mut repeated,
        &frame,
        viewport,
        "status",
        &host,
        &mut graphics,
    )
    .unwrap();
    assert!(
        repeated.is_empty(),
        "unchanged scene must not restart a presentation"
    );
}

#[test]
fn cursor_color_changed_while_hidden_is_applied_when_visible() {
    let mut frame = frame();
    let mut graphics = GraphicsState::new(1000);
    frame.cursor.color = Some([12, 34, 56]);
    draw(
        &mut Vec::new(),
        &frame,
        frame.size,
        "status",
        &ColorProbe::default(),
        &mut graphics,
    )
    .unwrap();
    assert_eq!(graphics.text.physical_cursor_color, None);
    frame.cursor.visible = true;
    let mut bytes = Vec::new();
    draw(
        &mut bytes,
        &frame,
        frame.size,
        "status",
        &ColorProbe::default(),
        &mut graphics,
    )
    .unwrap();
    let mut physical = Terminal::new(Size {
        rows: frame.size.rows + 1,
        ..frame.size
    })
    .unwrap();
    physical.feed(&bytes).unwrap();
    assert_eq!(
        physical.snapshot().unwrap().cursor.color,
        Some([12, 34, 56])
    );
}
