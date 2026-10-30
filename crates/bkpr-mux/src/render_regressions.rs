use super::*;
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
    let mut terminal = bkpr_terminal::Terminal::new(frame.size).unwrap();
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
fn enhanced_paste_barrier_queues_contents_before_later_enter() {
    let enter = InputEvent::Key(termwiz::input::KeyEvent {
        key: KeyCode::Enter,
        modifiers: Modifiers::NONE,
    });
    let mut incoming = VecDeque::from([
        ClientEvent::Legacy(enter),
        ClientEvent::Reply(b"5522;type=read:status=DONE:id=request-one".to_vec()),
    ]);
    let mut deferred = VecDeque::new();
    assert!(matches!(
        next_input(&mut incoming, &mut deferred, true).unwrap(),
        Some(ClientEvent::Reply(_))
    ));
    assert_eq!(deferred.len(), 1);
    let (input, requests) = std::sync::mpsc::sync_channel(4);
    let mut c = consent();
    c.paste = true;
    finish_consent(
        Path::new("/unused-local-endpoint"),
        "view",
        &c,
        success(vec![ClipboardContent {
            mime: "text/plain".into(),
            data: b"paste-sentinel".to_vec(),
        }]),
        &input,
    )
    .unwrap();
    let Some(ClientEvent::Legacy(InputEvent::Key(key))) =
        next_input(&mut incoming, &mut deferred, false).unwrap()
    else {
        panic!("missing deferred Enter");
    };
    dispatch_key(
        key_event(key),
        &mut Vec::new(),
        Path::new("/unused-local-endpoint"),
        "view",
        &mut None,
        &input,
        &mut KeyboardPolicy::default(),
    )
    .unwrap();
    let paste = requests.try_recv().unwrap().0;
    let enter = requests.try_recv().unwrap().0;
    assert_eq!(paste["op"], "paste_contents");
    assert_eq!(
        paste["contents"][0]["data"],
        json!(b"paste-sentinel".to_vec())
    );
    assert_eq!(enter["op"], "key");
    assert_eq!(enter["event"]["key"], "Enter");
}

#[test]
fn pending_capability_retains_consent_without_leaking_dialog_key_lifecycle() {
    let mut waiting = Some(consent());
    waiting.as_mut().unwrap().enhanced_supported = None;
    let mut policy = KeyboardPolicy::default();
    let mut output = Vec::new();
    let (input, received) = std::sync::mpsc::sync_channel(4);
    let press = keys(b"y").pop().unwrap();
    dispatch_key(
        press.clone(),
        &mut output,
        Path::new("/unused-local-endpoint"),
        "view",
        &mut waiting,
        &input,
        &mut policy,
    )
    .unwrap();
    let request = waiting.as_mut().unwrap();
    assert!(request.allowed);
    assert!(!request.sent);
    assert!(output.is_empty());
    assert!(received.try_recv().is_err());
    request.enhanced_supported = Some(true);
    assert!(begin_clipboard(&mut output, request).unwrap().is_none());
    assert!(request.sent);
    waiting = None;
    for action in [KeyAction::Repeat, KeyAction::Release] {
        let mut event = press.clone();
        event.action = action;
        dispatch_key(
            event,
            &mut output,
            Path::new("/unused-local-endpoint"),
            "view",
            &mut waiting,
            &input,
            &mut policy,
        )
        .unwrap();
    }
    assert!(received.try_recv().is_err());
    dispatch_key(
        press.clone(),
        &mut output,
        Path::new("/unused-local-endpoint"),
        "view",
        &mut waiting,
        &input,
        &mut policy,
    )
    .unwrap();
    let event: KeyEvent =
        serde_json::from_value(received.try_recv().unwrap().0["event"].clone()).unwrap();
    assert_eq!(event.key, press.key);
    assert_eq!(event.action, KeyAction::Press);
}
