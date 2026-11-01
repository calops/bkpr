use bkpr_protocol::{MAX_INPUT_BYTES, SelectionAction, SelectionEvent, SelectionSpan, Size};
use bkpr_terminal_engine::Terminal;

fn size(cols: u16, rows: u16) -> Size {
    Size {
        cols,
        rows,
        cell_width: 1,
        cell_height: 1,
    }
}
fn event(action: SelectionAction, x: f32, y: f32, modifiers: u8, time_ms: u64) -> SelectionEvent {
    SelectionEvent {
        action,
        x,
        y,
        modifiers,
        time_ms,
    }
}
fn send(
    term: &mut Terminal,
    view: &str,
    action: SelectionAction,
    x: f32,
    y: f32,
    modifiers: u8,
    time_ms: u64,
) {
    term.select_view(view, event(action, x, y, modifiers, time_ms))
        .unwrap();
}
fn drag(term: &mut Terminal, view: &str, start: (f32, f32), end: (f32, f32), modifiers: u8) {
    send(
        term,
        view,
        SelectionAction::Press,
        start.0,
        start.1,
        modifiers,
        1000,
    );
    send(
        term,
        view,
        SelectionAction::Drag,
        end.0,
        end.1,
        modifiers,
        1010,
    );
    send(
        term,
        view,
        SelectionAction::Release,
        end.0,
        end.1,
        modifiers,
        1020,
    );
}
fn copied(term: &mut Terminal, view: &str) -> String {
    String::from_utf8(term.copy_view_selection(view).unwrap().unwrap()).unwrap()
}

#[test]
fn native_gestures_keep_views_independent_and_never_modify_canonical_scene() {
    let mut term = Terminal::new(size(20, 4)).unwrap();
    term.feed(b"hello world").unwrap();
    let original = term.snapshot().unwrap();
    drag(&mut term, "left", (0.1, 0.5), (4.9, 0.5), 0);
    assert_eq!(copied(&mut term, "left"), "hello");
    assert_eq!(
        term.view_selection("left").unwrap().unwrap().spans,
        vec![SelectionSpan {
            row: 0,
            start: 0,
            end: 5
        }]
    );
    assert!(term.view_selection("right").unwrap().is_none());
    assert!(term.copy_view_selection("right").unwrap().is_none());
    for time in [100, 200] {
        send(
            &mut term,
            "right",
            SelectionAction::Press,
            7.5,
            0.5,
            0,
            time,
        );
        send(
            &mut term,
            "right",
            SelectionAction::Release,
            7.5,
            0.5,
            0,
            time + 1,
        );
    }
    assert_eq!(copied(&mut term, "right"), "world");
    send(&mut term, "right", SelectionAction::Press, 7.5, 0.5, 0, 300);
    send(
        &mut term,
        "right",
        SelectionAction::Release,
        7.5,
        0.5,
        0,
        301,
    );
    assert_eq!(copied(&mut term, "right"), "hello world");
    assert_eq!(copied(&mut term, "left"), "hello");
    assert_eq!(term.snapshot().unwrap(), original);
    send(&mut term, "right", SelectionAction::Clear, 0.0, 0.0, 0, 400);
    assert!(term.copy_view_selection("right").unwrap().is_none());
    assert_eq!(copied(&mut term, "left"), "hello");
}

#[test]
fn extraction_preserves_unicode_soft_wraps_reverse_and_rectangles() {
    let mut term = Terminal::new(size(8, 4)).unwrap();
    term.feed("ab界cdéfgh".as_bytes()).unwrap();
    drag(&mut term, "forward", (0.1, 0.5), (1.9, 1.5), 0);
    assert_eq!(copied(&mut term, "forward"), "ab界cdéfgh");
    drag(&mut term, "reverse", (1.9, 1.5), (0.1, 0.5), 0);
    assert_eq!(copied(&mut term, "reverse"), "ab界cdéfgh");
    let mut rectangle = Terminal::new(size(12, 4)).unwrap();
    rectangle.feed(b"abcde\r\nfghij").unwrap();
    drag(&mut rectangle, "block", (1.1, 0.5), (3.9, 1.5), 4);
    assert_eq!(copied(&mut rectangle, "block"), "bcd\nghi");
    let selected = rectangle.view_selection("block").unwrap().unwrap();
    assert!(selected.rectangle);
    assert_eq!(
        selected.spans,
        vec![
            SelectionSpan {
                row: 0,
                start: 1,
                end: 4
            },
            SelectionSpan {
                row: 1,
                start: 1,
                end: 4
            }
        ]
    );
}

#[test]
fn tracked_selection_survives_output_history_and_resize_then_retires_cleanly() {
    let mut term = Terminal::new(size(20, 3)).unwrap();
    term.feed(b"ANCHOR").unwrap();
    drag(&mut term, "reader", (0.1, 0.5), (5.9, 0.5), 0);
    term.feed(b"\r\nrow2\r\nrow3\r\nrow4\r\nrow5").unwrap();
    assert_eq!(copied(&mut term, "reader"), "ANCHOR");
    assert!(
        term.view_selection("reader")
            .unwrap()
            .unwrap()
            .spans
            .is_empty()
    );
    term.scroll_view("reader", i64::MAX).unwrap();
    assert_eq!(
        term.view_selection("reader").unwrap().unwrap().spans[0].row,
        0
    );
    term.resize(size(10, 3)).unwrap();
    assert_eq!(copied(&mut term, "reader"), "ANCHOR");
    assert_eq!(
        term.view_selection("reader").unwrap().unwrap().spans[0].end,
        6
    );
    term.retain_scrollback_views(|_| false);
    assert!(term.copy_view_selection("reader").unwrap().is_none());
    assert_eq!(term.view_scrollback_offset("reader").unwrap(), 0);
}

#[test]
fn native_tick_moves_only_originating_history_view_and_stops_after_release() {
    let mut term = Terminal::new(size(20, 3)).unwrap();
    term.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix")
        .unwrap();
    let canonical = term.snapshot().unwrap();
    send(&mut term, "drag", SelectionAction::Press, 1.1, 1.5, 0, 100);
    send(&mut term, "drag", SelectionAction::Drag, 1.1, -1.0, 0, 110);
    assert_eq!(term.view_selection("drag").unwrap().unwrap().autoscroll, -1);
    assert!(
        term.select_view("drag", event(SelectionAction::Tick, 1.1, -1.0, 0, 130))
            .unwrap()
    );
    assert_eq!(term.view_scrollback_offset("drag").unwrap(), 1);
    assert_eq!(term.view_scrollback_offset("other").unwrap(), 0);
    assert_eq!(term.snapshot().unwrap(), canonical);
    send(
        &mut term,
        "drag",
        SelectionAction::Release,
        1.1,
        -1.0,
        0,
        140,
    );
    assert_eq!(term.view_selection("drag").unwrap().unwrap().autoscroll, 0);
    assert!(
        !term
            .select_view("drag", event(SelectionAction::Tick, 1.1, -1.0, 0, 160))
            .unwrap()
    );
    assert_eq!(term.view_scrollback_offset("drag").unwrap(), 1);
}

#[test]
fn screen_changes_reset_pins_and_gestures_instead_of_resurrecting_old_selection() {
    let mut term = Terminal::new(size(20, 3)).unwrap();
    term.feed(b"selected").unwrap();
    drag(&mut term, "reader", (0.1, 0.5), (7.9, 0.5), 0);
    term.feed(b"\x1b[?1049hnew screen").unwrap();
    assert!(term.copy_view_selection("reader").unwrap().is_none());
    term.feed(b"\x1b[?1049l").unwrap();
    assert!(term.view_selection("reader").unwrap().is_none());
    drag(&mut term, "reader", (0.1, 0.5), (7.9, 0.5), 0);
    term.feed(b"\x1bc").unwrap();
    assert!(term.copy_view_selection("reader").unwrap().is_none());
}

#[test]
fn first_selection_after_empty_state_screen_changes_uses_the_current_screen() {
    let mut term = Terminal::new(size(20, 3)).unwrap();
    term.feed(b"primary\x1b[?1049h\x1b[Halternate").unwrap();
    term.resize(size(18, 3)).unwrap();
    assert!(!term.has_view_selections());
    drag(&mut term, "reader", (0.1, 0.5), (8.9, 0.5), 0);
    assert_eq!(copied(&mut term, "reader"), "alternate");
    term.feed(b"\x1b[?1049l").unwrap();
    assert!(term.copy_view_selection("reader").unwrap().is_none());
}

#[test]
fn pruned_history_never_exposes_stale_endpoints_or_keeps_drag_autoscroll_alive() {
    let mut term = Terminal::new(size(20, 3)).unwrap();
    term.feed(b"old private selection").unwrap();
    drag(&mut term, "reader", (0.1, 0.5), (18.9, 0.5), 0);
    let output = "new retained row\r\n".repeat(12000);
    for bytes in output.as_bytes().chunks(64 * 1024) {
        term.feed(bytes).unwrap();
    }
    assert!(term.copy_view_selection("reader").unwrap().is_none());
    assert!(term.view_selection("reader").unwrap().is_none());
    assert!(
        !term
            .select_view("reader", event(SelectionAction::Tick, 1.0, -1.0, 0, 2000))
            .unwrap()
    );
}

#[test]
fn oversized_copy_is_rejected_without_returning_truncated_text() {
    let mut term = Terminal::new(size(500, 300)).unwrap();
    // Combining graphemes make copied UTF-8 exceed the request bound without
    // relying on a particular native cell/page memory layout or pruning count.
    let grapheme = format!("e{}", "\u{301}".repeat(40));
    let line = format!("{}\r\n", grapheme.repeat(500));
    for _ in 0..100 {
        term.feed(line.as_bytes()).unwrap();
    }
    drag(&mut term, "large", (0.1, 0.5), (499.9, 99.5), 0);
    let error = term.copy_view_selection("large").unwrap_err();
    assert!(error.to_string().contains("payload limit"));
    assert!(line.len() * 100 > MAX_INPUT_BYTES);
}
