use super::spawn_tests::{sentinel_spec, wait_for_sentinel};
use super::*;
use std::os::unix::ffi::OsStringExt;

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "restore fixture did not reach expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn intent(registry: &Registry, id: &str, cwd: &Path, active: bool) -> HarnessRecovery {
    let descriptor = HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: format!("native-{id}"),
        cwd: cwd.into(),
    };
    let _transaction = registry.inner.transaction.lock().unwrap();
    let mut next = registry.inner.state.lock().unwrap().record.clone();
    next.recoveries.insert(id.into(), descriptor.clone());
    if active {
        next.automatic.insert(id.into(), descriptor.clone());
    }
    next.registry.revision += 1;
    commit(&registry.inner, next, |_| {}).unwrap();
    descriptor
}
fn configure(registry: &Registry, root: &Path) -> PathBuf {
    let executable = root.join("current-launcher");
    fs::write(&executable, b"#!/bin/sh\nprintf '%s\\n' \"$@\" > native-argv\nprintf '%s' \"$HOME\" > native-home\nwhile [ ! -f refuse ]; do sleep 0.02; done\nexit 23\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    registry
        .configure_harnesses(&root.join("owned.sock"), &executable)
        .unwrap();
    executable
}

#[test]
fn cold_restore_keeps_nested_layout_hidden_ownership_names_and_excludes_receipts() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace = create(&registry, "Saved project");
    let mut ids = Vec::new();
    let mut windows = Vec::new();
    for name in ["left", "middle", "right", "hidden", "other", "receipt"] {
        let home = root.path().join(name);
        crate::prepare_home(&home).unwrap();
        let (id, window) = unavailable(&registry, &workspace, &home);
        ids.push(id);
        windows.push(window);
    }
    registry
        .mutate(|r| {
            let workspace = r.workspaces.iter_mut().find(|w| w.id == workspace).unwrap();
            workspace
                .windows
                .retain(|w| w.id == windows[0] || w.id == windows[4]);
            workspace.windows[0].name = "Nested".into();
            workspace.windows[0].layout = Some(LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 317,
                first: Box::new(LayoutNode::Leaf {
                    pane_id: ids[0].clone(),
                }),
                second: Box::new(LayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 681,
                    first: Box::new(LayoutNode::Leaf {
                        pane_id: ids[1].clone(),
                    }),
                    second: Box::new(LayoutNode::Leaf {
                        pane_id: ids[2].clone(),
                    }),
                }),
            });
            for (index, pane) in r.panes.iter_mut().enumerate() {
                if index < 4 {
                    pane.window_id = Some(windows[0].clone());
                }
                if index == 5 {
                    pane.workspace_id = None;
                    pane.window_id = None;
                }
            }
            Ok(())
        })
        .unwrap();
    let before = registry.snapshot().unwrap();
    drop(registry);
    let registry = Registry::open(root.path()).unwrap();
    let caller = root.path().join("caller-must-not-be-created");
    let result = registry.bootstrap(sentinel_spec(&caller)).unwrap();
    assert_eq!(result["failures"], json!([]));
    assert_eq!(result["restored"].as_array().unwrap().len(), 5);
    for name in ["left", "middle", "right", "hidden", "other"] {
        wait_for_sentinel(&root.path().join(name));
    }
    assert!(!root.path().join("receipt/executed").exists());
    assert!(!caller.exists());
    let after = registry.snapshot().unwrap();
    assert_eq!(
        serde_json::to_value(&before.workspaces).unwrap(),
        serde_json::to_value(&after.workspaces).unwrap()
    );
    assert_eq!(after.panes.len(), 6);
    assert_eq!(
        after
            .panes
            .iter()
            .filter(|p| p.status == PaneStatus::Running)
            .count(),
        5
    );
    let durable_before = fs::read(root.path().join("registry.json")).unwrap();
    registry.bootstrap(sentinel_spec(&caller)).unwrap();
    assert_eq!(
        fs::read(root.path().join("registry.json")).unwrap(),
        durable_before
    );
    registry.shutdown().unwrap();
}

#[test]
fn partial_missing_cwd_and_unbound_native_refusal_preserve_original_intent_and_layout() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    configure(registry, fixture._root.path());
    let workspace = create(registry, "partial");
    let native_home = fixture.home("native");
    crate::prepare_home(&native_home).unwrap();
    let (native, _) = unavailable(registry, &workspace, &native_home);
    let expected = intent(registry, &native, &native_home, true);
    let missing_home = fixture.home("missing");
    let (missing, _) = unavailable(registry, &workspace, &missing_home);
    let shell_home = fixture.home("shell");
    crate::prepare_home(&shell_home).unwrap();
    let (shell, _) = unavailable(registry, &workspace, &shell_home);
    let saved_layout = registry.snapshot().unwrap().workspaces;
    let result = registry
        .bootstrap(sentinel_spec(&fixture.home("caller")))
        .unwrap();
    assert_eq!(result["restored"].as_array().unwrap().len(), 2);
    assert_eq!(result["failures"][0]["pane_id"], missing);
    assert!(!missing_home.exists());
    wait_for_sentinel(&shell_home);
    wait_until(|| native_home.join("native-argv").exists());
    assert_eq!(
        fs::read_to_string(native_home.join("native-argv")).unwrap(),
        format!("run\nomp\n--resume\n{}\n", expected.conversation_id)
    );
    assert_eq!(
        fs::read(native_home.join("native-home")).unwrap(),
        native_home.as_os_str().as_encoded_bytes()
    );
    fs::write(native_home.join("refuse"), b"now").unwrap();
    wait_until(|| {
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .any(|p| p.id == native && p.status == PaneStatus::Stopped && p.exit_code == Some(23))
    });
    registry.record_completion(&native).unwrap();
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.record.recoveries.get(&native), Some(&expected));
    assert_eq!(state.record.automatic.get(&native), Some(&expected));
    assert!(
        state
            .record
            .registry
            .panes
            .iter()
            .any(|p| p.id == shell && p.status == PaneStatus::Running)
    );
    assert_eq!(
        serde_json::to_value(&state.record.registry.workspaces).unwrap(),
        serde_json::to_value(saved_layout).unwrap()
    );
    assert!(!registry.session_end_requested());
    drop(state);
    let recovery = registry
        .recovery_candidates()
        .unwrap()
        .into_iter()
        .find(|c| c.pane_id == native)
        .unwrap();
    assert_eq!(recovery.status, RecoveryStatus::Recoverable);
    assert!(recovery.reason.unwrap().contains("before verified binding"));
    // Repeated bootstrap does not replay an ambiguous native attempt or missing CWD.
    let before = registry.snapshot().unwrap().revision;
    registry
        .bootstrap(sentinel_spec(&fixture.home("caller")))
        .unwrap();
    assert_eq!(registry.snapshot().unwrap().revision, before);
}

#[test]
fn current_native_resume_adapters_never_replay_saved_flags_and_use_current_geometry() {
    let fixture = Fixture::new();
    let executable = configure(&fixture.registry, fixture._root.path());
    for (kind, flag, id) in [
        (
            HarnessKind::Claude,
            "--resume",
            "claude-conversation".to_owned(),
        ),
        (HarnessKind::Omp, "--resume", "omp-conversation".to_owned()),
        (
            HarnessKind::Codex,
            "resume",
            "codex-conversation".to_owned(),
        ),
        (
            HarnessKind::Agy,
            "--conversation",
            Uuid::new_v4().to_string(),
        ),
    ] {
        let descriptor = HarnessRecovery {
            kind,
            conversation_id: id.clone(),
            cwd: fixture._root.path().into(),
        };
        let spec = fixture
            .registry
            .native_resume_spec(
                &descriptor,
                fixture._root.path().into(),
                true,
                "Saved title".into(),
                size(137),
            )
            .unwrap();
        assert_eq!(
            spec.argv,
            vec![
                executable.to_str().unwrap().to_owned(),
                "run".into(),
                kind.as_str().into(),
                flag.into(),
                id
            ]
        );
        assert_eq!(spec.size, size(137));
        assert_eq!(spec.cwd, descriptor.cwd);
        assert_eq!(spec.title.as_deref(), Some("Saved title"));
        assert!(spec.isolated_environment);
    }
}

#[test]
fn short_lived_restore_cannot_close_batch_before_independent_saved_execution_starts() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let workspace = create(registry, "batch");
    let quick_home = fixture.home("quick");
    crate::prepare_home(&quick_home).unwrap();
    let (quick, window) = unavailable(registry, &workspace, &quick_home);
    let other_home = fixture.home("other");
    crate::prepare_home(&other_home).unwrap();
    let (other, other_window) = unavailable(registry, &workspace, &other_home);
    registry.inner.state.lock().unwrap().restore_batch = true;
    let mut quick_spec = spec(&quick_home);
    quick_spec.argv = vec!["/bin/sh".into(), "-c".into(), "exit 0".into()];
    registry
        .spawn(
            SpawnTarget::Restore {
                workspace_id: workspace.clone(),
                window_id: window,
                pane_id: quick.clone(),
            },
            quick_spec,
        )
        .unwrap();
    wait_until(|| {
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .any(|p| p.id == quick && p.status == PaneStatus::Exited)
    });
    assert!(!registry.session_end_requested());
    assert!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .any(|p| p.id == quick && p.workspace_id.is_some())
    );
    registry
        .spawn(
            SpawnTarget::Restore {
                workspace_id: workspace.clone(),
                window_id: other_window,
                pane_id: other.clone(),
            },
            sentinel_spec(&other_home),
        )
        .unwrap();
    registry.inner.state.lock().unwrap().restore_batch = false;
    registry.drain_completions().unwrap();
    wait_for_sentinel(&other_home);
    let snapshot = registry.snapshot().unwrap();
    assert!(
        snapshot
            .panes
            .iter()
            .any(|p| p.id == quick && p.workspace_id.is_none())
    );
    assert!(
        snapshot
            .panes
            .iter()
            .any(|p| p.id == other && p.status == PaneStatus::Running)
    );
    assert_eq!(snapshot.workspaces.len(), 1);
    assert!(!registry.session_end_requested());
}

#[test]
fn historical_native_descriptor_and_schema_three_migration_restore_shell_not_old_harness() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace = create(&registry, "history");
    let home = root.path().join("shell-home");
    crate::prepare_home(&home).unwrap();
    let (pane, _) = unavailable(&registry, &workspace, &home);
    let expected = intent(&registry, &pane, &home, false);
    drop(registry);
    let path = root.path().join("registry.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["version"] = json!(3);
    value.as_object_mut().unwrap().remove("automatic");
    value.as_object_mut().unwrap().remove("directories");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    configure(&registry, root.path());
    let result = registry.bootstrap(sentinel_spec(root.path())).unwrap();
    assert_eq!(result["failures"], json!([]));
    wait_for_sentinel(&home);
    assert!(!home.join("native-argv").exists());
    let state = registry.inner.state.lock().unwrap();
    assert!(state.record.automatic.is_empty());
    assert_eq!(state.record.recoveries.get(&pane), Some(&expected));
    drop(state);
    registry.shutdown().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn real_shell_directory_transitions_persist_raw_bytes_and_survive_crash_checkpoint() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let home = fixture.home("shell");
    crate::prepare_home(&home).unwrap();
    let raw = home.join(std::ffi::OsString::from_vec(b"directory-\xff".to_vec()));
    fs::create_dir(&raw).unwrap();
    std::os::unix::fs::symlink(&raw, home.join("destination")).unwrap();
    let workspace = create(registry, "directory");
    let (pane, window) = unavailable(registry, &workspace, &home);
    let mut shell = spec(&home);
    shell.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "cd destination; printf ready > reached; exec sleep 60".into(),
    ];
    registry
        .spawn(
            SpawnTarget::Restore {
                workspace_id: workspace,
                window_id: window,
                pane_id: pane.clone(),
            },
            shell,
        )
        .unwrap();
    wait_until(|| raw.join("reached").exists());
    let before = fs::read(fixture._root.path().join("registry.json")).unwrap();
    registry.observe_processes();
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        before
    );
    registry.flush_directories().unwrap();
    let checkpoint = fs::read(fixture._root.path().join("registry.json")).unwrap();
    assert_ne!(checkpoint, before);
    let durable = read_record(fixture._root.path()).unwrap().unwrap();
    assert_eq!(durable.directories[&pane].cwd, raw);
    assert_eq!(
        durable
            .registry
            .panes
            .iter()
            .find(|p| p.id == pane)
            .unwrap()
            .cwd,
        home
    );
    assert!(
        durable
            .registry
            .panes
            .iter()
            .all(|p| p.foreground_cwd.is_none() && p.harness.is_none())
    );
    registry.observe_processes();
    registry.flush_directories().unwrap();
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        checkpoint
    );
    // A second private registry consumes the pre-shutdown checkpoint: this models
    // restart after abrupt loss without allowing teardown to refresh the saved CWD.
    let restart = private_tempdir();
    persist(restart.path(), &durable).unwrap();
    let reopened = Registry::open(restart.path()).unwrap();
    reopened
        .bootstrap(sentinel_spec(&fixture.home("ignored")))
        .unwrap();
    wait_for_sentinel(&raw);
    assert!(
        reopened
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .any(|p| p.id == pane && p.status == PaneStatus::Running)
    );
    reopened.shutdown().unwrap();
}

#[test]
fn directory_writer_failure_poisoning_fails_closed_without_consuming_last_good_intent() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("shell");
    let pane_id = spawn(registry, &home, None, None);
    let live = registry.inner.state.lock().unwrap().live[&pane_id].clone();
    let target = fixture.home("new-directory");
    fs::create_dir(&target).unwrap();
    {
        let mut state = registry.inner.state.lock().unwrap();
        directories::queue(&mut state, &pane_id, &live, &target, None);
    }
    let path = fixture._root.path().join("registry.json");
    fs::rename(&path, fixture._root.path().join("last-good.json")).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(registry.flush_directories().is_err());
    assert!(
        registry
            .snapshot()
            .unwrap_err()
            .to_string()
            .contains("persistence failed")
    );
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: None
            }))
            .is_err()
    );
    let last = fs::read(fixture._root.path().join("last-good.json")).unwrap();
    let stored: Record = serde_json::from_slice(&last).unwrap();
    assert!(!stored.directories.contains_key(&pane_id));
}

#[test]
fn native_end_keeps_explicit_receipt_but_clears_automatic_role_and_shutdown_preserves_it() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let workspace = create(registry, "native then shell");
    let home = fixture.home("saved-shell");
    crate::prepare_home(&home).unwrap();
    let (pane, _) = unavailable(registry, &workspace, &home);
    let expected = intent(registry, &pane, &home, true);
    let process = crate::harness_process::process(std::process::id() as i32)
        .unwrap()
        .unwrap();
    let launch = crate::harness::Launch {
        root: process.clone(),
        pane_root: process,
        grant: CaptureGrant {
            launch_id: Uuid::new_v4().to_string(),
            token: "private-test".into(),
            pane_id: pane.clone(),
            kind: expected.kind,
        },
        managed: ManagedHarness {
            launch_id: "private-test-launch".into(),
            conversation: Some(NativeConversation {
                id: expected.conversation_id.clone(),
                source: ConversationSource::OmpExtension,
                generation: 1,
            }),
            error: None,
        },
    };
    registry.inner.state.lock().unwrap().closing = true;
    {
        let _transaction = registry.inner.transaction.lock().unwrap();
        registry.automatic_end(&launch).unwrap();
    }
    assert_eq!(
        registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .automatic
            .get(&pane),
        Some(&expected)
    );
    registry.inner.state.lock().unwrap().closing = false;
    {
        let _transaction = registry.inner.transaction.lock().unwrap();
        registry.automatic_end(&launch).unwrap();
    }
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.record.recoveries.get(&pane), Some(&expected));
    assert!(!state.record.automatic.contains_key(&pane));
    drop(state);
    let result = registry
        .bootstrap(sentinel_spec(fixture._root.path()))
        .unwrap();
    assert_eq!(result["failures"], json!([]));
    wait_for_sentinel(&home);
    assert!(
        registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .automatic
            .is_empty()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_shutdown_captures_fresh_cwd_without_waiting_for_periodic_observer() {
    let root = private_tempdir();
    let home = root.path().join("shell");
    crate::prepare_home(&home).unwrap();
    fs::create_dir(home.join("last-directory")).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let workspace = create(&registry, "graceful");
    let (pane, window) = unavailable(&registry, &workspace, &home);
    let mut shell = spec(&home);
    shell.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "cd last-directory; printf ready > reached; exec sleep 60".into(),
    ];
    registry
        .spawn(
            SpawnTarget::Restore {
                workspace_id: workspace,
                window_id: window,
                pane_id: pane.clone(),
            },
            shell,
        )
        .unwrap();
    wait_until(|| home.join("last-directory/reached").exists());
    assert!(
        !registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .directories
            .contains_key(&pane)
    );
    registry.shutdown().unwrap();
    drop(registry);
    let reopened = Registry::open(root.path()).unwrap();
    reopened
        .bootstrap(sentinel_spec(&root.path().join("ignored")))
        .unwrap();
    wait_for_sentinel(&home.join("last-directory"));
    assert_eq!(
        reopened.inner.state.lock().unwrap().record.directories[&pane].cwd,
        home.join("last-directory")
    );
    reopened.shutdown().unwrap();
}

#[test]
fn stale_execution_directory_observation_never_rewrites_replacement_intent() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let first = spawn(registry, &fixture.home("first"), None, None);
    let second = spawn(registry, &fixture.home("second"), None, None);
    let destination = fixture.home("new");
    fs::create_dir(&destination).unwrap();
    let (original, replacement) = {
        let state = registry.inner.state.lock().unwrap();
        (state.live[&first].clone(), state.live[&second].clone())
    };
    {
        let mut state = registry.inner.state.lock().unwrap();
        directories::queue(&mut state, &first, &replacement, &destination, None);
        assert!(!state.pending_directories.contains_key(&first));
        directories::queue(&mut state, &first, &original, &destination, None);
        state.live.insert(first.clone(), replacement);
    }
    registry.flush_directories().unwrap();
    {
        let mut state = registry.inner.state.lock().unwrap();
        assert!(!state.record.directories.contains_key(&first));
        state.live.insert(first, original);
        state.pending_directories.clear();
    }
}

#[test]
fn blocked_directory_fsync_does_not_hold_registry_state_or_unrelated_terminal_input() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let workspace = create(registry, "independent");
    let home = fixture.home("reader");
    let mut reader = spec(&home);
    reader.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "IFS= read -r line; printf '%s' \"$line\" > received; exec sleep 60".into(),
    ];
    let result = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace,
            expected_revision: 1,
            name: None,
            spec: reader,
        },
    );
    let pane_id = result["pane_id"].as_str().unwrap();
    let view_id = view(registry, pane_id, 80);
    let pane = registry.inner.state.lock().unwrap().live[pane_id].clone();
    let destination = fixture.home("directory");
    fs::create_dir(&destination).unwrap();
    directories::queue(
        &mut registry.inner.state.lock().unwrap(),
        pane_id,
        &pane,
        &destination,
        None,
    );
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = super::spawn_tests::ResumeSpawn(Some(resume));
    let writer_registry = registry.clone();
    let writer = std::thread::spawn(move || writer_registry.flush_directories());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    send(
        registry,
        Operation::Pane {
            pane_id: pane_id.into(),
            request: PaneRequest::Input {
                view_id,
                data: b"unrelated-progress\n".to_vec(),
            },
        },
    );
    wait_until(|| fs::read(home.join("received")).ok().as_deref() == Some(b"unrelated-progress"));
    drop(release);
    writer.join().unwrap().unwrap();
}

#[test]
fn actual_foreground_shell_demotes_old_native_role_but_retains_explicit_descriptor() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let workspace = create(registry, "foreground shell");
    let home = fixture.home("shell");
    let result = send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace,
            expected_revision: 1,
            name: None,
            spec: sentinel_spec(&home),
        },
    );
    let pane = result["pane_id"].as_str().unwrap();
    wait_for_sentinel(&home);
    let expected = intent(registry, pane, &home, true);
    registry.observe_processes();
    registry.flush_directories().unwrap();
    let state = registry.inner.state.lock().unwrap();
    assert!(!state.record.automatic.contains_key(pane));
    assert_eq!(state.record.recoveries.get(pane), Some(&expected));
    assert_eq!(state.record.directories[pane].cwd, home);
}

#[test]
fn native_directory_intent_updates_raw_paths_without_replaying_live_authority() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let home = fixture.home("shell");
    let pane_id = spawn(registry, &home, None, None);
    let expected = intent(registry, &pane_id, &home, true);
    let live = registry.inner.state.lock().unwrap().live[&pane_id].clone();
    let raw = home.join(std::ffi::OsString::from_vec(b"native-\xff".to_vec()));
    fs::create_dir(&raw).unwrap();
    directories::queue(
        &mut registry.inner.state.lock().unwrap(),
        &pane_id,
        &live,
        &raw,
        Some((expected.kind, expected.conversation_id.as_str())),
    );
    registry.flush_directories().unwrap();
    let record = read_record(fixture._root.path()).unwrap().unwrap();
    assert_eq!(record.recoveries[&pane_id].cwd, raw);
    assert_eq!(record.automatic[&pane_id].cwd, raw);
    assert_eq!(record.directories[&pane_id].cwd, raw);
    assert!(record.registry.panes.iter().all(|p| p.harness.is_none()));
}

#[test]
fn duplicate_automatic_conversation_attempts_share_existing_native_reservation() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    configure(registry, fixture._root.path());
    let workspace = create(registry, "aliases");
    let home = fixture.home("native-home");
    crate::prepare_home(&home).unwrap();
    let (first, _) = unavailable(registry, &workspace, &home);
    let (second, _) = unavailable(registry, &workspace, &home);
    let expected = intent(registry, &first, &home, true);
    {
        let _transaction = registry.inner.transaction.lock().unwrap();
        let mut next = registry.inner.state.lock().unwrap().record.clone();
        next.recoveries.insert(second.clone(), expected.clone());
        next.automatic.insert(second.clone(), expected);
        next.registry.revision += 1;
        commit(&registry.inner, next, |_| {}).unwrap();
    }
    let result = registry
        .bootstrap(sentinel_spec(&fixture.home("must-not-fallback")))
        .unwrap();
    assert_eq!(result["restored"].as_array().unwrap().len(), 1);
    assert_eq!(result["failures"].as_array().unwrap().len(), 1);
    assert_eq!(result["failures"][0]["pane_id"], second);
    assert!(
        result["failures"][0]["error"]
            .as_str()
            .unwrap()
            .contains("already in progress")
    );
    assert!(!fixture.home("must-not-fallback").exists());
    let state = registry.inner.state.lock().unwrap();
    assert_eq!(state.resuming.len(), 1);
    assert!(
        state
            .record
            .registry
            .panes
            .iter()
            .any(|p| p.id == second && p.status == PaneStatus::Stopped)
    );
}

#[test]
fn only_native_refusal_keeps_session_recoverable_but_explicit_pending_termination_retires_it() {
    for explicit in [false, true] {
        let fixture = Fixture::new();
        let registry = &fixture.registry;
        configure(registry, fixture._root.path());
        let workspace = create(registry, "last native");
        let home = fixture.home("native-home");
        crate::prepare_home(&home).unwrap();
        let (pane, _) = unavailable(registry, &workspace, &home);
        let expected = intent(registry, &pane, &home, true);
        if !explicit {
            fs::write(home.join("refuse"), b"now").unwrap();
        }
        registry
            .bootstrap(sentinel_spec(&fixture.home("no-fallback")))
            .unwrap();
        if explicit {
            wait_until(|| home.join("native-argv").exists());
            send(
                registry,
                Operation::PaneTerminate {
                    pane_id: pane.clone(),
                },
            );
            wait_until(|| registry.session_end_requested());
            let state = registry.inner.state.lock().unwrap();
            assert!(state.record.registry.workspaces.is_empty());
            assert!(!state.record.recoveries.contains_key(&pane));
            assert!(!state.record.automatic.contains_key(&pane));
        } else {
            wait_until(|| {
                registry.snapshot().unwrap().panes.iter().any(|p| {
                    p.id == pane && p.status == PaneStatus::Stopped && p.exit_code == Some(23)
                })
            });
            assert!(!registry.session_end_requested());
            let state = registry.inner.state.lock().unwrap();
            assert_eq!(state.record.registry.workspaces.len(), 1);
            assert_eq!(state.record.automatic.get(&pane), Some(&expected));
        }
        assert!(!fixture.home("no-fallback").exists());
    }
}

#[test]
fn one_observation_batch_durably_commits_all_changed_panes_without_extra_deadline() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let first = spawn(registry, &fixture.home("first"), None, None);
    let second = spawn(registry, &fixture.home("second"), None, None);
    let first_cwd = fixture.home("first-next");
    let second_cwd = fixture.home("second-next");
    fs::create_dir(&first_cwd).unwrap();
    fs::create_dir(&second_cwd).unwrap();
    let before = fs::read(fixture._root.path().join("registry.json")).unwrap();
    let revision = registry.snapshot().unwrap().revision;
    {
        let mut state = registry.inner.state.lock().unwrap();
        let first_pane = state.live[&first].clone();
        let second_pane = state.live[&second].clone();
        directories::queue(&mut state, &first, &first_pane, &first_cwd, None);
        directories::queue(&mut state, &second, &second_pane, &second_cwd, None);
    }
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        before
    );
    registry.flush_directories().unwrap();
    let checkpoint = fs::read(fixture._root.path().join("registry.json")).unwrap();
    let record = read_record(fixture._root.path()).unwrap().unwrap();
    assert_eq!(record.registry.revision, revision + 1);
    assert_eq!(record.directories[&first].cwd, first_cwd);
    assert_eq!(record.directories[&second].cwd, second_cwd);
    registry.flush_directories().unwrap();
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        checkpoint
    );
}
