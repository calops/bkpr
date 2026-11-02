use bkpr_protocol::{MAX_SEARCH_BYTES, SearchAction, SearchSnapshot, Size};
use bkpr_terminal_engine::Terminal;

fn size(cols: u16, rows: u16) -> Size {
    Size {
        cols,
        rows,
        cell_width: 8,
        cell_height: 16,
    }
}
fn query(term: &mut Terminal, id: &str, text: &str) {
    term.search_view(id, 1, SearchAction::Query { text: text.into() })
        .unwrap();
}
fn finish(term: &mut Terminal, id: &str) -> SearchSnapshot {
    for _ in 0..20000 {
        term.advance_search().unwrap();
        if !term.has_search_work().unwrap() {
            return term.view_search(id).unwrap().unwrap();
        }
    }
    panic!("native search failed to finish bounded progress");
}
#[test]
fn native_search_finds_history_navigates_one_view_and_preserves_live_scene() {
    let mut term = Terminal::new(size(30, 4)).unwrap();
    for row in 0..300 {
        term.feed(format!("marker {row:04}\r\n").as_bytes())
            .unwrap();
    }
    // Move every marker beyond the live viewport: index zero must itself
    // require history navigation, not merely select an already-visible row.
    term.feed(b"live tail\r\nlive tail\r\nlive tail\r\nlive tail\r\n")
        .unwrap();
    term.feed("hello 界 e\u{301}".as_bytes()).unwrap();
    let live = term.snapshot().unwrap();
    query(&mut term, "reader", "marker");
    let first = finish(&mut term, "reader");
    assert_eq!(first.total_matches, 300);
    assert_eq!(first.selected_index, Some(0));
    assert!(first.spans.iter().any(|span| span.selected));
    let offset = term.view_scrollback_offset("reader").unwrap();
    assert!(offset > 0);
    assert_eq!(term.view_scrollback_offset("other").unwrap(), 0);
    assert!(term.view_search("other").unwrap().is_none());
    term.search_view("reader", 1, SearchAction::Next).unwrap();
    assert_eq!(
        term.view_search("reader").unwrap().unwrap().selected_index,
        Some(1)
    );
    let older_offset = term.view_scrollback_offset("reader").unwrap();
    assert!(older_offset > offset);
    term.search_view("reader", 1, SearchAction::Previous)
        .unwrap();
    assert_eq!(
        term.view_search("reader").unwrap().unwrap().selected_index,
        Some(0)
    );
    assert_eq!(term.snapshot().unwrap(), live);
    // Previous selects the newer match already visible in this viewport.
    // Native IfNeeded deliberately does not recenter it.
    assert_eq!(term.view_scrollback_offset("reader").unwrap(), older_offset);
    term.search_view("reader", 1, SearchAction::Clear).unwrap();
    assert!(term.view_search("reader").unwrap().is_none());
    assert_eq!(term.view_scrollback_offset("reader").unwrap(), older_offset);
    term.scroll_view("reader", i64::MIN).unwrap();
    assert_eq!(term.view_scrollback_offset("reader").unwrap(), 0);
}
#[test]
fn native_search_keeps_unicode_offsets_case_semantics_and_query_generations() {
    let mut term = Terminal::new(size(30, 4)).unwrap();
    term.feed("hello 界 e\u{301} Éé".as_bytes()).unwrap();
    query(&mut term, "v", "HELLO");
    let first = finish(&mut term, "v");
    assert_eq!(first.total_matches, 1);
    query(&mut term, "v", "界");
    let wide = finish(&mut term, "v");
    assert_eq!(wide.total_matches, 1);
    assert!(
        wide.spans
            .iter()
            .any(|s| s.row == 0 && s.start == 6 && s.end >= 7)
    );
    query(&mut term, "v", "e\u{301}");
    assert_eq!(finish(&mut term, "v").total_matches, 1);
    query(&mut term, "v", "é");
    assert_eq!(finish(&mut term, "v").total_matches, 1);
    query(&mut term, "v", "HELLO");
    let again = finish(&mut term, "v");
    assert!(again.generation > first.generation);
    assert_eq!(again.total_matches, 1);
    query(&mut term, "v", "absent");
    let absent = finish(&mut term, "v");
    assert_eq!(absent.total_matches, 0);
    assert!(absent.spans.is_empty());
    assert_eq!(absent.selected_index, None);
    assert!(
        term.search_view(
            "v",
            1,
            SearchAction::Query {
                text: "x".repeat(MAX_SEARCH_BYTES + 1)
            }
        )
        .is_err()
    );
    assert_eq!(term.view_search("v").unwrap().unwrap().query, "absent");
}
#[test]
fn output_reflow_screens_retention_and_epoch_retirement_reconcile_native_state() {
    let mut term = Terminal::new(size(40, 4)).unwrap();
    term.feed(b"unique-old marker\r\nmarker").unwrap();
    query(&mut term, "v", "marker");
    assert_eq!(finish(&mut term, "v").total_matches, 2);
    term.feed(b"\r\nmarker").unwrap();
    assert_eq!(finish(&mut term, "v").total_matches, 3);
    term.resize(size(12, 6)).unwrap();
    assert_eq!(finish(&mut term, "v").total_matches, 3);
    term.feed(b"\x1b[?1049halt marker").unwrap();
    assert_eq!(finish(&mut term, "v").total_matches, 1);
    term.feed(b"\x1b[?1049l").unwrap();
    assert_eq!(finish(&mut term, "v").total_matches, 3);
    term.feed(b"\x1b[?1049hmatch match").unwrap();
    query(&mut term, "v", "match");
    assert_eq!(finish(&mut term, "v").total_matches, 2);
    term.search_view("v", 1, SearchAction::Next).unwrap();
    assert_eq!(
        term.view_search("v").unwrap().unwrap().selected_index,
        Some(1)
    );
    term.search_view("v", 1, SearchAction::Previous).unwrap();
    assert_eq!(
        term.view_search("v").unwrap().unwrap().selected_index,
        Some(0)
    );
    assert_eq!(term.view_scrollback_offset("v").unwrap(), 0);
    term.feed(b"\x1b[?1049l").unwrap();
    assert_eq!(finish(&mut term, "v").total_matches, 0);
    query(&mut term, "v", "marker");
    assert_eq!(finish(&mut term, "v").total_matches, 3);
    query(&mut term, "v", "unique-old");
    assert_eq!(finish(&mut term, "v").total_matches, 1);
    for _ in 0..11000 {
        term.feed(b"\r\nreplacement").unwrap();
    }
    assert_eq!(finish(&mut term, "v").total_matches, 0);
    term.retain_search_views(|_| Some(2));
    assert!(!term.has_view_searches());
    assert!(term.view_search("v").unwrap().is_none());
    assert!(!term.has_search_work().unwrap());
}
#[test]
fn dense_results_and_query_cancellation_remain_exact_and_view_local() {
    let mut term = Terminal::new(size(80, 4)).unwrap();
    for _ in 0..1000 {
        term.feed(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n")
            .unwrap();
    }
    query(&mut term, "dense", "x");
    query(&mut term, "other", "absent");
    term.advance_search().unwrap();
    query(&mut term, "dense", "replacement");
    let cancelled = finish(&mut term, "dense");
    assert_eq!(cancelled.total_matches, 0);
    query(&mut term, "dense", "x");
    let dense = finish(&mut term, "dense");
    assert_eq!(dense.total_matches, 40000);
    assert!(dense.spans.len() <= 80 * 4);
    assert_eq!(term.view_search("other").unwrap().unwrap().total_matches, 0);
    assert!(!term.has_search_work().unwrap());
    assert!(term.advance_search().unwrap().is_none());
}

#[test]
fn selected_match_recovers_after_resize_and_soft_wrap_reflow() {
    let mut term = Terminal::new(size(76, 8)).unwrap();
    term.feed(format!("{}hello", "x".repeat(70)).as_bytes())
        .unwrap();
    query(&mut term, "reader", "hello");
    let initial = finish(&mut term, "reader");
    assert_eq!(initial.selected_index, Some(0));
    assert!(initial.spans.iter().any(|span| span.selected));
    for cols in [78, 76, 73, 40, 76] {
        term.resize(size(cols, 8)).unwrap();
        let resized = finish(&mut term, "reader");
        assert_eq!(resized.generation, initial.generation);
        assert_eq!(resized.total_matches, 1);
        assert_eq!(
            resized.selected_index,
            Some(0),
            "selection lost at {cols} columns"
        );
        assert!(resized.spans.iter().any(|span| span.selected));
        assert!(term.view_search("other").unwrap().is_none());
        assert_eq!(term.view_scrollback_offset("other").unwrap(), 0);
    }
}
