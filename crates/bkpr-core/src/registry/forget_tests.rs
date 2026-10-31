use super::*;

#[test]
fn forget_requires_retirement_no_layout_references_and_never_touches_running_panes() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "owner");
    let b = create(registry, "projection");
    let one = spawn(registry, &fixture.home("retired"), Some(a.clone()), Some(1));
    let parallel = spawn(registry, &fixture.home("parallel"), None, None);
    let parallel_pid = snapshot(registry, &parallel)["pid"].clone();
    assert!(
        registry
            .request(Request::new(Operation::PaneForget {
                pane_id: one.clone()
            }))
            .unwrap_err()
            .to_string()
            .contains("running pane")
    );
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: one.clone(),
        },
    );
    let old_handle = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&one)
        .unwrap()
        .clone();
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: b.clone(),
            expected_revision: 1,
            layout: Some(LayoutNode::Leaf {
                pane_id: one.clone(),
            }),
        },
    );
    assert!(
        registry
            .request(Request::new(Operation::PaneForget {
                pane_id: one.clone()
            }))
            .unwrap_err()
            .to_string()
            .contains("referenced")
    );
    send(
        registry,
        Operation::WorkspaceRemove {
            workspace_id: a,
            expected_revision: 3,
        },
    );
    assert!(
        registry
            .request(Request::new(Operation::PaneForget {
                pane_id: one.clone()
            }))
            .unwrap_err()
            .to_string()
            .contains(&b)
    );
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: b,
            expected_revision: 2,
            layout: None,
        },
    );
    let forgotten = send(
        registry,
        Operation::PaneForget {
            pane_id: one.clone(),
        },
    );
    let forgotten: RegistryResult = serde_json::from_value(forgotten).unwrap();
    assert!(!forgotten.registry.panes.iter().any(|pane| pane.id == one));
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: one,
                request: PaneRequest::Inspect
            }))
            .is_err()
    );
    assert!(
        old_handle
            .request(PaneRequest::Attach {
                spec: ViewSpec {
                    size: size(80),
                    interactive: false,
                    visible: true,
                    input: false,
                    clipboard: false
                }
            })
            .unwrap_err()
            .to_string()
            .contains("forgotten")
    );
    let still_running = snapshot(registry, &parallel);
    assert_eq!(still_running["pid"], parallel_pid);
    assert_eq!(still_running["lifecycle"], "running");
}

#[test]
fn active_view_blocks_forget_until_explicit_detach() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = spawn(registry, &fixture.home("viewed"), None, None);
    let client = view(registry, &id, 80);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: id.clone(),
        },
    );
    assert!(
        registry
            .request(Request::new(Operation::PaneForget {
                pane_id: id.clone()
            }))
            .unwrap_err()
            .to_string()
            .contains("active views")
    );
    send(
        registry,
        Operation::Pane {
            pane_id: id.clone(),
            request: PaneRequest::Detach { view_id: client },
        },
    );
    send(registry, Operation::PaneForget { pane_id: id });
    assert!(registry.snapshot().unwrap().panes.is_empty());
}

#[test]
fn explicit_forget_releases_record_capacity_without_automatic_pruning() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let ids = (0..MAX_PANE_RECORDS)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    registry
        .mutate(|r| {
            for id in &ids {
                r.panes.push(PaneInfo {
                    id: id.clone(),
                    workspace_id: None,
                    title: "retired".into(),
                    status: PaneStatus::Stopped,
                    exit_code: None,
                    cwd: fixture._root.path().into(),
                    home: fixture._root.path().into(),
                    isolated_environment: false,
                });
            }
            Ok(())
        })
        .unwrap();
    let home = fixture.home("capacity");
    assert!(
        registry
            .request(Request::new(Operation::PaneSpawn {
                workspace_id: None,
                expected_revision: None,
                spec: spec(&home)
            }))
            .unwrap_err()
            .to_string()
            .contains("pane limit")
    );
    assert!(!home.exists());
    assert_eq!(registry.snapshot().unwrap().panes.len(), MAX_PANE_RECORDS);
    send(
        registry,
        Operation::PaneForget {
            pane_id: ids[0].clone(),
        },
    );
    let new = spawn(registry, &home, None, None);
    let r = registry.snapshot().unwrap();
    assert_eq!(r.panes.len(), MAX_PANE_RECORDS);
    assert!(!r.panes.iter().any(|pane| pane.id == ids[0]));
    assert!(
        r.panes
            .iter()
            .any(|pane| pane.id == new && pane.status == PaneStatus::Running)
    );
    assert_eq!(
        r.panes
            .iter()
            .filter(|pane| pane.status == PaneStatus::Stopped)
            .count(),
        MAX_PANE_RECORDS - 1
    );
}

#[test]
fn failed_forget_persistence_keeps_metadata_and_retained_runtime_fail_closed() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = spawn(registry, &fixture.home("retained"), None, None);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: id.clone(),
        },
    );
    let retained = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&id)
        .unwrap()
        .clone();
    let path = fixture._root.path().join("registry.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::PaneForget {
                pane_id: id.clone()
            }))
            .is_err()
    );
    assert!(
        registry
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("persistence failed")
    );
    let state = registry.inner.state.lock().unwrap();
    assert!(state.record.registry.panes.iter().any(|pane| pane.id == id));
    assert!(state.live.contains_key(&id));
    assert!(
        !retained
            .shared
            .forgotten
            .load(std::sync::atomic::Ordering::Acquire)
    );
}
