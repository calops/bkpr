use super::spawn_tests::{ResumeSpawn, sentinel_spec, wait_for_sentinel};
use super::*;

fn value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}
fn stopped(registry: &Registry, workspace_id: &str, home: &Path) -> String {
    let id = Uuid::new_v4().to_string();
    registry
        .mutate(|r| {
            r.panes.push(PaneInfo {
                id: id.clone(),
                workspace_id: Some(workspace_id.into()),
                title: "historical program".into(),
                status: PaneStatus::Stopped,
                exit_code: Some(7),
                cwd: home.into(),
                home: home.into(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                project: None,
                isolated_environment: true,
            });
            Ok(())
        })
        .unwrap();
    project(registry, workspace_id, &id);
    id
}
fn project(registry: &Registry, workspace_id: &str, pane_id: &str) {
    let revision = registry
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .unwrap()
        .revision;
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: workspace_id.into(),
            expected_revision: revision,
            layout: Some(LayoutNode::Leaf {
                pane_id: pane_id.into(),
            }),
        },
    );
}
fn reopen(
    registry: &Registry,
    workspace_id: &str,
    pane_id: &str,
    launch: PaneSpawnSpec,
) -> Operation {
    let revision = registry
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .unwrap()
        .revision;
    Operation::PaneReopen {
        workspace_id: workspace_id.into(),
        expected_revision: revision,
        pane_id: pane_id.into(),
        spec: launch,
    }
}
fn pause_spawn(registry: &Registry) -> (std::sync::mpsc::Receiver<()>, ResumeSpawn) {
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    (entered_rx, ResumeSpawn(Some(resume)))
}

#[test]
fn reopen_replaces_only_the_selected_projection_and_retains_historical_identity() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let executable = std::env::current_exe().unwrap();
    registry
        .configure_harnesses(&fixture._root.path().join("private.sock"), &executable)
        .unwrap();
    let a = create(registry, "selected");
    let b = create(registry, "historical peer projection");
    let old = stopped(registry, &a, &fixture.home("old"));
    project(registry, &b, &old);
    let before = registry.snapshot().unwrap();
    let home = fixture.home("fresh");
    let mut launch = spec(&home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s\\n%s\\n%s\\n' \"$BKPR_PANE_ID\" \"$PWD\" \"$PATH\" > identity; IFS= read -r line; printf '%s' \"$line\" > received; exec sleep 60".into(),
    ];
    let result: RegistryResult =
        serde_json::from_value(send(registry, reopen(registry, &a, &old, launch))).unwrap();
    let new = result.pane_id.unwrap();
    assert_ne!(new, old);
    let old_info = before.panes.iter().find(|pane| pane.id == old).unwrap();
    assert_eq!(
        value(
            result
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == old)
                .unwrap()
        ),
        value(old_info)
    );
    let selected = result
        .registry
        .workspaces
        .iter()
        .find(|w| w.id == a)
        .unwrap();
    assert_eq!(
        selected.revision,
        before
            .workspaces
            .iter()
            .find(|w| w.id == a)
            .unwrap()
            .revision
            + 1
    );
    assert_eq!(
        selected.layout,
        Some(LayoutNode::Leaf {
            pane_id: new.clone()
        })
    );
    assert_eq!(
        value(
            result
                .registry
                .workspaces
                .iter()
                .find(|w| w.id == b)
                .unwrap()
        ),
        value(before.workspaces.iter().find(|w| w.id == b).unwrap())
    );
    let new_info = result
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == new)
        .unwrap();
    assert_eq!(new_info.workspace_id.as_deref(), Some(a.as_str()));
    assert_eq!(new_info.status, PaneStatus::Running);
    assert_eq!(new_info.cwd, home);
    assert!(new_info.harness.is_none());
    assert!(new_info.harness_status.is_none());
    let deadline = Instant::now() + Duration::from_secs(3);
    let identity = loop {
        if let Ok(identity) = fs::read_to_string(home.join("identity"))
            && identity.lines().count() == 3
        {
            break identity;
        }
        assert!(
            Instant::now() < deadline,
            "new child did not publish its launch environment"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let lines = identity.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], new);
    assert_eq!(Path::new(lines[1]), home);
    assert!(std::env::split_paths(lines[2]).any(|path| {
        fs::read_link(path.join("omp")).ok().as_deref() == Some(executable.as_path())
    }));
    let attached = view(registry, &new, 80);
    send(
        registry,
        Operation::Pane {
            pane_id: new.clone(),
            request: PaneRequest::Input {
                view_id: attached,
                data: b"new execution\n".to_vec(),
            },
        },
    );
    while fs::read(home.join("received")).ok().as_deref() != Some(b"new execution") {
        assert!(
            Instant::now() < deadline,
            "new execution did not receive authorized input"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: old,
                request: PaneRequest::Inspect,
            }))
            .is_err()
    );
}

#[test]
fn exited_scene_views_and_authority_are_not_adopted_by_reopen() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "retained scene");
    let old = spawn(
        registry,
        &fixture.home("old-scene"),
        Some(workspace.clone()),
        Some(1),
    );
    let old_view = view(registry, &old, 80);
    send(
        registry,
        Operation::PaneTerminate {
            pane_id: old.clone(),
        },
    );
    project(registry, &workspace, &old);
    let final_scene = snapshot(registry, &old);
    let new_home = fixture.home("new-scene");
    let result = send(
        registry,
        reopen(registry, &workspace, &old, spec(&new_home)),
    );
    let new = result["pane_id"].as_str().unwrap();
    assert_ne!(new, old);
    assert_eq!(snapshot(registry, &old)["frame"], final_scene["frame"]);
    let fresh: PaneSnapshot = serde_json::from_value(snapshot(registry, new)).unwrap();
    assert!(fresh.views.is_empty());
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: new.into(),
                request: PaneRequest::Input {
                    view_id: old_view.clone(),
                    data: b"must not arrive\n".to_vec()
                },
            }))
            .is_err()
    );
    let fresh_view = view(registry, new, 80);
    assert_ne!(fresh_view, old_view);
}

#[test]
fn live_stale_and_unprojected_reopen_requests_never_execute() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "live");
    let live = spawn(
        registry,
        &fixture.home("live"),
        Some(workspace.clone()),
        Some(1),
    );
    let unrelated = create(registry, "no leaf");
    let dead = stopped(registry, &unrelated, &fixture.home("dead"));
    let before = registry.snapshot().unwrap();
    let disk = fs::read(fixture._root.path().join("registry.json")).unwrap();
    for (name, operation) in [
        (
            "live-rejected",
            reopen(
                registry,
                &workspace,
                &live,
                sentinel_spec(&fixture.home("live-rejected")),
            ),
        ),
        (
            "stale-rejected",
            Operation::PaneReopen {
                workspace_id: unrelated.clone(),
                expected_revision: 1,
                pane_id: dead.clone(),
                spec: sentinel_spec(&fixture.home("stale-rejected")),
            },
        ),
        (
            "missing-leaf-rejected",
            reopen(
                registry,
                &workspace,
                &dead,
                sentinel_spec(&fixture.home("missing-leaf-rejected")),
            ),
        ),
        (
            "unknown-pane-rejected",
            reopen(
                registry,
                &unrelated,
                &Uuid::new_v4().to_string(),
                sentinel_spec(&fixture.home("unknown-pane-rejected")),
            ),
        ),
    ] {
        assert!(
            registry.request(Request::new(operation)).is_err(),
            "accepted {name}"
        );
        assert!(
            !fixture.home(name).exists(),
            "rejected request performed child setup"
        );
    }
    assert_eq!(
        value(&registry.snapshot().unwrap().workspaces),
        value(&before.workspaces)
    );
    assert_eq!(registry.snapshot().unwrap().panes.len(), before.panes.len());
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        disk
    );
    assert_eq!(snapshot(registry, &live)["lifecycle"], "running");
}

#[test]
fn failed_reopen_preserves_the_old_leaf_and_releases_its_reservation() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "retryable");
    let old = stopped(registry, &workspace, &fixture.home("old"));
    let before = registry.snapshot().unwrap();
    let disk = fs::read(fixture._root.path().join("registry.json")).unwrap();
    let mut bad_executable = spec(&fixture.home("bad-executable"));
    bad_executable.argv = vec!["/this-executable-does-not-exist-bkpr".into()];
    let mut missing_cwd = spec(&fixture.home("missing-cwd-home"));
    missing_cwd.cwd = fixture.home("removed-project");
    for launch in [bad_executable, missing_cwd] {
        assert!(
            registry
                .request(Request::new(reopen(registry, &workspace, &old, launch)))
                .is_err()
        );
        let after = registry.snapshot().unwrap();
        assert_eq!(value(&after), value(&before));
        assert_eq!(
            fs::read(fixture._root.path().join("registry.json")).unwrap(),
            disk
        );
    }
    let result = send(
        registry,
        reopen(registry, &workspace, &old, spec(&fixture.home("retry"))),
    );
    assert_ne!(result["pane_id"].as_str().unwrap(), old);
}

#[test]
fn reopen_reservation_blocks_conflicting_mutations_but_not_other_workspaces() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let a = create(registry, "reserved");
    let b = create(registry, "independent");
    let old = stopped(registry, &a, &fixture.home("old"));
    let operation = reopen(registry, &a, &old, sentinel_spec(&fixture.home("accepted")));
    let expected_revision = registry
        .snapshot()
        .unwrap()
        .workspaces
        .iter()
        .find(|w| w.id == a)
        .unwrap()
        .revision;
    let (entered, release) = pause_spawn(registry);
    let working = registry.clone();
    let worker = std::thread::spawn(move || working.request(Request::new(operation)));
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    for operation in [
        Operation::WorkspaceRemove {
            workspace_id: a.clone(),
            expected_revision,
        },
        Operation::LayoutUpdate {
            workspace_id: a.clone(),
            expected_revision,
            layout: None,
        },
        reopen(
            registry,
            &a,
            &old,
            sentinel_spec(&fixture.home("duplicate")),
        ),
        Operation::PaneForget {
            pane_id: old.clone(),
        },
    ] {
        assert!(registry.request(Request::new(operation)).is_err());
    }
    assert!(!fixture.home("duplicate").exists());
    let unrelated_home = fixture.home("unrelated");
    send(
        registry,
        Operation::PaneSpawn {
            workspace_id: Some(b),
            expected_revision: Some(1),
            spec: sentinel_spec(&unrelated_home),
        },
    );
    wait_for_sentinel(&unrelated_home);
    drop(release);
    let result = worker.join().unwrap().unwrap();
    assert_ne!(result["pane_id"].as_str().unwrap(), old);
    wait_for_sentinel(&fixture.home("accepted"));
}

#[test]
fn completion_during_reopen_rejects_stale_commit_without_losing_the_old_leaf() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "completion race");
    let old = stopped(registry, &workspace, &fixture.home("old"));
    let revision = registry.snapshot().unwrap().workspaces[0].revision;
    let survivor = spawn(
        registry,
        &fixture.home("ending"),
        Some(workspace.clone()),
        Some(revision),
    );
    let operation = reopen(
        registry,
        &workspace,
        &old,
        sentinel_spec(&fixture.home("replacement")),
    );
    let (entered, release) = pause_spawn(registry);
    let working = registry.clone();
    let worker = std::thread::spawn(move || working.request(Request::new(operation)));
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    send(registry, Operation::PaneTerminate { pane_id: survivor });
    let after_completion = registry.snapshot().unwrap();
    drop(release);
    assert!(worker.join().unwrap().is_err());
    let after = registry.snapshot().unwrap();
    assert_eq!(value(&after), value(&after_completion));
    assert_eq!(
        after.workspaces[0].layout,
        Some(LayoutNode::Leaf { pane_id: old })
    );
}

#[cfg(target_os = "linux")]
#[test]
fn failed_durable_reopen_kills_the_new_child_without_publishing_a_partial_replacement() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "durability failure");
    let old = stopped(registry, &workspace, &fixture.home("old"));
    let before = registry.snapshot().unwrap();
    let home = fixture.home("new-child");
    let mut launch = spec(&home);
    launch.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$$\" > pid; exec sleep 60".into(),
    ];
    let operation = reopen(registry, &workspace, &old, launch);
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = ResumeSpawn(Some(resume));
    let working = registry.clone();
    let worker = std::thread::spawn(move || working.request(Request::new(operation)));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let pid: i32 = loop {
        if let Ok(pid) = fs::read_to_string(home.join("pid"))
            && let Ok(pid) = pid.parse()
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "spawned child never ran");
        std::thread::sleep(Duration::from_millis(5));
    };
    let path = fixture._root.path().join("registry.json");
    let retained = fixture._root.path().join("retained.json");
    fs::rename(&path, &retained).unwrap();
    fs::create_dir(&path).unwrap();
    drop(release);
    assert!(worker.join().unwrap().is_err());
    assert!(registry.snapshot().is_err());
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(value(&state.record.registry), value(&before));
    assert!(state.live.is_empty());
    drop(state);
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "failed replacement child survived cleanup"
    );
    let durable: Record = serde_json::from_slice(&fs::read(retained).unwrap()).unwrap();
    assert_eq!(
        value(&durable.registry.workspaces),
        value(&before.workspaces)
    );
    assert_eq!(durable.registry.panes.len(), before.panes.len());
}

#[test]
fn bootstrap_preserves_dead_layouts_uses_current_argv_and_does_not_restart_live_panes() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let main = create(registry, "Main");
    create(registry, "Main 2");
    let old = stopped(registry, &main, &fixture.home("old"));
    let before = registry.snapshot().unwrap();
    let home = fixture.home("current-argv");
    let result: RegistryResult =
        serde_json::from_value(registry.bootstrap(sentinel_spec(&home)).unwrap()).unwrap();
    wait_for_sentinel(&home);
    assert_eq!(
        result.registry.workspaces.len(),
        before.workspaces.len() + 1
    );
    let boot = result
        .registry
        .workspaces
        .iter()
        .find(|w| Some(w.id.as_str()) == result.workspace_id.as_deref())
        .unwrap();
    assert_eq!(boot.name, "Main 3");
    assert_eq!(
        boot.layout,
        Some(LayoutNode::Leaf {
            pane_id: result.pane_id.clone().unwrap()
        })
    );
    assert_eq!(
        value(
            result
                .registry
                .workspaces
                .iter()
                .find(|w| w.id == main)
                .unwrap()
        ),
        value(before.workspaces.iter().find(|w| w.id == main).unwrap())
    );
    assert_eq!(
        value(result.registry.panes.iter().find(|p| p.id == old).unwrap()),
        value(before.panes.iter().find(|p| p.id == old).unwrap())
    );
    let ignored = fixture.home("ignored-bootstrap");
    let second: RegistryResult =
        serde_json::from_value(registry.bootstrap(sentinel_spec(&ignored)).unwrap()).unwrap();
    assert!(second.pane_id.is_none());
    assert!(!ignored.exists());
    assert_eq!(
        value(&second.registry.workspaces),
        value(&result.registry.workspaces)
    );
    assert_eq!(second.registry.panes.len(), result.registry.panes.len());
}

#[test]
fn fresh_bootstrap_failure_never_publishes_an_empty_workspace() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let before = registry.snapshot().unwrap();
    let mut launch = spec(&fixture.home("failed-bootstrap"));
    launch.argv = vec!["/this-executable-does-not-exist-bkpr".into()];
    assert!(registry.bootstrap(launch).is_err());
    assert_eq!(value(&registry.snapshot().unwrap()), value(&before));
    let result: RegistryResult = serde_json::from_value(
        registry
            .bootstrap(spec(&fixture.home("bootstrap")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result.registry.workspaces.len(), 1);
    assert_eq!(result.registry.workspaces[0].name, "Main");
    assert_eq!(result.registry.panes[0].status, PaneStatus::Running);
}
