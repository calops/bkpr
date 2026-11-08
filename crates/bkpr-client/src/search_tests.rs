fn search_result(query: &str, generation: u64, running: bool, total: u64) -> SearchSnapshot {
    SearchSnapshot {
        query: query.into(), generation, running, total_matches: total,
        selected_index: (total > 0).then_some(0),
        spans: if total > 0 { vec![SearchSpan { row: 0, start: 0, end: 2, selected: true }] } else { vec![] },
    }
}
fn assert_search_query(operation: Operation, expected: &str) {
    assert!(matches!(operation, Operation::Pane {
        request: PaneRequest::Search { action: SearchAction::Query { text }, .. }, ..
    } if text == expected));
}

#[test]
fn search_read_only_editor_owns_text_paste_navigation_and_escape_without_app_input() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.options.read_only = true;
    let mut prefix = key("KeyB");
    prefix.modifiers = 2;
    app.key(prefix, &mut Vec::new(), size(), &[]).unwrap();
    let mut slash = key("Slash");
    slash.text = "/".into();
    app.key(slash, &mut Vec::new(), size(), &[]).unwrap();
    assert!(app.search_open());
    app.paste("界e\u{301}".into()).unwrap();
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 7})), "界e\u{301}");
    app.flush_search().unwrap();
    app.key(key("Enter"), &mut Vec::new(), size(), &[]).unwrap();
    app.flush_search().unwrap();
    assert!(matches!(capture.respond(serde_json::json!({"generation": 7})),
        Operation::Pane { request: PaneRequest::Search { action: SearchAction::Next, .. }, .. }));
    app.flush_search().unwrap();
    let mut previous = key("Enter");
    previous.modifiers = 1;
    app.key(previous, &mut Vec::new(), size(), &[]).unwrap();
    app.flush_search().unwrap();
    assert!(matches!(capture.respond(serde_json::json!({"generation": 7})),
        Operation::Pane { request: PaneRequest::Search { action: SearchAction::Previous, .. }, .. }));
    app.flush_search().unwrap();
    app.key(key("Escape"), &mut Vec::new(), size(), &[]).unwrap();
    assert!(!app.search_open());
    app.flush_search().unwrap();
    assert!(matches!(capture.try_recv().unwrap(),
        Operation::Pane { request: PaneRequest::Search { action: SearchAction::Clear, .. }, .. }));
    assert!(capture.try_recv().is_err());
}

#[test]
fn search_coalesces_latest_query_and_rejects_old_generations_even_for_repeated_text() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.begin_search().unwrap();
    app.search_text("a");
    app.flush_search().unwrap();
    app.search_text("b");
    app.search_text("c");
    assert_search_query(capture.respond(serde_json::json!({"generation": 1})), "a");
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 2})), "abc");
    let mut clear = key("KeyU");
    clear.modifiers = 2;
    app.key(clear, &mut Vec::new(), size(), &[]).unwrap();
    app.search_text("a");
    app.flush_search().unwrap();
    app.views.get_mut("focused-pane").unwrap().metadata.search = Some(search_result("a", 1, false, 1));
    assert!(app.search_spans("focused-pane").is_empty());
    assert_search_query(capture.respond(serde_json::json!({"generation": 3})), "a");
    app.flush_search().unwrap();
    assert!(app.search_spans("focused-pane").is_empty());
    app.views.get_mut("focused-pane").unwrap().metadata.search = Some(search_result("a", 3, false, 1));
    assert_eq!(app.search_spans("focused-pane").len(), 1);
    app.flush_search().unwrap();
    assert!(capture.try_recv().is_err(), "idle search must not poll or requeue");
}

#[test]
fn search_retirement_hides_delayed_results_and_preserves_preexisting_key_release() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.key(key("KeyA"), &mut Vec::new(), size(), &[]).unwrap();
    assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Key { event, .. }, ..
    } if event.action == KeyAction::Press));
    app.begin_search().unwrap();
    app.search_text("needle");
    app.flush_search().unwrap();
    capture.respond(serde_json::json!({"generation": 1}));
    app.flush_search().unwrap();
    let mut release = key("KeyA");
    release.action = KeyAction::Release;
    app.key(release, &mut Vec::new(), size(), &[]).unwrap();
    assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Key { event, .. }, ..
    } if event.action == KeyAction::Release));
    app.state.focused = Some("different-pane".into());
    app.validate_search();
    assert!(!app.search_open());
    app.views.get_mut("focused-pane").unwrap().metadata.search = Some(search_result("needle", 1, false, 1));
    assert!(app.search_spans("focused-pane").is_empty());
    app.flush_search().unwrap();
    assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        request: PaneRequest::Search { action: SearchAction::Clear, .. }, ..
    }));
}

#[test]
fn search_overlay_reports_incomplete_counts_no_match_and_current_without_covering_pane() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.begin_search().unwrap();
    app.search_text("needle");
    app.flush_search().unwrap();
    capture.respond(serde_json::json!({"generation": 9}));
    app.flush_search().unwrap();
    let mut overlay = UiSurface::new(size());
    for (running, total, expected) in [
        (true, 0, "searching"), (true, 3, "1/3 found"),
        (false, 0, "no matches"), (false, 3, "1/3 newest"),
    ] {
        app.views.get_mut("focused-pane").unwrap().metadata.search = Some(search_result("needle", 9, running, total));
        assert!(app.search_overlay(&mut overlay, size()));
        assert_eq!(overlay.frame.size.rows, 1);
        let label = overlay.frame.rows[0].cells.iter().map(|cell| cell.text.as_str()).collect::<String>();
        assert!(label.contains(expected), "{label}");
        assert!(overlay.frame.cursor.visible);
    }
    app.views.get_mut("focused-pane").unwrap().retirement_requested = true;
    app.validate_search();
    assert!(!app.search_overlay(&mut overlay, size()));
}

#[test]
fn search_modal_workspace_and_view_replacement_retire_editor_without_revival() {
    for reason in 0..3 {
        let (mut app, capture) = fixture();
        attach_fixture(&mut app);
        app.begin_search().unwrap();
        app.search_text("old");
        app.flush_search().unwrap();
        match reason {
            0 => app.state.open(FormKind::ProjectDetails, String::new()),
            1 => app.state.workspace = Some("another-workspace".into()),
            _ => app.views.get_mut("focused-pane").unwrap().view = "replacement-view".into(),
        }
        app.validate_search();
        assert!(!app.search_open());
        capture.respond(serde_json::json!({"generation": 5}));
        app.flush_search().unwrap();
        assert!(app.search_spans("focused-pane").is_empty());
        assert!(!app.search_open());
        if reason == 2 {
            assert!(capture.try_recv().is_err(), "never clear a replacement view");
        } else {
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                request: PaneRequest::Search { action: SearchAction::Clear, .. }, ..
            }));
        }
    }
}

#[test]
fn search_slow_retired_view_does_not_block_another_panes_query_or_input() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.begin_search().unwrap();
    app.search_text("old");
    app.flush_search().unwrap();
    // The capture deliberately keeps the first completion outstanding while
    // another view starts its independent inspection and application lane.
    attach_named_fixture(&mut app, "other-pane", "other-view");
    app.validate_search();
    app.begin_search().unwrap();
    app.search_text("new");
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 1})), "old");
    assert_search_query(capture.respond(serde_json::json!({"generation": 2})), "new");
    app.flush_search().unwrap();
    assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
        pane_id, request: PaneRequest::Search { action: SearchAction::Clear, .. }
    } if pane_id == "focused-pane"));
}

#[test]
fn search_query_limits_and_backspace_preserve_complete_unicode_without_native_input() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.begin_search().unwrap();
    app.search_text("a界");
    app.key(key("Backspace"), &mut Vec::new(), size(), &[]).unwrap();
    app.search_text(&"x".repeat(MAX_SEARCH_BYTES));
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 1})), "a");
    app.flush_search().unwrap();
    assert!(capture.try_recv().is_err());
}

#[test]
fn search_editor_repeats_text_backspace_and_navigation_but_never_replays_closed_key_tails() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    let mut prefix = key("KeyB");
    prefix.modifiers = 2;
    app.key(prefix.clone(), &mut Vec::new(), size(), &[]).unwrap();
    prefix.action = KeyAction::Repeat;
    app.key(prefix, &mut Vec::new(), size(), &[]).unwrap();
    assert!(app.state.prefix, "held prefix repeat must not consume its command");
    let mut slash = key("Slash");
    slash.text = "/".into();
    app.key(slash.clone(), &mut Vec::new(), size(), &[]).unwrap();
    slash.action = KeyAction::Repeat;
    app.key(slash, &mut Vec::new(), size(), &[]).unwrap();
    let mut text = key("KeyA");
    text.text = "a".into();
    app.key(text.clone(), &mut Vec::new(), size(), &[]).unwrap();
    text.action = KeyAction::Repeat;
    app.key(text.clone(), &mut Vec::new(), size(), &[]).unwrap();
    app.key(text.clone(), &mut Vec::new(), size(), &[]).unwrap();
    let mut backspace = key("Backspace");
    app.key(backspace.clone(), &mut Vec::new(), size(), &[]).unwrap();
    backspace.action = KeyAction::Repeat;
    app.key(backspace, &mut Vec::new(), size(), &[]).unwrap();
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 1})), "a");
    app.flush_search().unwrap();
    let mut enter = key("Enter");
    app.key(enter.clone(), &mut Vec::new(), size(), &[]).unwrap();
    enter.action = KeyAction::Repeat;
    app.key(enter.clone(), &mut Vec::new(), size(), &[]).unwrap();
    for _ in 0..2 {
        app.flush_search().unwrap();
        assert!(matches!(capture.respond(serde_json::json!({"generation": 1})), Operation::Pane {
            request: PaneRequest::Search { action: SearchAction::Next, .. }, ..
        }));
    }
    app.flush_search().unwrap();
    app.key(key("Escape"), &mut Vec::new(), size(), &[]).unwrap();
    app.key(text.clone(), &mut Vec::new(), size(), &[]).unwrap();
    app.key(enter, &mut Vec::new(), size(), &[]).unwrap();
    assert!(capture.try_recv().is_err(), "closed editor repeat tails must not become application input");
    text.action = KeyAction::Release;
    app.key(text, &mut Vec::new(), size(), &[]).unwrap();
    assert!(capture.try_recv().is_err());
}

#[test]
fn search_local_rejections_preserve_results_and_accept_a_later_valid_edit() {
    let (mut app, capture) = fixture();
    attach_fixture(&mut app);
    app.begin_search().unwrap();
    app.search_text("needle");
    app.flush_search().unwrap();
    capture.respond(serde_json::json!({"generation": 1}));
    app.flush_search().unwrap();
    app.views.get_mut("focused-pane").unwrap().metadata.search = Some(search_result("needle", 1, false, 3));
    app.search_text(&"x".repeat(MAX_SEARCH_BYTES));
    assert_eq!(app.search_spans("focused-pane").len(), 1, "local rejection is not a fatal search error");
    for _ in 0..65 {
        app.key(key("Enter"), &mut Vec::new(), size(), &[]).unwrap();
    }
    assert_eq!(app.search_spans("focused-pane").len(), 1);
    app.search_text("x");
    app.flush_search().unwrap();
    assert_search_query(capture.respond(serde_json::json!({"generation": 2})), "needlex");
}
