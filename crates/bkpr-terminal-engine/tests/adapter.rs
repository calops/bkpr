use bkpr_protocol::*;
use bkpr_terminal_engine::*;
use std::sync::{Arc, Mutex};

fn size() -> Size {
    Size {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    }
}
fn terminal() -> Terminal {
    Terminal::new(size()).unwrap()
}
fn replies(effects: Vec<Effect>) -> Vec<u8> {
    effects
        .into_iter()
        .filter_map(|e| {
            if let Effect::PtyWrite(bytes) = e {
                Some(bytes)
            } else {
                None
            }
        })
        .flatten()
        .collect()
}
fn text(frame: &Frame) -> String {
    frame
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn unicode_styles_links_and_owned_frame_survive_mutation() {
    let mut term = terminal();
    term.set_default_colors([201, 202, 203], [1, 2, 3], Some([9, 8, 7]))
        .unwrap();
    term.feed("\x1b[1;3;4:3;38;2;12;34;56m界e\u{301}\x1b[0m\x1b]8;;https://example.test/\x1b\\X\x1b]8;;\x1b\\".as_bytes()).unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.rows[0].cells[0].text, "界");
    assert_eq!(frame.rows[0].cells[0].width, 2);
    assert_eq!(frame.rows[0].cells[1].width, 0);
    assert_eq!(frame.rows[0].cells[2].text, "e\u{301}");
    assert!(frame.rows[0].cells[0].style.bold && frame.rows[0].cells[0].style.italic);
    assert_eq!(frame.rows[0].cells[0].style.foreground, [12, 34, 56]);
    assert_eq!(
        frame.rows[0].cells[3].hyperlink.as_deref(),
        Some("https://example.test/")
    );
    assert_eq!(frame.cursor.color, Some([9, 8, 7]));
    term.feed(b"\x1bc").unwrap();
    assert_eq!(frame.rows[0].cells[0].text, "界");
    assert!(!text(&term.snapshot().unwrap()).contains('界'));
}

#[test]
fn empty_cell_background_and_palette_override_are_not_lost() {
    let mut term = terminal();
    let mut palette = [[0, 0, 0]; 256];
    palette[1] = [7, 8, 9];
    term.set_palette(&palette).unwrap();
    term.feed(b"\x1b[41m\x1b[2K\x1b[0m\x1b]4;1;rgb:aa/bb/cc\x1b\\\x1b[31mX")
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.palette[1], [170, 187, 204]);
    assert_eq!(frame.rows[0].cells[0].style.foreground, [170, 187, 204]);
    assert_eq!(frame.rows[0].cells[5].style.background, [170, 187, 204]);
}

#[test]
fn scrollback_read_restores_live_view_and_resize_reflows() {
    let mut term = terminal();
    for i in 0..30 {
        term.feed(format!("line-{i:02}\r\n").as_bytes()).unwrap();
    }
    let live = term.snapshot().unwrap();
    assert!(live.scrollback_total > 4);
    assert_eq!(live.scrollback_offset, 0);
    let current = term.read_scrollback(0).unwrap();
    assert_eq!(current.scrollback_offset, 0);
    assert_eq!(text(&current), text(&live));
    let older = term.read_scrollback(8).unwrap();
    assert_eq!(older.scrollback_offset, 8);
    assert_ne!(text(&older), text(&live));
    assert_eq!(term.snapshot().unwrap().scrollback_offset, 0);
    let history = term.read_scrollback(u64::MAX).unwrap();
    assert_eq!(
        history.scrollback_offset,
        live.scrollback_total - u64::from(live.size.rows)
    );
    assert!(text(&history).contains("line-00"));
    assert_eq!(text(&term.snapshot().unwrap()), text(&live));
    term.resize(Size {
        cols: 10,
        rows: 6,
        ..size()
    })
    .unwrap();
    assert_eq!(term.snapshot().unwrap().size.cols, 10);
    term.feed(b"\x1b[?1049hALT").unwrap();
    assert!(term.snapshot().unwrap().alternate_screen);
    term.feed(b"\x1b[?1049l").unwrap();
    assert!(!term.snapshot().unwrap().alternate_screen);
}

#[test]
fn native_modes_drive_key_mouse_focus_and_safe_paste() {
    let mut term = terminal();
    let up = KeyEvent {
        key: "ArrowUp".into(),
        modifiers: 0,
        text: String::new(),
        action: KeyAction::Press,
        unshifted_codepoint: 0,
    };
    assert_eq!(term.encode_key(&up).unwrap(), b"\x1b[A");
    term.feed(b"\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1006h")
        .unwrap();
    assert_eq!(term.encode_key(&up).unwrap(), b"\x1bOA");
    assert_eq!(
        term.encode_paste(b"one\ntwo").unwrap(),
        b"\x1b[200~one\ntwo\x1b[201~"
    );
    assert_eq!(term.encode_focus(true).unwrap(), b"\x1b[I");
    assert_eq!(
        term.encode_mouse(&MouseEvent {
            x: 16.0,
            y: 16.0,
            modifiers: 0,
            button: MouseButton::Left,
            action: MouseAction::Press
        })
        .unwrap(),
        b"\x1b[<0;3;2M"
    );
    term.feed(b"\x1b[?2004l").unwrap();
    assert!(term.encode_paste(b"one\ntwo").is_err());
    let ctrl_c = KeyEvent {
        key: "KeyC".into(),
        modifiers: 2,
        text: "c".into(),
        action: KeyAction::Press,
        unshifted_codepoint: 'c' as u32,
    };
    assert_eq!(term.encode_key(&ctrl_c).unwrap(), [3]);
    term.feed(b"\x1b[>1u").unwrap();
    assert_eq!(term.encode_key(&ctrl_c).unwrap(), b"\x1b[99;5u");
}

#[test]
fn kitty_pixels_are_shared_owned_and_deletion_removes_placements() {
    let mut term = terminal();
    term.feed(b"\x1b_Ga=T,f=32,i=7,s=1,v=1,c=2,r=1,q=2;/wAA/w==\x1b\\")
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.images.len(), 1);
    assert_eq!(frame.images[0].pixels, [255, 0, 0, 255]);
    assert_eq!(frame.placements[0].image_id, 7);
    let repeated = term.snapshot().unwrap();
    assert!(Arc::ptr_eq(&frame.images[0], &repeated.images[0]));
    term.feed(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\").unwrap();
    assert!(term.snapshot().unwrap().placements.is_empty());
    drop(term);
    assert_eq!(frame.images[0].pixels, [255, 0, 0, 255]);
}

#[test]
fn virtual_kitty_rows_resolve_images_without_replaying_protocol_glyphs() {
    use base64::Engine;
    let mut term = Terminal::new(Size { rows: 8, ..size() }).unwrap();
    let pixels = [255u8, 0, 255].repeat(32);
    let payload = base64::engine::general_purpose::STANDARD.encode(&pixels);
    term.feed(format!("\x1b_Ga=T,f=24,i=1,p=1,s=4,v=8,U=1,c=4,r=4,q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
    let diacritics = ['\u{305}', '\u{30d}', '\u{30e}', '\u{310}'];
    for (row, row_mark) in diacritics.iter().enumerate() {
        let mut line = format!("\x1b[{};3H\x1b[38;5;1;58;5;1m", row + 2);
        for col_mark in diacritics {
            line.push('\u{10eeee}');
            line.push(*row_mark);
            line.push(col_mark);
        }
        line.push_str("\x1b[0m");
        term.feed(line.as_bytes()).unwrap();
    }
    term.feed("\x1b[1;1H界e\u{301}\x1b[2;9Hkept\x1b[7;1H\u{e123}".as_bytes())
        .unwrap();
    // Palette mutation must not turn a source image ID into a host-terminal ID.
    term.feed(b"\x1b]4;1;rgb:cc/04/03\x1b\\").unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.images.len(), 1);
    assert_eq!(frame.images[0].pixels, pixels);
    assert_eq!(frame.placements.len(), 4);
    for (row, placement) in frame.placements.iter().enumerate() {
        assert_eq!(placement.image_id, 1);
        assert_eq!(placement.row, row as i32 + 1);
        assert_eq!(placement.col, 2);
        assert_eq!(placement.layer, ImageLayer::BelowText);
        for cell in &frame.rows[row + 1].cells[2..6] {
            assert!(cell.text.is_empty());
            assert_eq!(cell.width, 1);
            assert_eq!(cell.style.foreground_source, ColorSource::Palette(1));
            assert_eq!(cell.style.foreground, [204, 4, 3]);
        }
    }
    assert_eq!(frame.rows[0].cells[0].text, "界");
    assert_eq!(frame.rows[0].cells[2].text, "e\u{301}");
    assert_eq!(frame.rows[1].cells[8].text, "k");
    assert_eq!(frame.rows[6].cells[0].text, "\u{e123}");
    assert!(!text(&frame).contains('\u{10eeee}'));
    term.feed(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\").unwrap();
    let deleted = term.snapshot().unwrap();
    assert!(deleted.placements.is_empty());
    assert!(!text(&deleted).contains('\u{10eeee}'));
    assert_eq!(frame.placements.len(), 4);
}

#[test]
fn osc7501_native_validation_hierarchy_prompt_and_reset_lifecycle() {
    let mut term = terminal();
    let effects = term.feed(b"\x1b]7501;?\x1b\\\x1b]7501;state=working:app=fixture:msg=YnVpbGQ=\x07\x1b]7501;state=blocked:kind=permission:id=build/test\x07").unwrap();
    assert!(replies(effects).windows(7).any(|s| s == b"states="));
    let records = term.program_status();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].app.as_deref(), Some("fixture"));
    assert_eq!(records[0].message.as_deref(), Some("build"));
    term.feed(b"\x1b]7501;state=done:id=build\x07\x1b]7501;state=clear:id=build\x07")
        .unwrap();
    assert_eq!(term.program_status().len(), 1);
    term.feed(b"\x1b]7501;{\"state\":\"working\"}\x07").unwrap();
    assert_eq!(term.program_status().len(), 1);
    term.feed(b"\x1b]133;A\x07").unwrap();
    assert!(term.program_status().is_empty());
    term.feed(b"\x1b]7501;state=done:app=fixture\x07").unwrap();
    term.program_exited();
    assert_eq!(term.program_status().len(), 1);
    term.mark_status_seen();
    assert!(term.program_status().is_empty());
    term.feed(b"\x1b]7501;state=done\x07\x1bc").unwrap();
    assert!(term.program_status().is_empty());
}

#[test]
fn clipboard52_is_decoded_owned_and_answered_synchronously() {
    let mut term = terminal();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    term.set_clipboard_handler(Box::new(move |request| {
        observed.lock().unwrap().push(request);
        ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "text/plain".into(),
                data: b"reply".to_vec(),
            }],
            available: vec!["text/plain".into()],
            remember: false,
        }
    }));
    term.feed(b"\x1b]52;c;aGVsbG8=\x07").unwrap();
    let response = replies(term.feed(b"\x1b]52;c;?\x07").unwrap());
    assert!(String::from_utf8(response).unwrap().contains("cmVwbHk="));
    let captured = requests.lock().unwrap();
    assert!(
        matches!(&captured[0].operation, ClipboardOperation::Write {contents} if contents[0].data == b"hello")
    );
    assert!(
        matches!(&captured[1].operation, ClipboardOperation::Read {mimes,..} if mimes == &["text/plain"])
    );
}

#[test]
fn clipboard5522_atomic_mime_write_and_denied_read_use_native_replies() {
    let mut term = terminal();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    term.set_clipboard_handler(Box::new(move |request| {
        let success = matches!(request.operation, ClipboardOperation::Write { .. });
        observed.lock().unwrap().push(request);
        ClipboardResponse {
            result: if success {
                ClipboardResult::Success
            } else {
                ClipboardResult::Denied
            },
            contents: vec![],
            available: vec![],
            remember: false,
        }
    }));
    let response = replies(term.feed(b"\x1b]5522;type=write:id=w\x1b\\\x1b]5522;type=wdata:mime=aW1hZ2UvcG5n;AAECAw==\x1b\\\x1b]5522;type=wdata\x1b\\").unwrap());
    let response = String::from_utf8(response).unwrap();
    assert_eq!(response, "\x1b]5522;type=write:status=DONE:id=w\x1b\\");
    assert!(
        matches!(&requests.lock().unwrap()[0].operation, ClipboardOperation::Write{contents} if contents[0].mime == "image/png" && contents[0].data == [0,1,2,3])
    );
    let response = String::from_utf8(replies(
        term.feed(b"\x1b]5522;type=read:id=r;dGV4dC9wbGFpbg==\x1b\\")
            .unwrap(),
    ))
    .unwrap();
    assert!(response.contains("type=read:status=EPERM:id=r"));
    let before = requests.lock().unwrap().len();
    term.feed(b"\x1b]5522;type=write\x1b\\\x1b]5522;type=wdata:mime=aW1hZ2UvcG5n;!!!!\x1b\\\x1b]5522;type=wdata\x1b\\").unwrap();
    assert_eq!(requests.lock().unwrap().len(), before);
}

#[test]
fn enhanced_paste_is_native_event_and_callback_panic_is_denied() {
    let mut term = terminal();
    term.set_clipboard_handler(Box::new(|_| panic!("consent transport failed")));
    term.feed(b"\x1b[?5522h").unwrap();
    let event = String::from_utf8(
        term.paste_contents(
            1,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(event.contains("5522;type=read:status=OK") && event.contains("pw="));
    let response = String::from_utf8(replies(
        term.feed(b"\x1b]5522;type=read:id=r;dGV4dC9wbGFpbg==\x1b\\")
            .unwrap(),
    ))
    .unwrap();
    assert!(response.contains("status=EPERM"));
}

#[test]
fn explicit_rgb_equal_to_default_and_palette_sources_remain_distinct() {
    let mut term = terminal();
    term.set_default_colors([11, 22, 33], [44, 55, 66], None)
        .unwrap();
    term.feed(b"A\x1b[38;2;11;22;33mB\x1b[31mC\x1b[48;2;44;55;66m\x1b[K")
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(
        frame.rows[0].cells[0].style.foreground,
        frame.rows[0].cells[1].style.foreground
    );
    assert_eq!(
        frame.rows[0].cells[0].style.foreground_source,
        ColorSource::Default
    );
    assert_eq!(
        frame.rows[0].cells[1].style.foreground_source,
        ColorSource::Rgb([11, 22, 33])
    );
    assert_eq!(
        frame.rows[0].cells[2].style.foreground_source,
        ColorSource::Palette(1)
    );
    assert_eq!(
        frame.rows[0].cells[10].style.background_source,
        ColorSource::Rgb([44, 55, 66])
    );
}

#[test]
fn kitty_png_decoder_consumes_real_png_and_rejects_invalid_bytes() {
    use base64::Engine;
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255, 0, 0, 255, 0, 0, 255, 127])
            .unwrap();
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let mut term = terminal();
    term.feed(format!("\x1b_Ga=T,f=100,i=19,c=2,r=1,q=2;{payload}\x1b\\").as_bytes())
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.images[0].width, 2);
    assert_eq!(frame.images[0].pixels, [255, 0, 0, 255, 0, 0, 255, 127]);
    term.feed(b"\x1b_Ga=T,f=100,i=20,q=2;bm90IGEgcG5n\x1b\\")
        .unwrap();
    assert!(
        !term
            .snapshot()
            .unwrap()
            .images
            .iter()
            .any(|image| image.id == 20)
    );
}

#[test]
fn render_hold_owns_deleted_image_pixels_and_hyperlinks() {
    let mut term = terminal();
    term.feed(b"\x1b]8;;https://example.test/old\x1b\\X\x1b]8;;\x1b\\\x1b_Ga=T,f=32,i=7,s=1,v=1,q=2;/wAA/w==\x1b\\\x1b[?2026h\x1b[2J\x1b_Ga=d,d=I,i=7,q=2\x1b\\").unwrap();
    let held = term.snapshot().unwrap();
    assert_eq!(held.images[0].pixels, [255, 0, 0, 255]);
    assert_eq!(
        held.rows[0].cells[0].hyperlink.as_deref(),
        Some("https://example.test/old")
    );
    term.feed(b"\x1b[?2026l").unwrap();
    assert!(term.snapshot().unwrap().images.is_empty());
}

#[test]
fn status_capacity_evicts_oldest_updated_not_an_arbitrary_record() {
    let mut term = terminal();
    for n in 0..256 {
        term.feed(format!("\x1b]7501;state=done:id=job{n}\x07").as_bytes())
            .unwrap();
    }
    term.feed(b"\x1b]7501;state=error:id=job0\x07\x1b]7501;state=done:id=last\x07")
        .unwrap();
    let records = term.program_status();
    assert_eq!(records.len(), 256);
    assert!(records.iter().any(|r| r.id == "job0" && r.state == "error"));
    assert!(!records.iter().any(|r| r.id == "job1"));
}

#[test]
fn size_and_device_queries_reply_with_canonical_geometry_without_sixel() {
    let mut term = terminal();
    let response = String::from_utf8(replies(
        term.feed(b"\x1b[c\x1b[18t\x1b[16t\x1b[?2048h").unwrap(),
    ))
    .unwrap();
    assert!(response.contains("\x1b[?62;22;52c"));
    assert!(response.contains("\x1b[8;4;20t"));
    assert!(response.contains("\x1b[6;16;8t"));
    term.resize(Size {
        cols: 30,
        rows: 6,
        ..size()
    })
    .unwrap();
    let response = String::from_utf8(replies(term.drain_effects().unwrap())).unwrap();
    assert!(response.contains("\x1b[48;6;30;96;240t"));
}

#[test]
fn history_read_during_hold_cannot_consume_canonical_render_updates() {
    let mut term = terminal();
    term.feed(b"A\x1b[?2026hB").unwrap();
    assert!(term.sync_output_remaining().is_some());
    assert!(text(&term.read_scrollback(0).unwrap()).contains("AB"));
    assert!(!text(&term.snapshot().unwrap()).contains("AB"));
    term.feed(b"\x1b[?2026l").unwrap();
    assert!(term.sync_output_remaining().is_none());
    assert!(text(&term.snapshot().unwrap()).contains("AB"));
    // A subsequent hold before any further dirty rows also gets a fresh scene.
    term.feed(b"\x1b[?2026hC").unwrap();
    assert!(text(&term.snapshot().unwrap()).contains("AB"));
    assert!(!text(&term.snapshot().unwrap()).contains("ABC"));
    term.feed(b"\x1b[?2026l").unwrap();
    assert!(text(&term.snapshot().unwrap()).contains("ABC"));
}

#[test]
fn native_rgb_stays_rgb_and_pixels_use_base64_wire_encoding() {
    let mut term = terminal();
    term.feed(b"\x1b_Ga=T,f=24,i=9,s=1,v=1,q=2;/wD/\x1b\\")
        .unwrap();
    let frame = term.snapshot().unwrap();
    assert_eq!(frame.images[0].format, PixelFormat::Rgb);
    assert_eq!(frame.images[0].pixels, [255, 0, 255]);
    let json = serde_json::to_value(&frame).unwrap();
    assert_eq!(json["images"][0]["pixels"], "/wD/");
    assert_eq!(serde_json::to_value(frame.wire(&[])).unwrap(), json);
    let known = frame.image_keys();
    let filtered = serde_json::to_value(frame.wire(&known)).unwrap();
    let mut without_images = json.clone();
    without_images["images"] = serde_json::json!([]);
    assert_eq!(filtered, without_images);
    let wrong_generation = [ImageKey {
        id: known[0].id,
        generation: known[0].generation + 1,
    }];
    assert_eq!(
        serde_json::to_value(frame.wire(&wrong_generation)).unwrap(),
        json
    );
    let decoded: Frame = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, frame);
}

#[test]
fn large_native_rgba_and_rgb_roundtrip_below_frame_wire_quota() {
    use base64::Engine;
    use std::fmt::Write as _;
    for (wire_format, format, width, height) in [
        (32, PixelFormat::Rgba, 4096usize, 2048usize),
        (24, PixelFormat::Rgb, 4096, 2730),
    ] {
        let mut term = terminal();
        let pixels = vec![0xabu8; width * height * format.bytes_per_pixel()];
        let raw_chunk = 3 * 16 * 1024;
        let count = pixels.len().div_ceil(raw_chunk);
        let mut packet = String::with_capacity(128 + raw_chunk.div_ceil(3) * 4);
        for (index, chunk) in pixels.chunks(raw_chunk).enumerate() {
            packet.clear();
            let more = u8::from(index + 1 != count);
            if index == 0 {
                write!(
                    packet,
                    "\x1b_Ga=T,f={wire_format},i=32,s={width},v={height},c=1,r=1,q=2,m={more};"
                )
                .unwrap();
            } else {
                write!(packet, "\x1b_Gm={more};").unwrap();
            }
            base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut packet);
            packet.push_str("\x1b\\");
            term.feed(packet.as_bytes()).unwrap();
        }
        drop(pixels);
        let frame = term.snapshot().unwrap();
        assert_eq!(frame.images.len(), 1);
        assert_eq!(frame.images[0].format, format);
        assert_eq!(
            frame.images[0].pixels.len(),
            width * height * format.bytes_per_pixel()
        );
        let mut encoded = Vec::with_capacity(frame.images[0].pixels.len().div_ceil(3) * 4 + 65536);
        serde_json::to_writer(&mut encoded, &frame).unwrap();
        assert!(encoded.len() < 96 * 1024 * 1024);
        let decoded: Frame = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.images[0], frame.images[0]);
    }
}

fn offer_password(event: Vec<u8>) -> String {
    let event = String::from_utf8(event).unwrap();
    let rest = event.split(":pw=").nth(1).expect("native event password");
    rest.split([':', ';', '\x1b', '\x07'])
        .next()
        .unwrap()
        .to_owned()
}
fn read_offer(term: &mut Terminal, password: &str, id: &str, mime: &str, primary: bool) -> String {
    use base64::Engine;
    let mimes = base64::engine::general_purpose::STANDARD.encode(mime);
    let location = if primary { ":loc=primary" } else { "" };
    let request =
        format!("\x1b]5522;type=read:id={id}:name=YXBw:pw={password}{location};{mimes}\x1b\\");
    String::from_utf8(replies(term.feed(request.as_bytes()).unwrap())).unwrap()
}

#[test]
fn enhanced_offers_bind_exact_bytes_not_current_clipboard_or_session_grants() {
    let mut term = terminal();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&seen);
    term.set_clipboard_handler(Box::new(move |request| {
        observed.lock().unwrap().push((
            request.paste_offer_id,
            request.granted,
            request.can_remember,
        ));
        ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![ClipboardContent {
                mime: "image/png".into(),
                data: b"current-clipboard".to_vec(),
            }],
            available: vec!["wrong/replacement".into()],
            remember: true,
        }
    }));
    term.feed(b"\x1b[?5522h").unwrap();
    // Establish a real remembered grant before offering two distinct payloads.
    let session = "c2Vzc2lvbg==";
    assert!(read_offer(&mut term, session, "session", "image/png", false).contains("status=DATA"));
    let first = offer_password(
        term.paste_contents(
            11,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap(),
    );
    let second = offer_password(
        term.paste_contents(
            22,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![9, 8, 7],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap(),
    );
    assert!(term.has_paste_offer(11) && term.has_paste_offer(22));
    // A valid unrelated session password is not the paste offer correlation,
    // even if its untrusted OSC response ID happens to equal an offer ID.
    let ordinary = read_offer(&mut term, session, "11", "image/png", false);
    assert!(ordinary.contains("Y3VycmVudC1jbGlwYm9hcmQ="));
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((None, true, true))
    );
    let listing = read_offer(&mut term, &first, "list", ".", false);
    assert!(listing.contains("aW1hZ2UvcG5nCg=="));
    assert!(!listing.contains("wrong/replacement"));
    assert!(term.has_paste_offer(11)); // listing does not consume the native grant
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((Some(11), false, false))
    );
    let second_reply = read_offer(&mut term, &second, "second", "image/png", false);
    assert!(second_reply.contains(";CQgH\x1b\\"));
    assert!(!second_reply.contains("Y3VycmVudC1jbGlwYm9hcmQ="));
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((Some(22), true, false))
    );
    assert!(!term.has_paste_offer(22));
    let first_reply = read_offer(&mut term, &first, "first", "image/png", false);
    assert!(first_reply.contains(";AQID\x1b\\"));
    assert!(!term.has_paste_offer(11));
    // Even a policy response requesting remember cannot promote a paste
    // password to a session grant.
    read_offer(&mut term, &second, "replay", "image/png", false);
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((None, false, true))
    );
}

#[test]
fn cancelled_or_wrong_location_offers_deny_without_fetching_another_clipboard() {
    let mut term = terminal();
    let calls = Arc::new(Mutex::new(0));
    let observed = Arc::clone(&calls);
    term.set_clipboard_handler(Box::new(move |_| {
        *observed.lock().unwrap() += 1;
        ClipboardResponse {
            result: ClipboardResult::Success,
            contents: vec![],
            available: vec![],
            remember: false,
        }
    }));
    term.feed(b"\x1b[?5522h").unwrap();
    let first = offer_password(
        term.paste_contents(
            1,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![1],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap(),
    );
    assert!(term.cancel_paste_offer(1));
    assert!(
        read_offer(&mut term, &first, "cancelled", "image/png", false).contains("status=EPERM")
    );
    assert_eq!(*calls.lock().unwrap(), 0);
    let second = offer_password(
        term.paste_contents(
            2,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![2],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap(),
    );
    assert!(
        read_offer(&mut term, &second, "wrong-location", "image/png", true)
            .contains("status=EPERM")
    );
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(!term.has_paste_offer(2));
    assert!(
        term.paste_contents(1, vec![], ClipboardLocation::Standard, false)
            .is_err()
    );
}

#[test]
fn explicit_text_is_inserted_even_when_enhanced_paste_mode_is_enabled() {
    let mut term = terminal();
    term.feed(b"\x1b[?5522h\x1b[?2004h").unwrap();
    assert_eq!(
        term.encode_paste(b"provided").unwrap(),
        b"\x1b[200~provided\x1b[201~"
    );
    term.feed(b"\x1b[?5522l").unwrap();
    assert_eq!(
        term.paste_contents(
            1,
            vec![ClipboardContent {
                mime: "text/plain".into(),
                data: b"direct".to_vec()
            }],
            ClipboardLocation::Standard,
            false
        )
        .unwrap(),
        b"\x1b[200~direct\x1b[201~"
    );
    assert!(!term.has_paste_offer(1));
}

#[test]
fn pending_offer_count_is_bounded_and_cancelled_ids_are_never_reused() {
    let mut term = terminal();
    term.feed(b"\x1b[?5522h").unwrap();
    for id in 1..=PASTE_OFFER_MAX_COUNT as u64 {
        term.paste_contents(
            id,
            vec![ClipboardContent {
                mime: "image/png".into(),
                data: vec![1],
            }],
            ClipboardLocation::Standard,
            false,
        )
        .unwrap();
    }
    let next = PASTE_OFFER_MAX_COUNT as u64 + 1;
    assert!(
        term.paste_contents(next, vec![], ClipboardLocation::Standard, false)
            .is_err()
    );
    assert!(term.cancel_paste_offer(1));
    assert!(
        term.paste_contents(1, vec![], ClipboardLocation::Standard, false)
            .is_err()
    );
    term.paste_contents(next, vec![], ClipboardLocation::Standard, false)
        .unwrap();
    assert!(term.has_paste_offer(next));
}

#[test]
fn upstream_safe_ground_writer_uses_the_matching_native_boundary_abi() {
    // Initialize the product's global device hook before calling SDK APIs.
    let _policy = terminal();
    let mut native = libghostty_vt::Terminal::new(20, 4).unwrap();
    native.vt_write(b"\x1b]2;partial");
    assert!(!native.is_vt_ground().unwrap());
    assert_eq!(native.vt_write_until_ground(b"\x07rest").unwrap(), Some(1));
    assert!(native.is_vt_ground().unwrap());
    assert_eq!(native.title().unwrap(), b"partial");
    assert_eq!(
        native.vt_write_until_ground(b"not consumed").unwrap(),
        Some(0)
    );
}

#[test]
fn grayscale_png_and_alpha_are_decoded_by_the_upstream_safe_decoder() {
    use base64::Engine;
    for (color, pixels, expected) in [
        (
            png::ColorType::Grayscale,
            vec![17, 203],
            vec![17, 17, 17, 255, 203, 203, 203, 255],
        ),
        (
            png::ColorType::GrayscaleAlpha,
            vec![17, 29, 203, 127],
            vec![17, 17, 17, 29, 203, 203, 203, 127],
        ),
    ] {
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 2, 1);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixels)
                .unwrap();
        }
        let payload = base64::engine::general_purpose::STANDARD.encode(png_bytes);
        let mut term = terminal();
        term.feed(format!("\x1b_Ga=T,f=100,i=31,c=2,r=1,q=2;{payload}\x1b\\").as_bytes())
            .unwrap();
        let first = term.snapshot().unwrap();
        assert_eq!(first.images[0].pixels, expected);
        let again = term.snapshot().unwrap();
        assert!(Arc::ptr_eq(&first.images[0], &again.images[0]));
        let history = term.read_scrollback(0).unwrap();
        assert!(Arc::ptr_eq(&first.images[0], &history.images[0]));
    }
}
