use super::*;

struct ResumeSpawn(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ResumeSpawn {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}
fn sentinel_spec(home: &Path) -> PaneSpawnSpec {
    let mut launch = spec(home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf executed > executed; exec sleep 60".into(),
    ];
    launch
}
fn wait_for_sentinel(home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if fs::read(home.join("executed")).ok().as_deref() == Some(b"executed") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "accepted child never wrote its execution sentinel"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn spawn_reserves_workspace_revision_before_exec_without_blocking_other_workspaces() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "reserved");
    let b = create(registry, "independent");
    let accepted_home = fixture.home("accepted");
    let busy_home = fixture.home("rejected-busy");
    let stale_home = fixture.home("rejected-stale");
    let unrelated_home = fixture.home("unrelated");
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeSpawn(Some(resume));
    let worker_registry = registry.clone();
    let worker_workspace = a.clone();
    let launch = sentinel_spec(&accepted_home);
    let worker = std::thread::spawn(move || {
        worker_registry.request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(worker_workspace),
            expected_revision: Some(1),
            spec: launch,
        }))
    });
    // The test-only pause controls scheduling. Everything asserted below uses the
    // consumer API and observable child execution, not reservation implementation state.
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    for operation in [
        Operation::WorkspaceRename {
            workspace_id: a.clone(),
            expected_revision: 1,
            name: "raced".into(),
        },
        Operation::WorkspaceRemove {
            workspace_id: a.clone(),
            expected_revision: 1,
        },
        Operation::LayoutUpdate {
            workspace_id: a.clone(),
            expected_revision: 1,
            layout: None,
        },
        Operation::PaneSpawn {
            workspace_id: Some(a.clone()),
            expected_revision: Some(1),
            spec: sentinel_spec(&busy_home),
        },
    ] {
        let error = registry.request(Request::new(operation)).unwrap_err();
        assert!(
            error.to_string().contains("workspace busy"),
            "unexpected race result: {error:#}"
        );
    }
    assert!(!accepted_home.join("executed").exists());
    assert!(
        !busy_home.exists(),
        "Busy spawn performed pre-exec filesystem side effects"
    );
    let r = registry.snapshot().unwrap();
    assert_eq!(r.workspaces.iter().find(|w| w.id == a).unwrap().revision, 1);
    // Controls and an actual child exec in B must finish while A is paused.
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: b.clone(),
            expected_revision: 1,
            name: "available".into(),
        },
    );
    send(
        registry,
        Operation::PaneSpawn {
            workspace_id: Some(b),
            expected_revision: Some(2),
            spec: sentinel_spec(&unrelated_home),
        },
    );
    wait_for_sentinel(&unrelated_home);
    drop(release);
    let accepted = worker.join().unwrap().unwrap();
    assert!(accepted["pane_id"].as_str().is_some());
    wait_for_sentinel(&accepted_home);
    // A stale request is rejected before argv can run or even create its home.
    let error = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(a.clone()),
            expected_revision: Some(1),
            spec: sentinel_spec(&stale_home),
        }))
        .unwrap_err();
    assert!(error.to_string().contains("stale workspace revision"));
    assert!(!stale_home.exists(), "stale CAS ran child setup or argv");
    assert!(!busy_home.join("executed").exists());
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: a,
            expected_revision: 2,
            name: "reservation released".into(),
        },
    );
}

#[test]
fn failed_child_start_releases_workspace_reservation_without_advancing_revision() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let id = create(registry, "retryable");
    let mut launch = spec(&fixture.home("failed"));
    launch.argv = vec!["/this-executable-does-not-exist-bkpr".into()];
    assert!(
        registry
            .request(Request::new(Operation::PaneSpawn {
                workspace_id: Some(id.clone()),
                expected_revision: Some(1),
                spec: launch,
            }))
            .is_err()
    );
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: id,
            expected_revision: 1,
            name: "not locked".into(),
        },
    );
    assert!(registry.snapshot().unwrap().panes.is_empty());
}

#[test]
fn invalid_post_spawn_layout_is_rejected_before_child_setup_or_exec() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "maximum depth");
    let ids = (0..=MAX_LAYOUT_DEPTH)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    // Fixture models metadata from an earlier daemon; there are no surviving PTYs.
    registry
        .mutate(|r| {
            for id in &ids {
                r.panes.push(PaneInfo {
                    id: id.clone(),
                    workspace_id: Some(workspace_id.clone()),
                    title: "stopped".into(),
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
    let mut layout = LayoutNode::Leaf {
        pane_id: ids[0].clone(),
    };
    for pane_id in &ids[1..] {
        layout = LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 500,
            first: Box::new(layout),
            second: Box::new(LayoutNode::Leaf {
                pane_id: pane_id.clone(),
            }),
        };
    }
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: workspace_id.clone(),
            expected_revision: 1,
            layout: Some(layout),
        },
    );
    let home = fixture.home("must-not-execute");
    let error = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(workspace_id.clone()),
            expected_revision: Some(2),
            spec: sentinel_spec(&home),
        }))
        .unwrap_err();
    assert!(
        error.to_string().contains("layout depth limit"),
        "{error:#}"
    );
    assert!(
        !home.exists(),
        "invalid layout was rejected only after child setup or argv ran"
    );
    assert_eq!(registry.snapshot().unwrap().panes.len(), ids.len());
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id,
            expected_revision: 2,
            name: "still available".into(),
        },
    );
}
