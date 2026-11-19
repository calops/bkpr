fn assert_no_application_input(received: &updates::Capture) {
    while let Ok(operation) = received.try_recv() {
        assert!(!matches!(operation, Operation::Pane {
            request: PaneRequest::Key { .. } | PaneRequest::Input { .. }
                | PaneRequest::Paste { .. } | PaneRequest::PasteContents { .. }, ..
        }), "diagnostics or blocked input leaked to the application");
    }
}

#[test]
fn failed_view_typing_preserves_one_notice_and_its_original_expiry() {
    let (mut app, received) = fixture();
    attach_fixture(&mut app);
    app.fail_view("focused-pane".into(), "The pane's display connection stopped.", &anyhow::anyhow!("fixture poll EOF"));
    let incident = app.latest_diagnostic().unwrap();
    let deadline = app.toasts.lock().unwrap().next_deadline();
    for code in ["KeyA", "KeyB", "KeyC"] {
        let mut event = key(code);
        event.text = "x".into();
        app.key(event, &mut Vec::new(), size(), &[]).unwrap();
    }
    paint_test_toasts(&mut app, size());
    let toasts = app.toasts.lock().unwrap();
    assert_eq!(toasts.cards().count(), 1);
    assert_eq!(toasts.next_deadline(), deadline);
    assert!(toasts.cards().next().unwrap().action.is_some());
    assert_eq!(app.latest_diagnostic(), Some(incident));
    assert_no_application_input(&received);
}

#[test]
fn cached_input_failure_is_one_episode_but_distinct_receipts_are_not_coalesced() {
    let (mut app, received) = fixture();
    attach_fixture(&mut app);
    let view = app.views["focused-pane"].view.clone();
    let failure = updates::InputFailure {
        id: updates::InputFailure::new_id(),
        pane_id: "focused-pane".into(), view_id: Some(view.clone()),
        operation: "key", lane: updates::InputLane::Commands,
        stopped: true, unknown_outcome: true, not_dispatched: 0,
        error: "fixture receipt ended before a result".into(),
    };
    app.record_input_failure(failure.clone());
    let first = app.latest_diagnostic().unwrap();
    let deadline = app.toasts.lock().unwrap().next_deadline();
    for _ in 0..3 {
        let result = app.enqueue("focused-pane", PaneRequest::Key {
            view_id: view.clone(), event: key("KeyA"),
        });
        app.scope_input_result(result).unwrap();
    }
    paint_test_toasts(&mut app, size());
    assert_eq!(app.toasts.lock().unwrap().cards().count(), 1);
    assert_eq!(app.toasts.lock().unwrap().next_deadline(), deadline);
    assert_eq!(app.latest_diagnostic(), Some(first));
    assert_eq!(app.diagnostics.lock().unwrap().get(first).unwrap().outcome, diagnostics::Outcome::Unknown);
    let mut distinct = failure;
    distinct.id = updates::InputFailure::new_id();
    app.record_input_failure(distinct);
    assert_ne!(app.latest_diagnostic(), Some(first));
    paint_test_toasts(&mut app, size());
    assert_eq!(app.toasts.lock().unwrap().cards().count(), 2);
    assert_no_application_input(&received);
}

#[test]
fn details_preserve_pending_and_unknown_forms_and_consume_paste_and_keys() {
    for ambiguous in [false, true] {
        let (mut app, received) = fixture();
        attach_fixture(&mut app);
        let mut form = Form::new(FormKind::RenamePane, "original name".into(), Some("focused-pane".into()));
        form.pending = (!ambiguous).then(|| "accepted-operation".into());
        form.ambiguous = ambiguous;
        form.selection = 4;
        let pending = form.pending.clone();
        app.state.form = Some(form);
        let focused = app.state.focused.clone();
        let id = app.report_failure("Couldn't confirm this action.", &transport::mark_outcome_unknown(anyhow::anyhow!("fixture lost receipt")));
        app.open_diagnostic(id).unwrap();
        assert!(app.supports_action(crate::keyguide::Action::Cancel));
        assert!(app.supports_action(crate::keyguide::Action::Navigate(crate::keyguide::Navigation::PageNext)));
        app.paste("must not reach the form or pane".into()).unwrap();
        let mut event = key("KeyA");
        event.text = "x".into();
        app.key(event, &mut Vec::new(), size(), &[]).unwrap();
        app.key(key("Escape"), &mut Vec::new(), size(), &[]).unwrap();
        assert!(app.diagnostic_view.is_none());
        let form = app.state.form.as_ref().unwrap();
        assert_eq!(form.fields.primary.value, "original name");
        assert_eq!(form.pending, pending);
        assert_eq!(form.ambiguous, ambiguous);
        assert_eq!(form.selection, 4);
        assert_eq!(app.state.focused, focused);
        assert_eq!(app.diagnostics.lock().unwrap().get(id).unwrap().outcome, diagnostics::Outcome::Unknown);
        assert_no_application_input(&received);
    }
}

#[test]
fn expired_focused_incident_does_not_hide_retained_diagnostics() {
    let (mut app, _received) = fixture();
    attach_fixture(&mut app);
    app.fail_view("focused-pane".into(), "The pane stopped displaying.", &anyhow::anyhow!("fixture EOF"));
    let old = app.latest_diagnostic().unwrap();
    let latest = {
        let mut store = app.diagnostics.lock().unwrap();
        for _ in 0..128 {
            store.record("A separate operation failed.", "retained technical context", diagnostics::Outcome::NotApplicable);
        }
        assert!(store.get(old).is_none());
        store.latest().unwrap()
    };
    assert_eq!(app.latest_diagnostic(), Some(latest));
    app.state.prefix = true;
    let mut event = key("Digit1");
    event.text = "!".into();
    event.modifiers = 1;
    app.key(event, &mut Vec::new(), size(), &[]).unwrap();
    assert_eq!(app.diagnostic_view.as_ref().map(|viewer| viewer.id), Some(latest));
}

#[test]
fn diagnostics_without_incidents_consumes_the_prefix_without_dispatching_to_the_pane() {
    let (mut app, received) = fixture();
    attach_fixture(&mut app);
    physical_keys(&mut app, b"\x02!").unwrap();
    assert!(!app.state.prefix);
    assert!(app.diagnostic_view.is_none());
    assert_no_application_input(&received);
}

#[test]
fn failed_initial_attach_retires_its_loading_deadline() {
    let (mut app, _received) = fixture();
    let pane = "loading-pane";
    let rects = vec![(pane.to_owned(), Rect {
        x: 0, y: 0, width: size().cols, height: size().rows,
    })];
    app.begin_image_attach(pane, size());
    app.paint_image_feedback(size(), &rects, &Theme::default());
    let key = format!("attach:{pane}");
    app.jobs.submit(key.clone(), || bail!("initial frame unavailable")).unwrap();
    wait_background(&mut app, &key);
    app.paint_image_feedback(size(), &rects, &Theme::default());
    assert_eq!(app.image_timeout(1000, Instant::now()), 1000);
    assert!(app.view_failures.contains_key(pane));
    assert!(app.latest_diagnostic().is_some());
}

#[test]
fn visible_loading_is_not_hidden_by_keyboard_focus_loss() {
    let (mut app, _received) = fixture();
    let pane = "loading-pane";
    let rects = [(pane.to_owned(), Rect {
        x: 0, y: 0, width: size().cols, height: size().rows,
    })];
    app.begin_image_attach(pane, size());
    app.host_focused = false;
    app.paint_image_feedback(size(), &rects, &Theme::default());
    assert!(app.image_timeout(1000, Instant::now()) < 1000);
    app.backgrounded = true;
    app.paint_image_feedback(size(), &rects, &Theme::default());
    assert_eq!(app.image_timeout(1000, Instant::now()), 1000);
}
