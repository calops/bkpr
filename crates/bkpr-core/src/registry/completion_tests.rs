use super::*;

fn value(data: &impl serde::Serialize) -> Value {
    serde_json::to_value(data).unwrap()
}

struct ResumeCompletion(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ResumeCompletion {
    fn drop(&mut self) {
        if let Some(resume) = self.0.take() {
            let _ = resume.try_send(());
        }
    }
}

fn leaf(pane_id: &str) -> LayoutNode {
    LayoutNode::Leaf {
        pane_id: pane_id.into(),
    }
}
fn split(axis: Axis, ratio: u16, first: LayoutNode, second: LayoutNode) -> LayoutNode {
    LayoutNode::Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    }
}
fn set_layout(registry: &Registry, workspace_id: &str, layout: LayoutNode) {
    let expected_revision = registry
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .unwrap()
        .revision;
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: workspace_id.into(),
            expected_revision,
            layout: Some(layout),
        },
    );
}
fn launch_reader(
    registry: &Registry,
    home: &Path,
    script: &str,
    workspace_id: Option<&str>,
) -> String {
    let mut launch = spec(home);
    launch.argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
    send(
        registry,
        Operation::PaneSpawn {
            workspace_id: workspace_id.map(str::to_owned),
            expected_revision: workspace_id.map(|id| {
                registry
                    .snapshot()
                    .unwrap()
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == id)
                    .unwrap()
                    .revision
            }),
            spec: launch,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn wait_scene(registry: &Registry, id: &str, marker: &str) -> PaneSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let scene: PaneSnapshot = serde_json::from_value(snapshot(registry, id)).unwrap();
        let text: String = scene
            .frame
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .map(|cell| cell.text.as_str())
            .collect();
        if text.contains(marker) {
            return scene;
        }
        assert!(
            Instant::now() < deadline,
            "expected terminal output did not arrive"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn execution_completion_closes_all_projections(natural: bool) {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let owner = create(registry, "owner");
    let projection = create(registry, "projection");
    let sole = create(registry, "sole projection");
    let untouched = create(registry, "unaffected");
    let target = launch_reader(
        registry,
        &fixture.home("ending"),
        "printf FINAL-SCENE; IFS= read -r line; printf 'NATURAL-%s' \"$line\"; exit 7",
        Some(&owner),
    );
    let survivor_home = fixture.home("survivor");
    let survivor = launch_reader(
        registry,
        &survivor_home,
        "while IFS= read -r line; do printf '%s' \"$line\" > received; done",
        None,
    );
    let sibling = spawn(registry, &fixture.home("sibling"), None, None);
    let preserved = split(Axis::Horizontal, 711, leaf(&survivor), leaf(&sibling));
    set_layout(
        registry,
        &owner,
        split(Axis::Vertical, 173, leaf(&target), preserved.clone()),
    );
    set_layout(
        registry,
        &projection,
        split(Axis::Horizontal, 829, preserved.clone(), leaf(&target)),
    );
    set_layout(registry, &sole, leaf(&target));
    set_layout(registry, &untouched, preserved.clone());
    let target_view = view(registry, &target, 80);
    let survivor_view = view(registry, &survivor, 80);
    wait_scene(registry, &target, "FINAL-SCENE");
    let before = registry.snapshot().unwrap();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let resume = ResumeCompletion(Some(resume));
    let ending_registry = registry.clone();
    let ending_target = target.clone();
    let ending_view = target_view.clone();
    let ending = std::thread::spawn(move || {
        ending_registry.request(Request::new(if natural {
            Operation::Pane {
                pane_id: ending_target,
                request: PaneRequest::Input {
                    view_id: ending_view,
                    data: b"done\n".to_vec(),
                },
            }
        } else {
            Operation::PaneTerminate {
                pane_id: ending_target,
            }
        }))
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(5));
    if entered.is_err() {
        drop(resume);
        ending.join().unwrap().unwrap();
        panic!("execution completion did not reach durable publication");
    }
    // An explicit termination racing the observer must share its single commit.
    let repeating_registry = registry.clone();
    let repeating_target = target.clone();
    let repeating = std::thread::spawn(move || {
        repeating_registry.request(Request::new(Operation::PaneTerminate {
            pane_id: repeating_target,
        }))
    });
    assert_eq!(
        value(&registry.snapshot().unwrap().workspaces),
        value(&before.workspaces),
        "uncommitted layout changes must remain invisible during persistence"
    );
    drop(resume);
    ending.join().unwrap().unwrap();
    repeating.join().unwrap().unwrap();
    let after = registry.snapshot().unwrap();
    assert!(after.revision > before.revision);
    assert_eq!(after.workspaces.len(), before.workspaces.len());
    assert_eq!(after.panes.len(), before.panes.len());
    for old in &before.workspaces {
        let current = after
            .workspaces
            .iter()
            .find(|workspace| workspace.id == old.id)
            .unwrap();
        if old.id == untouched {
            assert_eq!(value(current), value(old));
        } else {
            assert_eq!(current.revision, old.revision + 1);
            assert_eq!(current.id, old.id);
            assert_eq!(current.name, old.name);
            assert_eq!(
                current.layout,
                if old.id == sole {
                    None
                } else {
                    Some(preserved.clone())
                }
            );
        }
    }
    for old in &before.panes {
        let current = after.panes.iter().find(|pane| pane.id == old.id).unwrap();
        if old.id == target {
            assert_eq!(current.status, PaneStatus::Exited);
            assert!(current.exit_code.is_some());
            if natural {
                assert_eq!(current.exit_code, Some(7));
            }
        } else {
            assert_eq!(current.status, old.status);
            assert_eq!(current.exit_code, old.exit_code);
        }
    }
    let scene = wait_scene(
        registry,
        &target,
        if natural {
            "NATURAL-done"
        } else {
            "FINAL-SCENE"
        },
    );
    assert_eq!(
        scene.lifecycle,
        if natural { "exited" } else { "terminated" }
    );
    assert!(scene.error.is_none());
    assert_eq!(
        scene.exit_code.map(|code| code as i32),
        after
            .panes
            .iter()
            .find(|pane| pane.id == target)
            .unwrap()
            .exit_code
    );
    for _ in 0..3 {
        send(
            registry,
            Operation::PaneTerminate {
                pane_id: target.clone(),
            },
        );
    }
    let repeated = send(
        registry,
        Operation::Pane {
            pane_id: target.clone(),
            request: PaneRequest::Terminate {
                view_id: target_view.clone(),
            },
        },
    );
    assert_eq!(repeated["terminated"], false);
    assert_eq!(repeated["already_completed"], true);
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: target.clone(),
                request: PaneRequest::Input {
                    view_id: target_view.clone(),
                    data: b"never delivered\n".to_vec()
                },
            }))
            .is_err()
    );
    for action in [KeyAction::Press, KeyAction::Repeat] {
        assert!(
            registry
                .request(Request::new(Operation::Pane {
                    pane_id: target.clone(),
                    request: PaneRequest::Key {
                        view_id: target_view.clone(),
                        event: KeyEvent {
                            key: "KeyD".into(),
                            modifiers: 2,
                            text: String::new(),
                            action,
                            unshifted_codepoint: u32::from('d'),
                        },
                    },
                }))
                .is_err()
        );
    }
    send(
        registry,
        Operation::Pane {
            pane_id: target.clone(),
            request: PaneRequest::Update {
                view_id: target_view.clone(),
                spec: ViewSpec {
                    size: size(80),
                    visible: false,
                    interactive: false,
                    input: true,
                    clipboard: false,
                },
            },
        },
    );
    for request in [
        PaneRequest::Focus {
            view_id: target_view.clone(),
            focused: false,
        },
        PaneRequest::Key {
            view_id: target_view.clone(),
            event: KeyEvent {
                key: "KeyD".into(),
                modifiers: 2,
                text: String::new(),
                action: KeyAction::Release,
                unshifted_codepoint: u32::from('d'),
            },
        },
        PaneRequest::Mouse {
            view_id: target_view.clone(),
            event: MouseEvent {
                x: 0.0,
                y: 0.0,
                modifiers: 0,
                button: MouseButton::Left,
                action: MouseAction::Release,
            },
        },
    ] {
        let cleanup = send(
            registry,
            Operation::Pane {
                pane_id: target.clone(),
                request,
            },
        );
        assert_eq!(cleanup["accepted"], false);
        assert!(cleanup["write_id"].is_null());
    }
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: target.clone(),
                request: PaneRequest::Focus {
                    view_id: "not-an-attached-view".into(),
                    focused: false
                },
            }))
            .is_err()
    );
    send(
        registry,
        Operation::Pane {
            pane_id: target.clone(),
            request: PaneRequest::Detach {
                view_id: target_view,
            },
        },
    );
    send(
        registry,
        Operation::Pane {
            pane_id: survivor.clone(),
            request: PaneRequest::Input {
                view_id: survivor_view,
                data: b"still-usable\n".to_vec(),
            },
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(survivor_home.join("received")).ok().as_deref() != Some(b"still-usable") {
        assert!(
            Instant::now() < deadline,
            "unrelated pane lost usable input"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(snapshot(registry, &survivor)["lifecycle"], "running");
    let stored = read_record(fixture._root.path()).unwrap().unwrap();
    assert_eq!(value(&stored.registry.workspaces), value(&after.workspaces));
    let stored_target = stored
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == target)
        .unwrap();
    assert_eq!(stored_target.status, PaneStatus::Exited);
    assert_eq!(
        stored_target.exit_code,
        scene.exit_code.map(|code| code as i32)
    );
    assert_eq!(
        snapshot(registry, &target)["frame"],
        serde_json::to_value(scene.frame).unwrap()
    );
}

#[test]
fn natural_shell_exit_atomically_closes_every_projection_and_retains_execution_history() {
    execution_completion_closes_all_projections(true);
}

#[test]
fn explicit_termination_atomically_closes_every_projection_and_retains_execution_history() {
    execution_completion_closes_all_projections(false);
}

#[test]
fn failed_completion_persistence_does_not_publish_partial_closure_or_success() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "completion storage failure");
    let pane_id = spawn(
        registry,
        &fixture.home("ending"),
        Some(workspace_id),
        Some(1),
    );
    let retained = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    let before = registry.snapshot().unwrap();
    let path = fixture._root.path().join("registry.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::PaneTerminate {
                pane_id: pane_id.clone()
            }))
            .is_err()
    );
    assert!(registry.snapshot().is_err());
    assert!(
        registry
            .request(Request::new(Operation::PaneTerminate { pane_id }))
            .is_err()
    );
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(
        value(&state.record.registry.workspaces),
        value(&before.workspaces)
    );
    assert!(state.durability_error.is_some());
    drop(state);
    let final_scene: PaneSnapshot =
        serde_json::from_value(retained.request(PaneRequest::Inspect).unwrap()).unwrap();
    assert_eq!(final_scene.lifecycle, "terminated");
    assert!(final_scene.frame.is_some());
    assert!(final_scene.exit_code.is_some());
}

#[test]
fn faulted_execution_keeps_its_projection_and_never_acknowledges_cleanup_or_termination() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace_id = create(registry, "inspectable fault");
    let pane_id = spawn(
        registry,
        &fixture.home("fault"),
        Some(workspace_id.clone()),
        Some(1),
    );
    let view_id = view(registry, &pane_id, 80);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: pane_id.clone(),
        },
    );
    set_layout(registry, &workspace_id, leaf(&pane_id));
    let before = registry.snapshot().unwrap();
    let retained = registry
        .inner
        .state
        .lock()
        .unwrap()
        .live
        .get(&pane_id)
        .unwrap()
        .clone();
    // Inject an engine/cleanup fault into a retained real execution, without
    // weakening process ownership or leaving an actual live process behind.
    let fault = "injected runtime failure";
    {
        let mut published = retained.shared.published.lock().unwrap();
        published.lifecycle = "fault".into();
        published.error = Some(fault.into());
    }
    registry.record_completion(&pane_id).unwrap();
    let after = registry.snapshot().unwrap();
    assert_eq!(value(&after.workspaces), value(&before.workspaces));
    assert_eq!(after.panes[0].status, PaneStatus::Stopped);
    let observed: PaneSnapshot = serde_json::from_value(snapshot(registry, &pane_id)).unwrap();
    assert_eq!(observed.error.as_deref(), Some(fault));
    assert!(observed.frame.is_some());
    assert!(
        registry
            .request(Request::new(Operation::PaneTerminate {
                pane_id: pane_id.clone(),
            }))
            .is_err()
    );
    for request in [
        PaneRequest::Focus {
            view_id: view_id.clone(),
            focused: false,
        },
        PaneRequest::Key {
            view_id: view_id.clone(),
            event: KeyEvent {
                key: "KeyD".into(),
                modifiers: 2,
                text: String::new(),
                action: KeyAction::Release,
                unshifted_codepoint: u32::from('d'),
            },
        },
        PaneRequest::Mouse {
            view_id,
            event: MouseEvent {
                x: 0.0,
                y: 0.0,
                modifiers: 0,
                button: MouseButton::Left,
                action: MouseAction::Release,
            },
        },
    ] {
        assert!(
            registry
                .request(Request::new(Operation::Pane {
                    pane_id: pane_id.clone(),
                    request,
                }))
                .is_err()
        );
    }
}
