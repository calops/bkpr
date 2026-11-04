use super::*;
use std::os::unix::ffi::OsStringExt;

fn save(registry: &Registry, pane: &str, descriptor: &HarnessRecovery) {
    let _transaction = registry.inner.transaction.lock().unwrap();
    let mut next = registry.inner.state.lock().unwrap().record.clone();
    next.recoveries.insert(pane.into(), descriptor.clone());
    next.registry.revision = next_revision(next.registry.revision).unwrap();
    validate_record(&next).unwrap();
    commit(&registry.inner, next, |_| {}).unwrap();
}
fn descriptor(cwd: &Path) -> HarnessRecovery {
    HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: "owned-native-conversation".into(),
        cwd: cwd.into(),
    }
}
fn configure_refusing_native(fixture: &Fixture) -> PathBuf {
    let home = fixture.home("native-home");
    crate::prepare_home(&home).unwrap();
    let executable = fixture._root.path().join("owned-refusing-launcher");
    fs::write(&executable, b"#!/bin/sh\nprintf started > resume-started\nwhile [ ! -f resume-exit ]; do sleep 0.02; done\nexit 23\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fixture
        .registry
        .configure_harnesses(&fixture._root.path().join("owned.sock"), &executable)
        .unwrap();
    home
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "fixture did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn wait_exited(registry: &Registry, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = registry.snapshot().unwrap();
        let pane = snapshot.panes.iter().find(|pane| pane.id == id).unwrap();
        if pane.status == PaneStatus::Exited && pane.workspace_id.is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "refused native process did not follow normal document completion"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn restart_retains_only_native_descriptor_and_raw_cwd_without_live_authority_or_read_side_effects()
{
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace = create(&registry, "saved");
    let (pane, _) = unavailable(&registry, &workspace, root.path());
    let raw = root
        .path()
        .join(std::ffi::OsString::from_vec(b"native-\xff".to_vec()));
    fs::create_dir(&raw).unwrap();
    let expected = descriptor(&raw);
    save(&registry, &pane, &expected);
    let stored: Value =
        serde_json::from_slice(&fs::read(root.path().join("registry.json")).unwrap()).unwrap();
    let fields = stored["recoveries"][&pane]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(fields, BTreeSet::from(["kind", "conversation_id", "cwd"]));
    drop(registry);
    let registry = Registry::open(root.path()).unwrap();
    let before = fs::read(root.path().join("registry.json")).unwrap();
    let list = registry
        .request(Request::new(Operation::RecoveryList))
        .unwrap()
        .into_json()
        .unwrap();
    let candidates: Vec<RecoveryCandidate> =
        serde_json::from_value(list["recoveries"].clone()).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].harness, expected);
    assert!(registry.inner.captures.lock().unwrap().launches.is_empty());
    assert!(registry.inner.state.lock().unwrap().live.is_empty());
    assert_eq!(fs::read(root.path().join("registry.json")).unwrap(), before);
    assert!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .all(|pane| pane.harness.is_none())
    );
}

// LEGACY_COMPAT(next-major): schema 2 did not authenticate durable recovery descriptors.
#[test]
fn schema_two_upgrade_does_not_promote_live_or_injected_recovery_facts() {
    let root = private_tempdir();
    let registry = Registry::open(root.path()).unwrap();
    let workspace = create(&registry, "saved");
    let (pane, _) = unavailable(&registry, &workspace, root.path());
    save(&registry, &pane, &descriptor(root.path()));
    drop(registry);
    let path = root.path().join("registry.json");
    let mut old: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old["version"] = json!(2);
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    assert!(registry.recovery_candidates().unwrap().is_empty());
    assert!(registry.inner.state.lock().unwrap().live.is_empty());
    let upgraded = fs::read(&path).unwrap();
    drop(registry);
    let _registry = Registry::open(root.path()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), upgraded);
}

#[test]
fn resume_reservation_refuses_stale_alias_close_reopen_and_forget_without_blocking_other_input() {
    let fixture = Fixture::new();
    let home = configure_refusing_native(&fixture);
    let registry = &fixture.registry;
    let workspace = create(registry, "saved");
    let (source, window) = unavailable(registry, &workspace, &home);
    let alias_workspace = create(registry, "alias");
    let (alias, _) = unavailable(registry, &alias_workspace, &home);
    let expected = descriptor(fixture._root.path());
    save(registry, &source, &expected);
    let mut alias_expected = expected.clone();
    alias_expected.cwd = fixture.home("different-cwd");
    save(registry, &alias, &alias_expected);
    let mut stale = expected.clone();
    stale.conversation_id = "replaced-native-conversation".into();
    assert!(registry.resume(&source, stale).is_err());
    assert!(!fixture._root.path().join("resume-started").exists());
    let other_home = fixture.home("independent");
    let mut other_spec = spec(&other_home);
    other_spec.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "IFS= read -r line; printf '%s' \"$line\" > received; exec sleep 60".into(),
    ];
    let other = send(
        registry,
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            spec: other_spec,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_view = view(registry, &other, 80);
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.spawn_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = spawn_tests::ResumeSpawn(Some(resume));
    let worker_registry = registry.clone();
    let worker_source = source.clone();
    let worker_expected = expected.clone();
    let worker =
        std::thread::spawn(move || worker_registry.resume(&worker_source, worker_expected));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(registry.resume(&alias, alias_expected).is_err());
    for operation in [
        Operation::PaneForget {
            pane_id: source.clone(),
        },
        Operation::WorkspaceClose {
            workspace_id: workspace.clone(),
            expected_revision: 2,
        },
        Operation::PaneReopen {
            workspace_id: workspace.clone(),
            window_id: window.clone(),
            expected_revision: 1,
            pane_id: source.clone(),
            spec: spec(&fixture.home("must-not-start")),
        },
    ] {
        assert!(registry.request(Request::new(operation)).is_err());
    }
    assert!(!fixture.home("must-not-start").exists());
    assert!(!fixture._root.path().join("resume-started").exists());
    send(
        registry,
        Operation::Pane {
            pane_id: other,
            request: PaneRequest::Input {
                view_id: other_view,
                data: b"independent\n".to_vec(),
            },
        },
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while fs::read(other_home.join("received")).ok().as_deref() != Some(b"independent") {
        assert!(
            std::time::Instant::now() < deadline,
            "unrelated pane did not process exact input while native resume was reserved"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(release);
    let result = worker.join().unwrap().unwrap();
    let new = result["pane_id"].as_str().unwrap();
    wait_file(&fixture._root.path().join("resume-started"));
    assert_ne!(new, source);
    fs::write(fixture._root.path().join("resume-exit"), b"refuse").unwrap();
    wait_exited(registry, new);
    let recoveries = registry.recovery_candidates().unwrap();
    assert!(
        recoveries
            .iter()
            .any(|candidate| candidate.pane_id == source
                && candidate.harness == expected
                && candidate.status == RecoveryStatus::Recoverable)
    );
    assert!(!recoveries.iter().any(|candidate| candidate.pane_id == new));
    let retry = registry.resume(&source, expected.clone()).unwrap();
    let retry_id = retry["pane_id"].as_str().unwrap();
    assert_ne!(retry_id, new);
    assert_ne!(retry["workspace_id"], result["workspace_id"]);
    wait_exited(registry, retry_id);
    send(
        registry,
        Operation::PaneForget {
            pane_id: source.clone(),
        },
    );
    assert!(
        !registry
            .recovery_candidates()
            .unwrap()
            .iter()
            .any(|candidate| candidate.pane_id == source)
    );
}

#[test]
fn invalid_resume_ids_and_raw_path_boundaries_never_execute_and_explicit_close_removes_intent() {
    let fixture = Fixture::new();
    let home = configure_refusing_native(&fixture);
    let registry = &fixture.registry;
    let workspace = create(registry, "saved");
    let (pane, _) = unavailable(registry, &workspace, &home);
    let expected = descriptor(fixture._root.path());
    save(registry, &pane, &expected);
    for id in ["--resume", "-", "native\ncommand", " padded "] {
        let mut invalid = expected.clone();
        invalid.conversation_id = id.into();
        assert!(registry.resume(&pane, invalid).is_err());
    }
    for cwd in [
        PathBuf::from("relative"),
        PathBuf::from(std::ffi::OsString::from_vec(b"/nul\0cwd".to_vec())),
    ] {
        let mut invalid = expected.clone();
        invalid.cwd = cwd;
        assert!(registry.resume(&pane, invalid).is_err());
    }
    assert!(!fixture._root.path().join("resume-started").exists());
    send(
        registry,
        Operation::WorkspaceClose {
            workspace_id: workspace,
            expected_revision: 2,
        },
    );
    assert!(registry.recovery_candidates().unwrap().is_empty());
    assert!(registry.resume(&pane, expected).is_err());
}

#[test]
fn started_unbound_native_remains_closable_and_releases_duplicate_reservation() {
    let fixture = Fixture::new();
    let home = configure_refusing_native(&fixture);
    let registry = &fixture.registry;
    let workspace = create(registry, "pending native");
    let (source, _) = unavailable(registry, &workspace, &home);
    let other_workspace = create(registry, "alias");
    let (alias, _) = unavailable(registry, &other_workspace, &home);
    let expected = descriptor(fixture._root.path());
    save(registry, &source, &expected);
    save(registry, &alias, &expected);
    let result = registry.resume(&source, expected.clone()).unwrap();
    let new = result["pane_id"].as_str().unwrap();
    wait_file(&fixture._root.path().join("resume-started"));
    assert!(registry.resume(&alias, expected.clone()).is_err());
    let before = registry.snapshot().unwrap();
    let owned = before
        .workspaces
        .iter()
        .find(|owned| owned.id == workspace)
        .unwrap();
    send(
        registry,
        Operation::WorkspaceClose {
            workspace_id: workspace,
            expected_revision: owned.revision,
        },
    );
    wait_exited(registry, new);
    assert!(
        !registry
            .recovery_candidates()
            .unwrap()
            .iter()
            .any(|candidate| candidate.pane_id == source)
    );
    fs::write(fixture._root.path().join("resume-exit"), b"refuse").unwrap();
    let retry = registry.resume(&alias, expected).unwrap();
    wait_exited(registry, retry["pane_id"].as_str().unwrap());
}
