use super::spawn_tests::{ResumeSpawn, sentinel_spec, wait_for_sentinel};
use super::*;

#[test]
fn reopen_replaces_only_owned_unavailable_position_and_retires_membership_not_receipt() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "saved");
    let (old, window_id) = unavailable(registry, &workspace_id, fixture._root.path());
    let another = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace_id.clone(),
            expected_revision: 2,
            name: Some("Other".into()),
            spec: spec(&fixture.home("other")),
        },
    );
    let other_pane = another["pane_id"].as_str().unwrap();
    let value = send(
        registry,
        Operation::PaneReopen {
            workspace_id: workspace_id.clone(),
            window_id: window_id.clone(),
            expected_revision: 1,
            pane_id: old.clone(),
            spec: spec(&fixture.home("reopened")),
        },
    );
    let new = value["pane_id"].as_str().unwrap();
    let r = registry.snapshot().unwrap();
    let workspace = r.workspaces.iter().find(|w| w.id == workspace_id).unwrap();
    assert_eq!(
        workspace.windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: new.into()
        })
    );
    assert_eq!(snapshot(registry, other_pane)["lifecycle"], "running");
    let old = r.panes.iter().find(|p| p.id == old).unwrap();
    assert!(old.workspace_id.is_none() && old.window_id.is_none());
    assert_eq!(old.exit_code, Some(7));
    assert_eq!(
        r.panes
            .iter()
            .find(|p| p.id == new)
            .unwrap()
            .window_id
            .as_deref(),
        Some(window_id.as_str())
    );
}
#[test]
fn reopen_rejects_foreign_window_live_stale_and_missing_leaf_before_argv() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "a");
    let b = create(registry, "b");
    let (old, window_id) = unavailable(registry, &a, fixture._root.path());
    let (foreign, foreign_window) = unavailable(registry, &b, fixture._root.path());
    for (index, operation) in [
        Operation::PaneReopen {
            workspace_id: a.clone(),
            window_id: foreign_window,
            expected_revision: 1,
            pane_id: foreign,
            spec: sentinel_spec(&fixture.home("foreign")),
        },
        Operation::PaneReopen {
            workspace_id: a.clone(),
            window_id: window_id.clone(),
            expected_revision: 0,
            pane_id: old.clone(),
            spec: sentinel_spec(&fixture.home("stale")),
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            registry.request(Request::new(operation)).is_err(),
            "case {index}"
        );
    }
    assert!(!fixture.home("foreign").exists());
    assert!(!fixture.home("stale").exists());
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: a.clone(),
            window_id: window_id.clone(),
            expected_revision: 1,
            layout: None,
        },
    );
    assert!(
        registry
            .request(Request::new(Operation::PaneReopen {
                workspace_id: a,
                window_id,
                expected_revision: 2,
                pane_id: old,
                spec: sentinel_spec(&fixture.home("unreferenced"))
            }))
            .is_err()
    );
    assert!(!fixture.home("unreferenced").exists());
    let pane = spawn(registry, &fixture.home("live"), Some(b.clone()), Some(1));
    let r = registry.snapshot().unwrap();
    let window = &r.workspaces.iter().find(|w| w.id == b).unwrap().windows[0];
    assert!(
        registry
            .request(Request::new(Operation::PaneReopen {
                workspace_id: b,
                window_id: window.id.clone(),
                expected_revision: window.revision,
                pane_id: pane,
                spec: sentinel_spec(&fixture.home("live-rejected"))
            }))
            .is_err()
    );
}
#[test]
fn reopen_reserves_close_and_preserves_documents_when_child_start_fails() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "saved");
    let (old, window_id) = unavailable(registry, &workspace_id, fixture._root.path());
    let before = serde_json::to_value(registry.snapshot().unwrap()).unwrap();
    let mut invalid = spec(&fixture.home("invalid"));
    invalid.argv = vec!["/does-not-exist-bkpr".into()];
    assert!(
        registry
            .request(Request::new(Operation::PaneReopen {
                workspace_id: workspace_id.clone(),
                window_id: window_id.clone(),
                expected_revision: 1,
                pane_id: old.clone(),
                spec: invalid
            }))
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(registry.snapshot().unwrap()).unwrap(),
        before
    );
    let (entered, rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeSpawn(Some(resume));
    let worker_registry = registry.clone();
    let home = fixture.home("accepted");
    let launch = sentinel_spec(&home);
    let w = workspace_id.clone();
    let worker = std::thread::spawn(move || {
        worker_registry.request(Request::new(Operation::PaneReopen {
            workspace_id: w,
            window_id,
            expected_revision: 1,
            pane_id: old,
            spec: launch,
        }))
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceClose {
                workspace_id,
                expected_revision: 2
            }))
            .unwrap_err()
            .to_string()
            .contains("workspace busy")
    );
    drop(release);
    worker.join().unwrap().unwrap();
    wait_for_sentinel(&home);
}
#[test]
fn bootstrap_restores_saved_unavailable_position_with_current_argv_only_once() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let id = create(&registry, "saved");
    let saved_home = root.path().join("saved-home");
    crate::prepare_home(&saved_home).unwrap();
    let (old, window) = unavailable(&registry, &id, &saved_home);
    drop(registry);
    let registry = Registry::open(root.path()).unwrap();
    let home = root.path().join("current");
    let result = registry.bootstrap(sentinel_spec(&home)).unwrap();
    assert_eq!(result["failures"], json!([]));
    wait_for_sentinel(&saved_home);
    assert!(
        !home.exists(),
        "caller HOME must not replace saved private HOME"
    );
    let r = registry.snapshot().unwrap();
    assert_eq!(r.workspaces.len(), 1);
    assert_eq!(r.workspaces[0].windows[0].id, window);
    assert_eq!(
        r.workspaces[0].windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: old.clone()
        })
    );
    assert_eq!(result["restored"][0]["pane_id"], old);
    assert_eq!(
        r.panes.iter().find(|pane| pane.id == old).unwrap().status,
        PaneStatus::Running
    );
    let before = r.revision;
    registry
        .bootstrap(sentinel_spec(&root.path().join("must-not-run")))
        .unwrap();
    assert!(!root.path().join("must-not-run").exists());
    assert_eq!(registry.snapshot().unwrap().revision, before);
    registry.shutdown().unwrap();
}
