use super::*;

fn initial(registry: &Registry, home: &Path) -> RegistryResult {
    serde_json::from_value(send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec(home)),
        },
    ))
    .unwrap()
}

#[test]
fn independent_windows_have_no_backend_active_selection_and_close_only_owned_executions() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let first = initial(registry, &fixture.home("first"));
    let workspace_id = first.workspace_id.unwrap();
    let first_pane = first.pane_id.unwrap();
    let w = &first.registry.workspaces[0];
    let second: RegistryResult = serde_json::from_value(send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace_id.clone(),
            expected_revision: w.revision,
            name: None,
            spec: spec(&fixture.home("second")),
        },
    ))
    .unwrap();
    let second_pane = second.pane_id.unwrap();
    let second_window = second.window_id.unwrap();
    let workspace = &second.registry.workspaces[0];
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(
        workspace.windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: first_pane.clone()
        })
    );
    assert_eq!(
        workspace.windows[1].layout,
        Some(LayoutNode::Leaf {
            pane_id: second_pane.clone()
        })
    );
    let first_view = view(registry, &first_pane, 100);
    let second_view = view(registry, &second_pane, 70);
    send(
        registry,
        Operation::WindowClose {
            workspace_id: workspace_id.clone(),
            window_id: second_window,
            expected_revision: workspace.windows[1].revision,
        },
    );
    assert_eq!(snapshot(registry, &first_pane)["lifecycle"], "running");
    assert_eq!(snapshot(registry, &second_pane)["lifecycle"], "terminated");
    assert_eq!(registry.snapshot().unwrap().workspaces[0].windows.len(), 1);
    assert!(!registry.session_end_requested());
    send(
        registry,
        Operation::Pane {
            pane_id: first_pane.clone(),
            request: PaneRequest::Input {
                view_id: first_view,
                data: Vec::new(),
            },
        },
    );
    send(
        registry,
        Operation::Pane {
            pane_id: second_pane,
            request: PaneRequest::Detach {
                view_id: second_view,
            },
        },
    );
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: first_pane,
        },
    );
    assert!(registry.snapshot().unwrap().workspaces.is_empty());
    assert!(registry.session_end_requested());
}
#[test]
fn natural_last_exit_requests_end_without_client_polling_and_keeps_final_receipt() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (tx, rx) = std::sync::mpsc::channel();
    registry.set_session_end_waker(Arc::new(move || {
        let _ = tx.send(());
    }));
    let home = fixture.home("natural");
    let mut launch = spec(&home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "while [ ! -e finish ]; do sleep 0.01; done; exit 7".into(),
    ];
    let created = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(launch),
        },
    );
    let pane_id = created["pane_id"].as_str().unwrap();
    fs::write(home.join("finish"), b"").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let r = registry.snapshot().unwrap();
    assert!(r.workspaces.is_empty());
    let pane = r.panes.iter().find(|p| p.id == pane_id).unwrap();
    assert_eq!(pane.status, PaneStatus::Exited);
    assert_eq!(pane.exit_code, Some(7));
    assert!(pane.workspace_id.is_none() && pane.window_id.is_none());
    assert_eq!(snapshot(registry, pane_id)["lifecycle"], "exited");
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: None
            }))
            .is_err()
    );
    let (tx, rx) = std::sync::mpsc::channel();
    registry.set_session_end_waker(Arc::new(move || {
        let _ = tx.send(());
    }));
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
}
#[test]
fn empty_admin_standalone_detach_and_layout_only_removal_do_not_end_session() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    assert!(!registry.session_end_requested());
    let admin = create(registry, "admin");
    let standalone = spawn(registry, &fixture.home("standalone"), None, None);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: standalone,
        },
    );
    assert!(!registry.session_end_requested());
    assert_eq!(registry.snapshot().unwrap().workspaces[0].id, admin);
    let created = initial(registry, &fixture.home("bound"));
    let pane = created.pane_id.unwrap();
    let workspace = &created.registry.workspaces[1];
    let window = &workspace.windows[0];
    let id = view(registry, &pane, 80);
    send(
        registry,
        Operation::Pane {
            pane_id: pane.clone(),
            request: PaneRequest::Detach { view_id: id },
        },
    );
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: workspace.id.clone(),
            window_id: window.id.clone(),
            expected_revision: window.revision,
            layout: None,
        },
    );
    assert_eq!(snapshot(registry, &pane)["lifecycle"], "running");
    assert!(!registry.session_end_requested());
    send(registry, Operation::PaneTerminate { pane_id: pane });
    assert_eq!(registry.snapshot().unwrap().workspaces.len(), 1);
    assert!(!registry.session_end_requested());
}
#[test]
fn faults_keep_owned_recovery_documents_instead_of_closing_session() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let created = initial(registry, &fixture.home("fault"));
    let pane_id = created.pane_id.unwrap();
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    pane.shared.published.lock().unwrap().lifecycle = "fault".into();
    registry.record_completion(&pane_id).unwrap();
    let r = registry.snapshot().unwrap();
    assert_eq!(r.panes[0].status, PaneStatus::Stopped);
    assert_eq!(r.workspaces.len(), 1);
    assert_eq!(r.workspaces[0].windows.len(), 1);
    assert!(!registry.session_end_requested());
}
#[test]
fn final_close_persistence_failure_never_requests_successful_session_end() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "admin");
    let path = fixture._root.path().join("registry.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceClose {
                workspace_id: id,
                expected_revision: 1
            }))
            .is_err()
    );
    assert!(!registry.session_end_requested());
    assert!(registry.snapshot().is_err());
}

#[test]
fn native_close_durability_failure_reports_completed_side_effects_and_suspends_commits() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let created = initial(registry, &fixture.home("close-failure"));
    let pane_id = created.pane_id.unwrap();
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let path = fixture._root.path().join("registry.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceClose {
                workspace_id: created.workspace_id.unwrap(),
                expected_revision: created.registry.workspaces[0].revision,
            }))
            .is_err()
    );
    assert_eq!(
        pane.request(PaneRequest::Inspect).unwrap()["lifecycle"],
        "terminated"
    );
    assert!(!registry.session_end_requested());
    assert!(registry.snapshot().is_err());
    assert_eq!(
        registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .registry
            .workspaces
            .len(),
        1
    );
}

#[test]
fn registering_end_waker_concurrently_with_final_commit_has_no_lost_notification() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "admin");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker_registry = registry.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        worker_registry.set_session_end_waker(Arc::new(move || {
            let _ = tx.send(());
        }));
    });
    barrier.wait();
    send(
        registry,
        Operation::WorkspaceClose {
            workspace_id: id,
            expected_revision: 1,
        },
    );
    worker.join().unwrap();
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(registry.session_end_requested());
}

#[test]
fn window_revisions_are_independent_and_workspace_close_cas_covers_all_membership_edits() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let first = initial(registry, &fixture.home("first-revision"));
    let workspace_id = first.workspace_id.unwrap();
    let created = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace_id.clone(),
            expected_revision: first.registry.workspaces[0].revision,
            name: Some("second".into()),
            spec: spec(&fixture.home("second-revision")),
        },
    );
    let result: RegistryResult = serde_json::from_value(created).unwrap();
    let workspace = &result.registry.workspaces[0];
    let one = &workspace.windows[0];
    let two = &workspace.windows[1];
    send(
        registry,
        Operation::WindowRename {
            workspace_id: workspace_id.clone(),
            window_id: one.id.clone(),
            expected_revision: one.revision,
            name: "renamed".into(),
        },
    );
    assert!(
        registry
            .request(Request::new(Operation::WindowRename {
                workspace_id: workspace_id.clone(),
                window_id: one.id.clone(),
                expected_revision: one.revision,
                name: "stale".into(),
            }))
            .is_err()
    );
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceClose {
                workspace_id: workspace_id.clone(),
                expected_revision: workspace.revision
            }))
            .is_err()
    );
    let split = send(
        registry,
        Operation::PaneSpawn {
            workspace_id: Some(workspace_id.clone()),
            window_id: Some(two.id.clone()),
            expected_revision: Some(two.revision),
            spec: spec(&fixture.home("second-split")),
        },
    );
    let pane = split["pane_id"].as_str().unwrap();
    let r = registry.snapshot().unwrap();
    assert_eq!(r.workspaces[0].windows[0].name, "renamed");
    assert_eq!(
        r.panes
            .iter()
            .find(|p| p.id == pane)
            .unwrap()
            .window_id
            .as_deref(),
        Some(two.id.as_str())
    );
    for (index, (workspace_id, window_id, revision)) in [
        (Some(workspace_id.clone()), None, Some(two.revision)),
        (None, Some(two.id.clone()), Some(two.revision)),
        (
            Some(workspace_id),
            Some(Uuid::new_v4().to_string()),
            Some(two.revision),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let home = fixture.home(&format!("rejected-membership-{index}"));
        assert!(
            registry
                .request(Request::new(Operation::PaneSpawn {
                    workspace_id,
                    window_id,
                    expected_revision: revision,
                    spec: spec(&home)
                }))
                .is_err()
        );
        assert!(!home.exists());
    }
}

#[test]
fn review_regression_window_close_drains_sibling_normal_completion() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let first = initial(registry, &fixture.home("closing"));
    let workspace_id = first.workspace_id.unwrap();
    let first_pane = first.pane_id.unwrap();
    let home = fixture.home("sibling");
    let mut launch = spec(&home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "while [ ! -e finish ]; do sleep 0.01; done; exit 7".into(),
    ];
    let second: RegistryResult = serde_json::from_value(send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace_id.clone(),
            expected_revision: first.registry.workspaces[0].revision,
            name: None,
            spec: launch,
        },
    ))
    .unwrap();
    let sibling_id = second.pane_id.unwrap();
    let window = &second.registry.workspaces[0].windows[0];
    let closing = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&first_pane)
        .unwrap()
        .clone();
    // Hold this pane's request admission, not publication: background metadata
    // may read publication while reserving registry state. The close still
    // reserves the workspace while its sibling can publish a real completion.
    let held = closing.shared.requests.lock().unwrap();
    let other = registry.clone();
    let id = workspace_id.clone();
    let window_id = window.id.clone();
    let revision = window.revision;
    let worker = std::thread::spawn(move || {
        other.request(Request::new(Operation::WindowClose {
            workspace_id: id,
            window_id,
            expected_revision: revision,
        }))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !registry
        .inner
        .state
        .lock()
        .unwrap()
        .closing_workspaces
        .contains(&workspace_id)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(home.join("finish"), b"").unwrap();
    let deferred = loop {
        let state = registry.inner.state.lock().unwrap();
        let observed = state.record.registry.panes.iter().any(|p| {
            p.id == sibling_id && p.status == PaneStatus::Exited && p.workspace_id.is_some()
        });
        drop(state);
        if observed || Instant::now() >= deadline {
            break observed;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(held);
    let result = worker.join().unwrap().unwrap();
    assert!(
        deferred,
        "did not exercise completion during the close reservation"
    );
    assert!(registry.session_end_requested());
    assert!(registry.snapshot().unwrap().workspaces.is_empty());
    assert!(
        result["registry"]["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
