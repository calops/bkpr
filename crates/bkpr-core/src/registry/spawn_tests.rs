use super::*;

pub(super) struct ResumeSpawn(pub(super) Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ResumeSpawn {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}
pub(super) fn sentinel_spec(home: &Path) -> PaneSpawnSpec {
    let mut launch = spec(home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf executed > executed; exec sleep 60".into(),
    ];
    launch
}
pub(super) fn wait_for_sentinel(home: &Path) {
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
        worker_registry.request(Request::new(Operation::WindowCreate {
            workspace_id: worker_workspace,
            expected_revision: 1,
            name: None,
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
        Operation::WorkspaceClose {
            workspace_id: a.clone(),
            expected_revision: 1,
        },
        Operation::WindowCreate {
            workspace_id: a.clone(),
            expected_revision: 1,
            name: None,
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
        Operation::WindowCreate {
            workspace_id: b,
            expected_revision: 2,
            name: None,
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
        .request(Request::new(Operation::WindowCreate {
            workspace_id: a.clone(),
            expected_revision: 1,
            name: None,
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
            .request(Request::new(Operation::WindowCreate {
                workspace_id: id.clone(),
                expected_revision: 1,
                name: None,
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
    let window_id = Uuid::new_v4().to_string();
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
                    window_id: Some(window_id.clone()),
                    title: "stopped".into(),
                    status: PaneStatus::Stopped,
                    exit_code: None,
                    cwd: fixture._root.path().into(),
                    home: fixture._root.path().into(),
                    foreground_cwd: None,
                    harness: None,
                    harness_status: None,
                    project: None,
                    isolated_environment: false,
                });
            }
            r.workspaces[0].windows.push(Window {
                id: window_id.clone(),
                name: "Saved".into(),
                revision: 1,
                layout: None,
            });
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
            window_id: window_id.clone(),
            expected_revision: 1,
            layout: Some(layout),
        },
    );
    let home = fixture.home("must-not-execute");
    let error = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(workspace_id.clone()),
            window_id: Some(window_id),
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

#[test]
fn close_reserves_owned_documents_without_locking_unrelated_controls_or_starting_argv() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "closing");
    let b = create(registry, "unrelated");
    let pane_id = spawn(registry, &fixture.home("pane"), Some(a.clone()), Some(1));
    let r = registry.snapshot().unwrap();
    let w = r.workspaces.iter().find(|w| w.id == a).unwrap();
    let window_id = w.windows[0].id.clone();
    let pane = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let blocked = pane.shared.requests.lock().unwrap();
    let worker_registry = registry.clone();
    let target = a.clone();
    let worker = std::thread::spawn(move || {
        worker_registry.request(Request::new(Operation::WorkspaceClose {
            workspace_id: target,
            expected_revision: 2,
        }))
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !registry
        .inner
        .state
        .lock()
        .unwrap()
        .closing_workspaces
        .contains(&a)
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let home = fixture.home("must-not-run");
    let error = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(a.clone()),
            window_id: Some(window_id.clone()),
            expected_revision: Some(2),
            spec: sentinel_spec(&home),
        }))
        .unwrap_err();
    assert!(error.to_string().contains("workspace busy"));
    assert!(!home.exists());
    assert!(
        registry
            .request(Request::new(Operation::LayoutUpdate {
                workspace_id: a,
                window_id,
                expected_revision: 2,
                layout: None
            }))
            .is_err()
    );
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: b,
            expected_revision: 1,
            name: "not blocked".into(),
        },
    );
    drop(blocked);
    worker.join().unwrap().unwrap();
}

#[test]
fn atomic_initial_creation_rolls_back_failed_exec_and_chooses_names_under_commit() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let mut launch = spec(&fixture.home("failed-initial"));
    launch.argv = vec!["/does-not-exist-bkpr".into()];
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: Some(launch)
            }))
            .is_err()
    );
    assert!(registry.snapshot().unwrap().workspaces.is_empty());
    assert!(registry.snapshot().unwrap().panes.is_empty());
    let one = send(
        registry,
        Operation::WorkspaceCreate {
            name: Some("  ".into()),
            initial: Some(spec(&fixture.home("initial-one"))),
        },
    );
    let two = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec(&fixture.home("initial-two"))),
        },
    );
    let r = registry.snapshot().unwrap();
    assert_ne!(r.workspaces[0].name, r.workspaces[1].name);
    for value in [one, two] {
        let pane = r
            .panes
            .iter()
            .find(|p| p.id == value["pane_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(pane.workspace_id.as_deref(), value["workspace_id"].as_str());
        assert_eq!(pane.window_id.as_deref(), value["window_id"].as_str());
        assert_eq!(pane.cwd, pane.home);
    }
}

#[test]
fn last_execution_completion_during_admitted_spawn_converges_after_reservation_release() {
    for new_workspace in [false, true] {
        let fixture = Fixture::new();
        let registry = &fixture.registry;
        let workspace_id = create(registry, "existing");
        let old = spawn(
            registry,
            &fixture.home("old"),
            Some(workspace_id.clone()),
            Some(1),
        );
        let r = registry.snapshot().unwrap();
        let window_id = r.workspaces[0].windows[0].id.clone();
        let (entered, rx) = std::sync::mpsc::sync_channel(1);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
        *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
            entered,
            resume: Mutex::new(resume_rx),
        }));
        let release = ResumeSpawn(Some(resume));
        let worker_registry = registry.clone();
        let launch = spec(&fixture.home("new"));
        let operation = if new_workspace {
            Operation::WorkspaceCreate {
                name: None,
                initial: Some(launch),
            }
        } else {
            Operation::PaneSpawn {
                workspace_id: Some(workspace_id.clone()),
                window_id: Some(window_id),
                expected_revision: Some(2),
                spec: launch,
            }
        };
        let worker = std::thread::spawn(move || worker_registry.request(Request::new(operation)));
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        send(
            registry,
            Operation::PaneTerminate {
                pane_id: old.clone(),
            },
        );
        assert!(!registry.session_end_requested());
        assert_eq!(registry.snapshot().unwrap().workspaces.len(), 1);
        drop(release);
        let created = worker.join().unwrap().unwrap();
        let r = registry.snapshot().unwrap();
        assert_eq!(r.workspaces.len(), 1);
        assert!(!registry.session_end_requested());
        assert!(
            r.panes
                .iter()
                .find(|p| p.id == old)
                .unwrap()
                .workspace_id
                .is_none()
        );
        let new = r
            .panes
            .iter()
            .find(|p| p.id == created["pane_id"].as_str().unwrap())
            .unwrap();
        assert!(new.workspace_id.is_some() && new.window_id.is_some());
        assert_eq!(
            r.workspaces[0].windows[0].layout,
            Some(LayoutNode::Leaf {
                pane_id: new.id.clone()
            })
        );
    }
}
