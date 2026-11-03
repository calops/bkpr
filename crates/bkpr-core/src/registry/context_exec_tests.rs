use super::spawn_tests::ResumeSpawn;
use super::*;

const SNAPSHOT_SCRIPT: &str = r#"
write_snapshot() {
    env -0 > "$1.tmp" &&
    printf 'CONTEXT_EXEC_COMPLETE=ready\000' >> "$1.tmp" &&
    mv "$1.tmp" "$1"
}
write_snapshot exec-env || exit 70
while IFS= read -r command; do
    write_snapshot next-env || exit 71
done
"#;

fn context_spec(home: &Path) -> PaneSpawnSpec {
    let mut launch = spec(home);
    launch.argv = vec!["/bin/sh".into(), "-c".into(), SNAPSHOT_SCRIPT.into()];
    launch
}

fn read_snapshot(home: &Path, name: &str) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(bytes) = fs::read(home.join(name))
            && bytes.ends_with(b"CONTEXT_EXEC_COMPLETE=ready\0")
        {
            return bytes;
        }
        assert!(Instant::now() < deadline, "child did not finish {name}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn environment_fields(bytes: &[u8]) -> BTreeMap<&str, &str> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (key, value) = field.split_at(field.iter().position(|byte| *byte == b'=').unwrap());
            (
                std::str::from_utf8(key).unwrap(),
                std::str::from_utf8(&value[1..]).unwrap(),
            )
        })
        .collect()
}

fn assert_canonical(registry: &Registry, pane_id: &str, home: &Path) -> Vec<u8> {
    let bytes = read_snapshot(home, "exec-env");
    let fields = environment_fields(&bytes);
    let state = registry.inner.state.lock().unwrap();
    let pane = state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == pane_id)
        .unwrap();
    assert_eq!(
        fields.get("BKPR_SESSION_ID"),
        Some(&registry.inner.session.session_id.as_str())
    );
    assert_eq!(
        fields.get("BKPR_SESSION_NAME"),
        Some(&registry.inner.session.name.as_str())
    );
    assert_eq!(fields.get("BKPR_PANE_ID"), Some(&pane_id));
    match &pane.workspace_id {
        Some(id) => {
            let workspace = state
                .record
                .registry
                .workspaces
                .iter()
                .find(|w| &w.id == id)
                .unwrap();
            assert_eq!(fields.get("BKPR_WORKSPACE_ID"), Some(&id.as_str()));
            assert_eq!(
                fields.get("BKPR_WORKSPACE_NAME"),
                Some(&workspace.name.as_str())
            );
        }
        None => {
            assert!(!fields.contains_key("BKPR_WORKSPACE_ID"));
            assert!(!fields.contains_key("BKPR_WORKSPACE_NAME"));
        }
    }
    assert!(!fields.contains_key("BKPR_WINDOW_ID"));
    assert!(!fields.contains_key("BKPR_WINDOW_NAME"));
    bytes
}

fn pane_id(value: &Value) -> String {
    value["pane_id"].as_str().unwrap().to_owned()
}

fn workspace_and_window(registry: &Registry, pane_id: &str) -> (Workspace, Window) {
    let snapshot = registry.snapshot().unwrap();
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == pane_id)
        .unwrap();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| Some(&w.id) == pane.workspace_id.as_ref())
        .unwrap()
        .clone();
    let window = workspace
        .windows
        .iter()
        .find(|w| Some(&w.id) == pane.window_id.as_ref())
        .unwrap()
        .clone();
    (workspace, window)
}

#[test]
fn canonical_context_reaches_bootstrap_workspace_window_and_append_execs() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("bootstrap");
    let initial = registry.bootstrap(context_spec(&home)).unwrap();
    let initial_id = pane_id(&initial);
    assert_canonical(registry, &initial_id, &home);
    let (workspace, _) = workspace_and_window(registry, &initial_id);
    assert_eq!(workspace.name, "Main");

    let home = fixture.home("workspace");
    let created = send(
        registry,
        Operation::WorkspaceCreate {
            name: Some("Canonical λ workspace".into()),
            initial: Some(context_spec(&home)),
        },
    );
    let created_id = pane_id(&created);
    assert_canonical(registry, &created_id, &home);
    let (workspace, _) = workspace_and_window(registry, &created_id);

    let home = fixture.home("window");
    let opened = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace.id,
            expected_revision: workspace.revision,
            name: Some("Second window".into()),
            spec: context_spec(&home),
        },
    );
    let opened_id = pane_id(&opened);
    assert_canonical(registry, &opened_id, &home);
    let (workspace, window) = workspace_and_window(registry, &opened_id);

    let home = fixture.home("append");
    let appended = send(
        registry,
        Operation::PaneSpawn {
            workspace_id: Some(workspace.id),
            window_id: Some(window.id),
            expected_revision: Some(window.revision),
            spec: context_spec(&home),
        },
    );
    assert_canonical(registry, &pane_id(&appended), &home);
}

#[test]
fn reopened_and_resumed_children_get_current_canonical_workspace() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let executable = fixture._root.path().join("private-native-launcher");
    fs::write(&executable, format!("#!/bin/sh\n{SNAPSHOT_SCRIPT}")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    registry
        .configure_harnesses(&fixture._root.path().join("private.sock"), &executable)
        .unwrap();
    let workspace = create(registry, "Saved label");
    let home = fixture.home("reopened");
    let (old, window) = unavailable(registry, &workspace, &home);
    let opened = send(
        registry,
        Operation::PaneReopen {
            workspace_id: workspace.clone(),
            window_id: window,
            expected_revision: 1,
            pane_id: old.clone(),
            spec: context_spec(&home),
        },
    );
    assert_canonical(registry, &pane_id(&opened), &home);
    assert!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == old)
            .unwrap()
            .workspace_id
            .is_none()
    );

    let resume_home = fixture.home("resumed");
    crate::prepare_home(&resume_home).unwrap();
    let (source, _) = unavailable(registry, &workspace, &resume_home);
    let descriptor = HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: "private-context-exec-conversation".into(),
        cwd: resume_home.clone(),
    };
    {
        let _transaction = registry.inner.transaction.lock().unwrap();
        let mut next = registry.inner.state.lock().unwrap().record.clone();
        next.recoveries.insert(source.clone(), descriptor.clone());
        next.registry.revision = next_revision(next.registry.revision).unwrap();
        commit(&registry.inner, next, |_| {}).unwrap();
    }
    let resumed = registry.resume(&source, descriptor).unwrap();
    assert_canonical(registry, &pane_id(&resumed), &resume_home);
}

fn pause_next_spawn(registry: &Registry) -> (std::sync::mpsc::Receiver<()>, ResumeSpawn) {
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    (entered_rx, ResumeSpawn(Some(resume)))
}

#[test]
fn pending_names_are_reserved_for_exec_without_blocking_unrelated_workspaces() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let unrelated = create(registry, "Independent");
    let home = fixture.home("pending");
    let (entered, release) = pause_next_spawn(registry);
    let worker_registry = registry.clone();
    let launch = context_spec(&home);
    let worker = std::thread::spawn(move || {
        worker_registry.request(Request::new(Operation::WorkspaceCreate {
            name: None,
            initial: Some(launch),
        }))
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    let second_home = fixture.home("concurrent");
    let second = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(context_spec(&second_home)),
        },
    );
    let second_id = pane_id(&second);
    assert_canonical(registry, &second_id, &second_home);
    assert_eq!(
        workspace_and_window(registry, &second_id).0.name,
        "Workspace 2"
    );
    let metadata = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: None,
        },
    );
    let metadata_id = metadata["workspace_id"].as_str().unwrap();
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .workspaces
            .iter()
            .find(|w| w.id == metadata_id)
            .unwrap()
            .name,
        "Workspace 3"
    );

    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: unrelated.clone(),
            expected_revision: 1,
            name: "Still independent".into(),
        },
    );
    let unrelated_home = fixture.home("unrelated");
    let independent = send(
        registry,
        Operation::WindowCreate {
            workspace_id: unrelated,
            expected_revision: 2,
            name: None,
            spec: context_spec(&unrelated_home),
        },
    );
    assert_canonical(registry, &pane_id(&independent), &unrelated_home);
    // Explicit duplicate labels remain legal, including a label reserved by a child.
    let duplicate_home = fixture.home("duplicate");
    let duplicate = send(
        registry,
        Operation::WorkspaceCreate {
            name: Some("Workspace".into()),
            initial: Some(context_spec(&duplicate_home)),
        },
    );
    assert_canonical(registry, &pane_id(&duplicate), &duplicate_home);
    assert_eq!(
        workspace_and_window(registry, &pane_id(&duplicate)).0.name,
        "Workspace"
    );

    drop(release);
    let accepted = worker.join().unwrap().unwrap();
    let accepted_id = pane_id(&accepted);
    assert_canonical(registry, &accepted_id, &home);
    assert_eq!(
        workspace_and_window(registry, &accepted_id).0.name,
        "Workspace"
    );
}

#[test]
fn failed_pending_name_is_released_and_reused_by_the_next_actual_child() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let mut launch = context_spec(&fixture.home("failed"));
    launch.argv = vec!["/nonexistent-bkpr-context-exec".into()];
    let (entered, release) = pause_next_spawn(registry);
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        worker_registry.request(Request::new(Operation::WorkspaceCreate {
            name: None,
            initial: Some(launch),
        }))
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let concurrent_home = fixture.home("concurrent");
    let concurrent = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(context_spec(&concurrent_home)),
        },
    );
    assert_canonical(registry, &pane_id(&concurrent), &concurrent_home);
    assert_eq!(
        workspace_and_window(registry, &pane_id(&concurrent)).0.name,
        "Workspace 2"
    );
    drop(release);
    assert!(worker.join().unwrap().is_err());
    let retry_home = fixture.home("retry");
    let retry = send(
        registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(context_spec(&retry_home)),
        },
    );
    assert_canonical(registry, &pane_id(&retry), &retry_home);
    assert_eq!(
        workspace_and_window(registry, &pane_id(&retry)).0.name,
        "Workspace"
    );
}

#[test]
fn raw_exec_environment_stays_immutable_after_labels_layout_and_ownership_retire() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("immutable");
    let created = send(
        registry,
        Operation::WorkspaceCreate {
            name: Some("At exec".into()),
            initial: Some(context_spec(&home)),
        },
    );
    let id = pane_id(&created);
    let original = assert_canonical(registry, &id, &home);
    let pid = registry.inner.state.lock().unwrap().live[&id]
        .shared
        .published
        .lock()
        .unwrap()
        .pid
        .unwrap();
    let proc_path = format!("/proc/{pid}/environ");
    let raw = fs::read(&proc_path).unwrap();
    let (workspace, window) = workspace_and_window(registry, &id);
    send(
        registry,
        Operation::WorkspaceRename {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
            name: "After exec".into(),
        },
    );
    send(
        registry,
        Operation::WindowRename {
            workspace_id: workspace.id.clone(),
            window_id: window.id,
            expected_revision: window.revision,
            name: "Renamed window".into(),
        },
    );
    send(
        registry,
        Operation::PaneRename {
            pane_id: id.clone(),
            title: "Renamed pane".into(),
        },
    );
    let (workspace, window) = workspace_and_window(registry, &id);
    send(
        registry,
        Operation::LayoutUpdate {
            workspace_id: workspace.id,
            window_id: window.id,
            expected_revision: window.revision,
            layout: None,
        },
    );
    assert_eq!(fs::read(&proc_path).unwrap(), raw);
    let view_id = view(registry, &id, 80);
    send(
        registry,
        Operation::Pane {
            pane_id: id.clone(),
            request: PaneRequest::Input {
                view_id,
                data: b"snapshot\n".to_vec(),
            },
        },
    );
    let next = read_snapshot(&home, "next-env");
    let before = environment_fields(&original);
    let after = environment_fields(&next);
    for key in [
        "BKPR_SESSION_ID",
        "BKPR_SESSION_NAME",
        "BKPR_PANE_ID",
        "BKPR_WORKSPACE_ID",
        "BKPR_WORKSPACE_NAME",
    ] {
        assert_eq!(before.get(key), after.get(key));
    }
    assert_eq!(after["BKPR_WORKSPACE_NAME"], "At exec");
    let (workspace, _) = workspace_and_window(registry, &id);
    send(
        registry,
        Operation::WorkspaceClose {
            workspace_id: workspace.id,
            expected_revision: workspace.revision,
        },
    );
    assert!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .workspace_id
            .is_none()
    );
    assert_eq!(read_snapshot(&home, "exec-env"), original);
}

#[test]
fn inherited_context_is_stripped_without_clobbering_user_settings_or_standalone_boundaries() {
    const CHILD: &str = "CONTEXT_EXEC_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = private_tempdir();
        let home = root.path().join("parent-home");
        crate::prepare_home(&home).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("HOME", &home)
            .env(CHILD, "1")
            .env("CONTEXT_EXEC_USER_SETTING", "preserved λ user value");
        for (key, directory) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            command.env(key, home.join(directory));
        }
        for key in [
            "BKPR_SESSION_ID",
            "BKPR_SESSION_NAME",
            "BKPR_PANE_ID",
            "BKPR_WORKSPACE_ID",
            "BKPR_WORKSPACE_NAME",
            "BKPR_WINDOW_ID",
            "BKPR_RELAY_TOKEN",
            "BKPR_SOCKET",
            "BKPR_HARNESS_WRAPPER_DIR",
            "ZELLIJ_PANE_ID",
            "TMUX",
        ] {
            command.env(key, "stale-parent-context");
        }
        let output = command.args(["--exact", "registry::tests::context_exec_tests::inherited_context_is_stripped_without_clobbering_user_settings_or_standalone_boundaries", "--nocapture", "--test-threads=1"]).output().unwrap();
        assert!(
            output.status.success(),
            "private child test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed"),
            "private child did not exercise its test: {stdout}"
        );
        return;
    }

    let fixture = Fixture::new();
    let registry = &fixture.registry;
    for isolated in [false, true] {
        let home = fixture.home(if isolated { "isolated" } else { "normal" });
        crate::prepare_home(&home).unwrap();
        let mut launch = context_spec(&home);
        launch.isolated_environment = isolated;
        let created = send(
            registry,
            Operation::PaneSpawn {
                workspace_id: None,
                window_id: None,
                expected_revision: None,
                spec: launch,
            },
        );
        let bytes = assert_canonical(registry, &pane_id(&created), &home);
        let fields = environment_fields(&bytes);
        for key in [
            "BKPR_WINDOW_ID",
            "BKPR_RELAY_TOKEN",
            "BKPR_SOCKET",
            "BKPR_HARNESS_WRAPPER_DIR",
            "ZELLIJ_PANE_ID",
            "TMUX",
        ] {
            assert!(!fields.contains_key(key), "inherited {key}");
        }
        if isolated {
            assert!(!fields.contains_key("CONTEXT_EXEC_USER_SETTING"));
        } else {
            assert_eq!(
                fields["CONTEXT_EXEC_USER_SETTING"],
                "preserved λ user value"
            );
            assert_eq!(fields["PATH"], std::env::var("PATH").unwrap());
        }
    }
    for isolated in [false, true] {
        let home = fixture.home(if isolated {
            "unowned-isolated"
        } else {
            "unowned-normal"
        });
        crate::prepare_home(&home).unwrap();
        let launch = context_spec(&home);
        let unowned = Pane::spawn(SpawnSpec {
            argv: launch.argv,
            home: launch.home,
            cwd: launch.cwd,
            size: launch.size,
            isolated_process: false,
            isolated_environment: isolated,
        })
        .unwrap();
        let bytes = read_snapshot(&home, "exec-env");
        let fields = environment_fields(&bytes);
        assert_eq!(fields["BKPR_PANE_ID"], unowned.shared.pane_id);
        for key in [
            "BKPR_SESSION_ID",
            "BKPR_SESSION_NAME",
            "BKPR_WORKSPACE_ID",
            "BKPR_WORKSPACE_NAME",
            "BKPR_SOCKET",
            "BKPR_RELAY_TOKEN",
        ] {
            assert!(!fields.contains_key(key));
        }
        unowned.request(PaneRequest::Shutdown).unwrap();
    }
}
