use super::*;

fn observer(registry: &Registry, pane_id: &str) -> String {
    send(
        registry,
        Operation::Pane {
            pane_id: pane_id.into(),
            request: PaneRequest::Attach {
                spec: ViewSpec {
                    size: size(80),
                    visible: true,
                    interactive: false,
                    input: false,
                    clipboard: false,
                },
            },
        },
    )["view_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn many_views_detach_independently_without_changing_process_lifecycle() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let panes = (0..2)
        .map(|i| spawn(registry, &fixture.home(&format!("pane-{i}")), None, None))
        .collect::<Vec<_>>();
    let views = panes
        .iter()
        .map(|pane| {
            (0..80)
                .map(|_| observer(registry, pane))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for pane in &panes {
        let scene = snapshot(registry, pane);
        assert_eq!(scene["views"].as_array().unwrap().len(), 80);
        assert!(scene["size_owner"].is_null());
        assert_eq!(scene["lifecycle"], "running");
    }
    for view in &views[0] {
        send(
            registry,
            Operation::Pane {
                pane_id: panes[0].clone(),
                request: PaneRequest::Detach {
                    view_id: view.clone(),
                },
            },
        );
    }
    let detached = snapshot(registry, &panes[0]);
    assert!(detached["views"].as_array().unwrap().is_empty());
    assert_eq!(detached["lifecycle"], "running");
    assert_eq!(
        snapshot(registry, &panes[1])["views"]
            .as_array()
            .unwrap()
            .len(),
        80
    );
    observer(registry, &panes[0]);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: panes[0].clone(),
        },
    );
    let unaffected = snapshot(registry, &panes[1]);
    assert_eq!(unaffected["views"].as_array().unwrap().len(), 80);
    assert_eq!(unaffected["lifecycle"], "running");
}

#[test]
fn attaching_to_completed_pane_expires_stale_view_and_releases_its_size_pin() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let pane_id = spawn(registry, &fixture.home("completed"), None, None);
    let old_view = view(registry, &pane_id, 80);
    send(
        registry,
        Operation::Pane {
            pane_id: pane_id.clone(),
            request: PaneRequest::Pin {
                view_id: old_view.clone(),
                size: Some(size(100)),
            },
        },
    );
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: pane_id.clone(),
        },
    );
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let finished = pane
        .shared
        .finished
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(finished);
    {
        let mut authority = pane.shared.authority.lock().unwrap();
        authority.views.get_mut(&old_view).unwrap().last_seen =
            Instant::now() - Duration::from_secs(11);
    }
    let new_view = view(registry, &pane_id, 90);
    send(
        registry,
        Operation::Pane {
            pane_id: pane_id.clone(),
            request: PaneRequest::Pin {
                view_id: new_view.clone(),
                size: Some(size(90)),
            },
        },
    );
    let scene = snapshot(registry, &pane_id);
    assert_eq!(scene["views"].as_array().unwrap().len(), 1);
    assert_eq!(scene["views"][0]["id"], new_view);
    assert_eq!(scene["size_owner"]["view_id"], new_view);
    assert_eq!(scene["size_owner"]["size"]["cols"], 90);
    let old_view_remains = pane
        .shared
        .authority
        .lock()
        .unwrap()
        .views
        .contains_key(&old_view);
    assert!(!old_view_remains);
}
