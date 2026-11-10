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
    let accepted = worker.join().unwrap().unwrap().into_json().unwrap();
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
fn excessive_requested_layout_is_rejected_before_child_setup_or_exec() {
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
    let requested = split_new_pane(Some(&layout));
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
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: sentinel_spec(&home),
            }],
            layout: Some(requested),
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
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: sentinel_spec(&home),
            }],
            layout: Some(SpawnLayoutNode::New {
                reference: "pane".into(),
            }),
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
                panes: vec![PaneSpawnEntry {
                    reference: "pane".into(),
                    spec: launch,
                }],
                layout: Some(SpawnLayoutNode::New {
                    reference: "pane".into(),
                }),
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
        let created = worker.join().unwrap().unwrap().into_json().unwrap();
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

fn new_leaf(reference: &str) -> SpawnLayoutNode {
    SpawnLayoutNode::New {
        reference: reference.into(),
    }
}
fn spawn_split(
    axis: Axis,
    ratio: u16,
    first: SpawnLayoutNode,
    second: SpawnLayoutNode,
) -> SpawnLayoutNode {
    SpawnLayoutNode::Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    }
}
fn batch_entry(reference: &str, home: &Path) -> PaneSpawnEntry {
    PaneSpawnEntry {
        reference: reference.into(),
        spec: sentinel_spec(home),
    }
}
fn batch_operation(
    registry: &Registry,
    workspace: &str,
    panes: Vec<PaneSpawnEntry>,
    layout: SpawnLayoutNode,
) -> Operation {
    let snapshot = registry.snapshot().unwrap();
    let owned = snapshot
        .workspaces
        .iter()
        .find(|owned| owned.id == workspace)
        .unwrap();
    let window = &owned.windows[0];
    Operation::PaneSpawn {
        workspace_id: Some(workspace.into()),
        window_id: Some(window.id.clone()),
        expected_revision: Some(window.revision),
        panes,
        layout: Some(layout),
    }
}
fn batch_fixture(fixture: &Fixture) -> (String, String) {
    // Keep transaction revisions and persistence barriers independent of background
    // project/CWD observation. Native process execution and completion remain real.
    fixture.registry.stop_projects();
    fixture
        .registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let workspace = create(&fixture.registry, "batch target");
    let existing = spawn(
        &fixture.registry,
        &fixture.home("existing"),
        Some(workspace.clone()),
        Some(1),
    );
    (workspace, existing)
}
fn assert_rolled_back(pane: &Pane) {
    assert!(
        pane.shared
            .finished
            .load(std::sync::atomic::Ordering::Acquire),
        "rollback returned before child cleanup"
    );
    #[cfg(target_os = "linux")]
    if let Some(root) = pane.shared.root.lock().unwrap().as_ref() {
        assert!(
            crate::harness_process::process(root.pid).unwrap().is_none(),
            "rollback left its root process alive"
        );
    }
}

#[test]
fn batch_publishes_exact_three_pane_layout_with_one_revision_per_document() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, existing) = batch_fixture(&fixture);
    let before = registry.snapshot().unwrap();
    let codex_home = fixture.home("codex-command");
    let claude_home = fixture.home("claude-command");
    let result: RegistryResult = serde_json::from_value(send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![
                batch_entry("codex", &codex_home),
                batch_entry("claude", &claude_home),
            ],
            spawn_split(
                Axis::Horizontal,
                600,
                SpawnLayoutNode::Existing {
                    pane_id: existing.clone(),
                },
                spawn_split(Axis::Vertical, 350, new_leaf("codex"), new_leaf("claude")),
            ),
        ),
    ))
    .unwrap();
    assert!(result.pane_id.is_none());
    assert_eq!(result.created_panes.len(), 2);
    let codex = result.created_panes["codex"].clone();
    let claude = result.created_panes["claude"].clone();
    assert_ne!(codex, claude);
    let after = registry.snapshot().unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(
        after.workspaces[0].revision,
        before.workspaces[0].revision + 1
    );
    assert_eq!(
        after.workspaces[0].windows[0].revision,
        before.workspaces[0].windows[0].revision + 1
    );
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        Some(LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 600,
            first: Box::new(LayoutNode::Leaf { pane_id: existing }),
            second: Box::new(LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 350,
                first: Box::new(LayoutNode::Leaf {
                    pane_id: codex.clone()
                }),
                second: Box::new(LayoutNode::Leaf {
                    pane_id: claude.clone()
                }),
            }),
        })
    );
    for id in [codex, claude] {
        let info = after.panes.iter().find(|info| info.id == id).unwrap();
        assert_eq!(info.workspace_id.as_deref(), Some(workspace.as_str()));
        let _: PaneSnapshot = serde_json::from_value(send(
            registry,
            Operation::Pane {
                pane_id: id,
                request: PaneRequest::Inspect,
            },
        ))
        .unwrap();
    }
    wait_for_sentinel(&codex_home);
    wait_for_sentinel(&claude_home);
}

#[test]
fn single_spawn_uses_requested_side_axis_ratio_and_can_hide_existing_panes() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, existing) = batch_fixture(&fixture);
    let result: RegistryResult = serde_json::from_value(send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![batch_entry("above", &fixture.home("above"))],
            spawn_split(
                Axis::Vertical,
                275,
                new_leaf("above"),
                SpawnLayoutNode::Existing {
                    pane_id: existing.clone(),
                },
            ),
        ),
    ))
    .unwrap();
    let above = result.created_panes["above"].clone();
    assert_eq!(result.pane_id.as_deref(), Some(above.as_str()));
    assert_eq!(
        result.registry.workspaces[0].windows[0].layout,
        Some(LayoutNode::Split {
            axis: Axis::Vertical,
            ratio: 275,
            first: Box::new(LayoutNode::Leaf { pane_id: above }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: existing.clone()
            }),
        })
    );
    let hidden = send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![batch_entry("only", &fixture.home("only-visible"))],
            new_leaf("only"),
        ),
    );
    assert_eq!(
        hidden["registry"]["workspaces"][0]["windows"][0]["layout"]["pane_id"],
        hidden["created_panes"]["only"]
    );
    let old = registry
        .snapshot()
        .unwrap()
        .panes
        .into_iter()
        .find(|pane| pane.id == existing)
        .unwrap();
    assert_eq!(old.status, PaneStatus::Running);
    assert_eq!(old.workspace_id.as_deref(), Some(workspace.as_str()));
    // A hidden owned pane remains legal in the next explicit final layout.
    send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![batch_entry("next", &fixture.home("next"))],
            spawn_split(
                Axis::Horizontal,
                700,
                SpawnLayoutNode::Existing { pane_id: existing },
                new_leaf("next"),
            ),
        ),
    );
}

#[test]
fn invalid_batch_ownership_references_and_layouts_never_reach_child_setup() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, existing) = batch_fixture(&fixture);
    let foreign_workspace = create(registry, "foreign");
    let foreign = spawn(
        registry,
        &fixture.home("foreign"),
        Some(foreign_workspace.clone()),
        Some(1),
    );
    let first_home = fixture.home("valid-first-must-not-start");
    let second_home = fixture.home("valid-second-must-not-start");
    let entries = vec![
        batch_entry("one", &first_home),
        batch_entry("two", &second_home),
    ];
    let valid = spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two"));
    let malformed_layouts = [
        new_leaf("one"),
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("unknown")),
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("one")),
        spawn_split(Axis::Horizontal, 0, new_leaf("one"), new_leaf("two")),
        spawn_split(Axis::Vertical, 1000, new_leaf("one"), new_leaf("two")),
        spawn_split(
            Axis::Horizontal,
            500,
            valid.clone(),
            SpawnLayoutNode::Existing { pane_id: foreign },
        ),
        spawn_split(
            Axis::Horizontal,
            500,
            valid.clone(),
            SpawnLayoutNode::Existing {
                pane_id: Uuid::new_v4().to_string(),
            },
        ),
        spawn_split(
            Axis::Horizontal,
            500,
            valid.clone(),
            spawn_split(
                Axis::Vertical,
                500,
                SpawnLayoutNode::Existing {
                    pane_id: existing.clone(),
                },
                SpawnLayoutNode::Existing { pane_id: existing },
            ),
        ),
    ];
    for layout in malformed_layouts {
        assert!(
            registry
                .request(Request::new(batch_operation(
                    registry,
                    &workspace,
                    entries.clone(),
                    layout
                )))
                .is_err()
        );
    }
    for bad_reference in [String::new(), "one".into(), "x".repeat(257)] {
        let mut invalid = entries.clone();
        invalid[1].reference = bad_reference;
        assert!(
            registry
                .request(Request::new(batch_operation(
                    registry,
                    &workspace,
                    invalid,
                    valid.clone()
                )))
                .is_err()
        );
    }
    for case in 0..6 {
        let mut operation = batch_operation(registry, &workspace, entries.clone(), valid.clone());
        if let Operation::PaneSpawn {
            workspace_id,
            window_id,
            expected_revision,
            panes,
            layout,
        } = &mut operation
        {
            match case {
                0 => *expected_revision = Some(0),
                1 => *workspace_id = Some(foreign_workspace.clone()),
                2 => *window_id = None,
                3 => *layout = None,
                4 => panes.clear(),
                5 => {
                    *workspace_id = None;
                    *window_id = None;
                    *expected_revision = None;
                }
                _ => unreachable!(),
            }
        }
        assert!(registry.request(Request::new(operation)).is_err());
    }
    assert!(!first_home.exists() && !second_home.exists());
    assert_eq!(registry.snapshot().unwrap().panes.len(), 2);
}

#[test]
fn every_spec_is_validated_before_any_batch_home_or_pty_setup() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let first_home = fixture.home("first-preflight");
    let second_home = fixture.home("second-preflight");
    let mut invalid = Vec::new();
    let normal = spec(&second_home);
    let mut launch = normal.clone();
    launch.argv.clear();
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.argv = vec![String::new()];
    launch.title = Some("empty program".into());
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.argv.push("bad\0argument".into());
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.argv = vec!["x".repeat(128 * 1024 + 1)];
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.argv = vec!["x".into(); 257];
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.title = Some("invalid\nname".into());
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.size.cols = 0;
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.cwd = PathBuf::from("relative");
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.home = PathBuf::from("relative");
    invalid.push(launch);
    let mut launch = normal.clone();
    launch.cwd = PathBuf::from(std::ffi::OsString::from_vec(b"/invalid\0cwd".to_vec()));
    invalid.push(launch);
    let mut launch = normal;
    launch.home = PathBuf::from(std::ffi::OsString::from_vec(b"/invalid\0home".to_vec()));
    invalid.push(launch);
    for spec in invalid {
        let operation = batch_operation(
            registry,
            &workspace,
            vec![
                batch_entry("one", &first_home),
                PaneSpawnEntry {
                    reference: "two".into(),
                    spec,
                },
            ],
            spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
        );
        assert!(registry.request(Request::new(operation)).is_err());
        assert!(!first_home.exists() && !second_home.exists());
    }
}

#[test]
fn later_filesystem_prerequisite_failure_never_executes_an_earlier_command() {
    for case in 0..6 {
        let fixture = Fixture::new();
        let registry = &fixture.registry;
        let (workspace, _) = batch_fixture(&fixture);
        let before = registry.snapshot().unwrap();
        let first_home = fixture.home("must-not-execute");
        let later_home = fixture.home("invalid-later-home");
        let mut later = batch_entry("later", &later_home);
        match case {
            0 => {
                fs::write(&later_home, b"not a directory").unwrap();
                later.spec.isolated_environment = false;
                later.spec.cwd = fixture._root.path().into();
            }
            1 => {
                let cwd = fixture._root.path().join("cwd-file");
                fs::write(&cwd, b"not a directory").unwrap();
                later.spec.cwd = cwd;
            }
            2 => later.spec.cwd = fixture._root.path().join("missing-cwd"),
            3 => {
                fs::create_dir(&later_home).unwrap();
                fs::write(later_home.join("user-file"), b"must not adopt").unwrap();
            }
            4 => {
                fs::write(&later_home, b"not a directory").unwrap();
            }
            5 => {
                fs::create_dir(&later_home).unwrap();
                fs::write(later_home.join(".bkpr-native-home"), b"fixture").unwrap();
                fs::write(later_home.join("config"), b"not a directory").unwrap();
            }
            _ => unreachable!(),
        }
        let sentinel_home = first_home.clone();
        // If an earlier child starts, force its real side effect to become
        // observable before startup proceeds to the invalid later entry.
        *registry.inner.spawn_started.lock().unwrap() =
            Some(Arc::new(move |_| wait_for_sentinel(&sentinel_home)));
        let operation = batch_operation(
            registry,
            &workspace,
            vec![batch_entry("first", &first_home), later],
            spawn_split(Axis::Horizontal, 500, new_leaf("first"), new_leaf("later")),
        );
        assert!(registry.request(Request::new(operation)).is_err());
        assert!(!first_home.join("executed").exists(), "case {case}");
        let after = registry.snapshot().unwrap();
        assert_eq!(after.panes.len(), before.panes.len(), "case {case}");
        assert_eq!(
            after.workspaces[0].windows[0].layout, before.workspaces[0].windows[0].layout,
            "case {case}"
        );
    }
}

#[test]
fn second_exec_failure_reaps_first_child_and_publishes_nothing() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let before = registry.snapshot().unwrap();
    let (started, started_rx) = std::sync::mpsc::sync_channel(2);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        started.send(pane.clone()).unwrap();
    }));
    let mut second = batch_entry("two", &fixture.home("failed-second"));
    second.spec.argv = vec!["/bkpr-test-no-such-executable".into()];
    let operation = batch_operation(
        registry,
        &workspace,
        vec![batch_entry("one", &fixture.home("first-started")), second],
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
    );
    assert!(registry.request(Request::new(operation)).is_err());
    let first = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_rolled_back(&first);
    assert!(started_rx.try_recv().is_err());
    let after = registry.snapshot().unwrap();
    assert_eq!(after.panes.len(), before.panes.len());
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    assert_eq!(
        after.workspaces[0].windows[0].revision,
        before.workspaces[0].windows[0].revision
    );
    assert_eq!(registry.inner.state.lock().unwrap().spawning, 0);
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: workspace,
            expected_revision: before.workspaces[0].revision,
            name: "reservation released".into(),
        },
    );
}

#[test]
fn batch_reservation_rejects_same_workspace_layout_and_close_but_merges_unrelated_commits() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let unrelated = create(registry, "responsive");
    let before = registry.snapshot().unwrap();
    let target = before
        .workspaces
        .iter()
        .find(|owned| owned.id == workspace)
        .unwrap();
    let first_home = fixture.home("reserved-one");
    let second_home = fixture.home("reserved-two");
    let operation = batch_operation(
        registry,
        &workspace,
        vec![
            batch_entry("one", &first_home),
            batch_entry("two", &second_home),
        ],
        spawn_split(Axis::Vertical, 425, new_leaf("one"), new_leaf("two")),
    );
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeSpawn(Some(resume));
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || worker_registry.request(Request::new(operation)));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    for operation in [
        Operation::LayoutUpdate {
            workspace_id: workspace.clone(),
            window_id: target.windows[0].id.clone(),
            expected_revision: target.windows[0].revision,
            layout: None,
        },
        Operation::WindowClose {
            workspace_id: workspace.clone(),
            window_id: target.windows[0].id.clone(),
            expected_revision: target.windows[0].revision,
        },
        Operation::WorkspaceClose {
            workspace_id: workspace.clone(),
            expected_revision: target.revision,
        },
    ] {
        assert!(
            registry
                .request(Request::new(operation))
                .unwrap_err()
                .to_string()
                .contains("workspace busy")
        );
    }
    assert!(!first_home.exists() && !second_home.exists());
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: unrelated.clone(),
            expected_revision: 1,
            name: "merged during startup".into(),
        },
    );
    let other = send(
        registry,
        Operation::WindowCreate {
            workspace_id: unrelated.clone(),
            expected_revision: 2,
            name: None,
            spec: spec(&fixture.home("unrelated-start")),
        },
    );
    let _: PaneSnapshot = serde_json::from_value(send(
        registry,
        Operation::Pane {
            pane_id: other["pane_id"].as_str().unwrap().into(),
            request: PaneRequest::Inspect,
        },
    ))
    .unwrap();
    drop(release);
    let result: RegistryResult =
        serde_json::from_value(worker.join().unwrap().unwrap().into_json().unwrap()).unwrap();
    assert_eq!(result.created_panes.len(), 2);
    let after = registry.snapshot().unwrap();
    let unrelated = after
        .workspaces
        .iter()
        .find(|owned| owned.id == unrelated)
        .unwrap();
    assert_eq!(unrelated.name, "merged during startup");
    assert_eq!(unrelated.windows.len(), 1);
}

#[test]
fn batch_persist_failure_keeps_initial_layout_readable_then_reaps_all_and_poisons_writes() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, existing) = batch_fixture(&fixture);
    let before = registry.snapshot().unwrap();
    let operation = batch_operation(
        registry,
        &workspace,
        vec![
            batch_entry("one", &fixture.home("persist-one")),
            batch_entry("two", &fixture.home("persist-two")),
        ],
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
    );
    let (started, started_rx) = std::sync::mpsc::sync_channel(2);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        started.send(pane.clone()).unwrap();
    }));
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeSpawn(Some(resume));
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || worker_registry.request(Request::new(operation)));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let first = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let second = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let during = registry.snapshot().unwrap();
    assert_eq!(during.panes.len(), before.panes.len());
    assert_eq!(
        during.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    let _: PaneSnapshot = serde_json::from_value(send(
        registry,
        Operation::Pane {
            pane_id: existing,
            request: PaneRequest::Inspect,
        },
    ))
    .unwrap();
    let record_path = fixture._root.path().join("registry.json");
    fs::remove_file(&record_path).unwrap();
    fs::create_dir(&record_path).unwrap();
    drop(release);
    assert!(worker.join().unwrap().is_err());
    assert_rolled_back(&first);
    assert_rolled_back(&second);
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.record.registry.panes.len(), before.panes.len());
    assert_eq!(
        state.record.registry.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    assert!(state.durability_error.is_some());
    assert_eq!(state.spawning, 0);
    drop(state);
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceRename {
                workspace_id: workspace,
                expected_revision: before.workspaces[0].revision,
                name: "must remain poisoned".into()
            }))
            .unwrap_err()
            .to_string()
            .contains("restart required")
    );
}

#[test]
fn early_exit_before_batch_publication_follows_normal_completion_without_losing_other_children() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let mut exiting = batch_entry("exit", &fixture.home("early-exit"));
    exiting.spec.argv = vec!["/bin/sh".into(), "-c".into(), "exit 7".into()];
    let first = std::sync::atomic::AtomicBool::new(true);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            // Wait for the real worker to finish before registry callbacks exist.
            let deadline = Instant::now() + Duration::from_secs(3);
            while !pane
                .shared
                .finished
                .load(std::sync::atomic::Ordering::Acquire)
            {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }));
    let result: RegistryResult = serde_json::from_value(send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![exiting, batch_entry("live", &fixture.home("survivor"))],
            spawn_split(Axis::Vertical, 500, new_leaf("exit"), new_leaf("live")),
        ),
    ))
    .unwrap();
    let after = registry.snapshot().unwrap();
    let exit = after
        .panes
        .iter()
        .find(|pane| pane.id == result.created_panes["exit"])
        .unwrap();
    assert_eq!(exit.status, PaneStatus::Exited);
    assert_eq!(exit.exit_code, Some(7));
    assert!(exit.workspace_id.is_none());
    let live = after
        .panes
        .iter()
        .find(|pane| pane.id == result.created_panes["live"])
        .unwrap();
    assert_eq!(live.status, PaneStatus::Running);
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: live.id.clone()
        })
    );
}

#[test]
fn new_reference_equal_to_existing_id_keeps_both_namespaces_distinct() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, existing) = batch_fixture(&fixture);
    let result: RegistryResult = serde_json::from_value(send(
        registry,
        batch_operation(
            registry,
            &workspace,
            vec![batch_entry(
                &existing,
                &fixture.home("separate-reference-namespace"),
            )],
            spawn_split(
                Axis::Horizontal,
                450,
                SpawnLayoutNode::Existing {
                    pane_id: existing.clone(),
                },
                new_leaf(&existing),
            ),
        ),
    ))
    .unwrap();
    let created = result.created_panes[&existing].clone();
    assert_ne!(created, existing);
    assert_eq!(
        result.registry.workspaces[0].windows[0].layout,
        Some(LayoutNode::Split {
            axis: Axis::Horizontal,
            ratio: 450,
            first: Box::new(LayoutNode::Leaf { pane_id: existing }),
            second: Box::new(LayoutNode::Leaf { pane_id: created }),
        })
    );
}

#[test]
fn failed_restore_commit_never_revokes_intent_owned_by_old_finished_handle() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, id) = batch_fixture(&fixture);
    registry.inner.state.lock().unwrap().restore_batch = true;
    let old_handle = registry.inner.state.lock().unwrap().live[&id].clone();
    let callback = old_handle.shared.on_finish.lock().unwrap().take().unwrap();
    let (finished, finished_rx) = std::sync::mpsc::sync_channel(1);
    *old_handle.shared.on_finish.lock().unwrap() = Some(Arc::new(move || {
        callback();
        let _ = finished.send(());
    }));
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: id.clone(),
        },
    );
    finished_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let expected = HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: "restore-rollback-owned-native".into(),
        cwd: fixture._root.path().into(),
    };
    let window_id = {
        let _transaction = registry.inner.transaction.lock().unwrap();
        let mut next = registry.inner.state.lock().unwrap().record.clone();
        next.recoveries.insert(id.clone(), expected.clone());
        next.automatic.insert(id.clone(), expected.clone());
        next.registry.revision = next_revision(next.registry.revision).unwrap();
        validate_record(&next).unwrap();
        let window = next.registry.workspaces[0].windows[0].id.clone();
        commit(&registry.inner, next, |_| {}).unwrap();
        window
    };
    registry.inner.state.lock().unwrap().restore_batch = false;
    let path = fixture._root.path().join("registry.json");
    let (started, started_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        started.send(pane.clone()).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
    }));
    registry
        .spawn_inner(
            SpawnTarget::Restore {
                workspace_id: workspace,
                window_id,
                pane_id: id.clone(),
            },
            spec(&fixture.home("failed-restore")),
            None,
            None,
        )
        .unwrap_err();
    let unpublished = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_rolled_back(&unpublished);
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.record.recoveries.get(&id), Some(&expected));
    assert_eq!(state.record.automatic.get(&id), Some(&expected));
    assert!(Arc::ptr_eq(&state.live[&id].shared, &old_handle.shared));
    assert_eq!(state.spawning, 0);
}

#[test]
fn commit_revalidates_window_revision_after_start_and_rolls_back_entire_batch() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let operation = batch_operation(
        registry,
        &workspace,
        vec![
            batch_entry("one", &fixture.home("recheck-one")),
            batch_entry("two", &fixture.home("recheck-two")),
        ],
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
    );
    let weak = Arc::downgrade(&registry.inner);
    let (started, started_rx) = std::sync::mpsc::sync_channel(2);
    let first = std::sync::atomic::AtomicBool::new(true);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        started.send(pane.clone()).unwrap();
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            // Impossible through normal workspace mutations (they return Busy),
            // but verifies the independent final CAS guard against record drift.
            weak.upgrade()
                .unwrap()
                .state
                .lock()
                .unwrap()
                .record
                .registry
                .workspaces[0]
                .windows[0]
                .revision += 1;
        }
    }));
    assert!(
        registry
            .request(Request::new(operation))
            .unwrap_err()
            .to_string()
            .contains("stale window revision")
    );
    assert_rolled_back(&started_rx.recv_timeout(Duration::from_secs(3)).unwrap());
    assert_rolled_back(&started_rx.recv_timeout(Duration::from_secs(3)).unwrap());
    assert_eq!(registry.snapshot().unwrap().panes.len(), 1);
    assert_eq!(registry.inner.state.lock().unwrap().spawning, 0);
}

#[test]
fn shutdown_during_batch_start_reaps_every_unpublished_child_before_returning() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let operation = batch_operation(
        registry,
        &workspace,
        vec![
            batch_entry("one", &fixture.home("shutdown-one")),
            batch_entry("two", &fixture.home("shutdown-two")),
        ],
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
    );
    let (started, started_rx) = std::sync::mpsc::sync_channel(2);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume_rx = Mutex::new(resume_rx);
    let release = ResumeSpawn(Some(resume));
    let first = std::sync::atomic::AtomicBool::new(true);
    *registry.inner.spawn_started.lock().unwrap() = Some(Arc::new(move |pane| {
        started.send(pane.clone()).unwrap();
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            resume_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    }));
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || worker_registry.request(Request::new(operation)));
    let first = started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let shutdown_registry = registry.clone();
    let shutdown = std::thread::spawn(move || shutdown_registry.shutdown());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !registry.inner.state.lock().unwrap().closing {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(release);
    assert!(worker.join().unwrap().is_err());
    shutdown.join().unwrap().unwrap();
    assert_rolled_back(&first);
    assert_rolled_back(&started_rx.recv_timeout(Duration::from_secs(3)).unwrap());
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.spawning, 0);
    assert_eq!(state.record.registry.panes.len(), 1);
}

#[test]
fn unowned_admin_batch_returns_every_mapping_without_creating_workspace_or_layout() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let result: RegistryResult = serde_json::from_value(send(
        registry,
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            panes: vec![
                batch_entry("one", &fixture.home("admin-one")),
                batch_entry("two", &fixture.home("admin-two")),
            ],
            layout: None,
        },
    ))
    .unwrap();
    assert_eq!(result.created_panes.len(), 2);
    assert!(result.pane_id.is_none());
    assert!(result.registry.workspaces.is_empty());
    assert_eq!(result.registry.panes.len(), 2);
    for pane in &result.registry.panes {
        assert!(pane.workspace_id.is_none() && pane.window_id.is_none());
        assert!(result.created_panes.values().any(|id| id == &pane.id));
    }
}

#[test]
fn new_windows_use_existing_names_without_a_document_count_ceiling() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "many windows");
    registry
        .mutate(|snapshot| {
            let workspace = snapshot
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .unwrap();
            for index in 0..384 {
                workspace.windows.push(Window {
                    id: Uuid::new_v4().to_string(),
                    name: if index == 0 {
                        "Window".into()
                    } else {
                        format!("Window {}", index + 1)
                    },
                    revision: 1,
                    layout: None,
                });
            }
            Ok(())
        })
        .unwrap();
    send(
        registry,
        Operation::WindowCreate {
            workspace_id,
            expected_revision: 1,
            name: None,
            spec: spec(&fixture.home("next-window")),
        },
    );
    let snapshot = registry.snapshot().unwrap();
    let workspace = &snapshot.workspaces[0];
    assert_eq!(workspace.windows.len(), 385);
    assert_eq!(workspace.windows.last().unwrap().name, "Window 385");
    assert_eq!(snapshot.panes.len(), 1);
}

#[test]
fn oversized_durable_candidate_is_rejected_before_exec_without_poisoning_registry() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let (workspace, _) = batch_fixture(&fixture);
    let first_home = fixture.home("oversized-first");
    let second_home = fixture.home("oversized-second");
    let mut second = batch_entry("two", &second_home);
    second.spec.cwd = PathBuf::from(format!("/{}", "x".repeat(MAX_RECORD as usize)));
    let operation = batch_operation(
        registry,
        &workspace,
        vec![batch_entry("one", &first_home), second],
        spawn_split(Axis::Horizontal, 500, new_leaf("one"), new_leaf("two")),
    );
    assert!(
        registry
            .request(Request::new(operation))
            .unwrap_err()
            .to_string()
            .contains("registry file limit")
    );
    assert!(!first_home.exists() && !second_home.exists());
    assert_eq!(registry.snapshot().unwrap().panes.len(), 1);
    assert!(
        registry
            .inner
            .state
            .lock()
            .unwrap()
            .durability_error
            .is_none()
    );
}
