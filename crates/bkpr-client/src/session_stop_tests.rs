fn stop_target(owner: TreeOwner) -> crate::controller::SessionStopTarget {
    crate::controller::SessionStopTarget {
        owner,
        session: SessionInfo {
            name: "selected session".into(),
            session_id: "logical session".into(),
            daemon_id: "host incarnation".into(),
            session_instance_id: "runtime incarnation".into(),
        },
        host: Some("fixture-host".into()),
    }
}

#[test]
fn session_stop_binding_is_root_only_read_only_gated_and_never_pane_input() {
    let (mut app, capture) = fixture();
    publish_test_tree(&mut app);
    app.enter_sessions();
    let mut stop = key("KeyS");
    stop.text = "s".into();
    app.key(stop.clone(), &mut Vec::new(), size(), &[]).unwrap();
    assert!(matches!(app.session_action.take(), Some(SessionAction::Tree {
        target: TreeTarget { node: TreeNode::Session, .. },
        action: crate::keyguide::Action::StopSession,
        ..
    })));
    app.options.read_only = true;
    assert!(!app.supports_action(crate::keyguide::Action::StopSession));
    app.state.suppressed.clear();
    assert!(app.key(stop, &mut Vec::new(), size(), &[]).is_err());
    assert!(app.session_action.is_none());
    assert!(capture.try_recv().is_err());
}

#[test]
fn dismissing_a_submitted_session_stop_does_not_cancel_or_repeat_its_intent() {
    let (mut app, capture) = fixture();
    publish_test_tree(&mut app);
    app.enter_sessions();
    let target = stop_target(app.tree_owner());
    app.open_form(FormKind::StopSession(target.clone()), String::new()).unwrap();
    app.state.form.as_mut().unwrap().confirmed = true;
    app.apply_form(size()).unwrap();
    assert!(app.state.form.as_ref().unwrap().pending.is_some());
    app.cancel_modal();
    assert!(app.state.form.is_none());
    assert!(app.state.sidebar.active);
    assert!(matches!(app.session_action.take(), Some(SessionAction::Stop(captured)) if captured == target));
    assert!(capture.try_recv().is_err());
}
