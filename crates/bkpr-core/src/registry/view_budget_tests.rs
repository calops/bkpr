use super::*;

fn observer(registry: &Registry, pane_id: &str) -> Result<Value> {
    registry.request(Request::new(Operation::Pane {
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
    }))
}

#[test]
fn global_attach_capacity_is_enforced_before_success_and_released_on_detach_and_retired_expiry() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let panes = (0..MAX_ADMITTED_VIEWS / MAX_VIEWS_PER_PANE + 1)
        .map(|i| spawn(registry, &fixture.home(&format!("pane-{i}")), None, None))
        .collect::<Vec<_>>();
    let mut first_view = String::new();
    for (i, pane) in panes[..panes.len() - 1].iter().enumerate() {
        for j in 0..MAX_VIEWS_PER_PANE {
            let attached = observer(registry, pane).unwrap();
            if i == 0 && j == 0 {
                first_view = attached["view_id"].as_str().unwrap().to_owned();
            }
        }
    }
    let overflow = panes.last().unwrap();
    let error = observer(registry, overflow).unwrap_err();
    assert!(error.to_string().contains("daemon view limit"));
    assert_eq!(
        snapshot(registry, overflow)["views"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Budgets are registry-owned: another daemon fixture can still attach normally.
    let independent = Fixture::new();
    let independent_pane = spawn(
        &independent.registry,
        &independent.home("independent"),
        None,
        None,
    );
    observer(&independent.registry, &independent_pane).unwrap();
    send(
        registry,
        Operation::Pane {
            pane_id: panes[0].clone(),
            request: PaneRequest::Detach {
                view_id: first_view,
            },
        },
    );
    let replacement = observer(registry, overflow).unwrap()["view_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(observer(registry, overflow).is_err());
    send(
        registry,
        Operation::Pane {
            pane_id: overflow.clone(),
            request: PaneRequest::Detach {
                view_id: replacement,
            },
        },
    );
    observer(registry, &panes[0]).unwrap();
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: panes[0].clone(),
        },
    );
    let retired = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&panes[0])
        .unwrap()
        .clone();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !retired
        .shared
        .finished
        .load(std::sync::atomic::Ordering::Acquire)
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    // Controlled lease time, not a ten-second sleep: completed workers no longer expire
    // views themselves, so the next Attach must reclaim their stale global slots.
    for view in retired.shared.authority.lock().unwrap().views.values_mut() {
        view.last_seen = Instant::now() - Duration::from_secs(11);
    }
    for _ in 0..MAX_VIEWS_PER_PANE {
        observer(registry, overflow).unwrap();
    }
    assert!(observer(registry, overflow).is_err());
    assert!(
        snapshot(registry, &panes[0])["views"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    send(
        registry,
        Operation::PaneForget {
            pane_id: panes[0].clone(),
        },
    );
    assert_eq!(snapshot(registry, overflow)["lifecycle"], "running");
}
