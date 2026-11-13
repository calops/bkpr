use super::*;
use bkpr_protocol::{
    AgentScope, CaptureEvent, CaptureGrant, ConversationSource, HarnessKind, HarnessRecovery, Size,
};
use serde_json::Value;
use std::{
    fs,
    process::{Child, Command},
    time::Instant,
};

fn wire(socket: &Path, operation: Operation) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    wire::write_frame(
        &mut stream,
        Kind::Request,
        Codec::new().encode_request(&Request::new(operation))?,
    )?;
    let bytes = wire::read_frame(&mut stream, Kind::Response, MAX_RESPONSE)?;
    Ok(serde_json::to_value(wire::decode_response(&bytes)?)?)
}

struct NativeChild(Child);
impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "owned native callback subprocess, requires private HOME target"]
fn startup_control_peer_fixture() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    fs::write(home.join("peer-stage"), b"entered").unwrap();
    assert_eq!(
        fs::read(home.join(".bkpr-native-home")).unwrap(),
        b"isolated native pane runtime\n"
    );
    if let Ok(target) = fs::read(home.join("control-target.json")) {
        let target: Value = serde_json::from_slice(&target).unwrap();
        // The race under test is shared Control, not the separate early-root
        // capture-admission race. Begin only after the first spawn has committed.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !home.join("control-ready").exists() {
            assert!(
                Instant::now() < deadline,
                "fixture capture was not released"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
        let begin = wire(
            &socket,
            Operation::HarnessBegin {
                kind: HarnessKind::Codex,
            },
        )
        .unwrap();
        fs::write(home.join("peer-stage"), begin.to_string()).unwrap();
        assert_eq!(begin["ok"], true, "{begin}");
        let grant: CaptureGrant = serde_json::from_value(begin["result"].clone()).unwrap();
        // Capture binds the recognized native descendant's real CWD, never the
        // supervisor's. Use an owned copied shell, not a real harness/provider.
        let _native = NativeChild(Command::new(home.join("native/codex"))
            .args(["-c", "printf ready > native-ready; while test ! -f release-native; do sleep 0.02; done"])
            .current_dir(&home).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(home.join("native-ready")).ok().as_deref() != Some(b"ready") {
            assert!(
                Instant::now() < deadline,
                "owned native descendant did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let bind = wire(
            &socket,
            Operation::HarnessEvent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                event: CaptureEvent::Bind {
                    conversation_id: "startup-attacker".into(),
                    source: ConversationSource::CodexResume,
                    expected_revision: None,
                },
            },
        )
        .unwrap();
        fs::write(home.join("peer-stage"), bind.to_string()).unwrap();
        assert_eq!(bind["ok"], true, "{bind}");
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let request = Request::new(Operation::Agent {
            launch_id: grant.launch_id,
            token: grant.token,
            guard: None,
            request: AgentRequest::Control {
                scope: AgentScope::Session,
                reply: bkpr_protocol::control::ControlReply::Full,
                request: Box::new(Operation::PaneResume {
                    pane_id: target["pane_id"].as_str().unwrap().into(),
                    expected: serde_json::from_value(target["expected"].clone()).unwrap(),
                }),
            },
        });
        wire::write_frame(
            &mut stream,
            Kind::Request,
            Codec::new().encode_request(&request).unwrap(),
        )
        .unwrap();
        fs::write(home.join("control-sent"), b"sent").unwrap();
        let bytes = wire::read_frame(&mut stream, Kind::Response, MAX_RESPONSE).unwrap();
        let reply = wire::decode_response(&bytes).unwrap();
        fs::write(
            home.join("control-result"),
            serde_json::to_vec(&reply).unwrap(),
        )
        .unwrap();
    }
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

struct ReleaseSpawn(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for ReleaseSpawn {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}
struct Serving {
    registry: Registry,
    socket: PathBuf,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
impl Drop for Serving {
    fn drop(&mut self) {
        let _ = wire(&self.socket, Operation::Shutdown);
        let _ = self.registry.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn spec(home: &Path) -> PaneSpawnSpec {
    PaneSpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
        home: home.into(),
        cwd: home.into(),
        isolated_environment: true,
        title: None,
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
    }
}

#[test]
fn agent_control_cannot_consume_a_saved_pane_while_automatic_batch_is_pending() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("state");
    let socket = root.path().join("daemon.sock");
    let first_home = root.path().join("first");
    let second_home = root.path().join("second");
    let target_home = root.path().join("target");
    let registry = Registry::open(&state).unwrap();
    let mut created = Vec::new();
    for (name, home) in [
        ("first", &first_home),
        ("second", &second_home),
        ("target", &target_home),
    ] {
        let result = registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: Some(name.into()),
                initial: Some(spec(home)),
            }))
            .unwrap()
            .into_json()
            .unwrap();
        created.push(result);
    }
    let target = created[2]["pane_id"].as_str().unwrap().to_owned();
    let target_workspace = created[2]["workspace_id"].as_str().unwrap().to_owned();
    registry.shutdown().unwrap();
    drop(registry);
    // A private saved-native fixture, not live capture authority. The requesting
    // peer below still has to acquire its real OS/PTY-authenticated launch grant.
    let descriptor = HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: "saved-target".into(),
        cwd: target_home,
    };
    let registry_file = state.join("registry.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&registry_file).unwrap()).unwrap();
    saved["recoveries"][&target] = serde_json::to_value(&descriptor).unwrap();
    saved["automatic"][&target] = serde_json::to_value(&descriptor).unwrap();
    fs::write(&registry_file, serde_json::to_vec(&saved).unwrap()).unwrap();
    fs::write(
        first_home.join("control-target.json"),
        serde_json::to_vec(&json!({"pane_id":target,"expected":descriptor})).unwrap(),
    )
    .unwrap();
    fs::create_dir(first_home.join("native")).unwrap();
    fs::copy("/bin/sh", first_home.join("native/codex")).unwrap();
    let executable = std::env::current_exe().unwrap();
    let launcher = root.path().join("codex");
    std::os::unix::fs::symlink(&executable, &launcher).unwrap();
    let registry = Registry::open(&state).unwrap();
    registry.configure_harnesses(&socket, &executable).unwrap();
    let (first_entered, first_release) = registry.test_pause_next_spawn();
    let mut initial = spec(&first_home);
    initial.argv = vec![
        launcher.to_str().unwrap().into(),
        "--ignored".into(),
        "--exact".into(),
        "transport::startup_tests::startup_control_peer_fixture".into(),
        "--nocapture".into(),
    ];
    let bound = bind(&socket).unwrap();
    let serving_registry = registry.clone();
    let worker = thread::spawn(move || bound.serve_bootstrap(serving_registry, Some(initial)));
    let _serving = Serving {
        registry: registry.clone(),
        socket,
        worker: Some(worker),
    };
    let first_release = ReleaseSpawn(Some(first_release));
    first_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let (second_entered, second_release) = registry.test_pause_next_spawn();
    let second_release = ReleaseSpawn(Some(second_release));
    drop(first_release);
    second_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    fs::write(first_home.join("control-ready"), b"committed").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !first_home.join("control-sent").exists() {
        assert!(
            Instant::now() < deadline,
            "native callback could not run during bootstrap; first={:?}, second={:?}, panes={:?}",
            fs::read_to_string(first_home.join("peer-stage")),
            fs::read_to_string(second_home.join("peer-stage")),
            registry.snapshot().unwrap().panes
        );
        thread::sleep(Duration::from_millis(5));
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while !first_home.join("control-result").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let premature = first_home.join("control-result").exists();
    let pending = registry.snapshot().unwrap();
    let preserved = pending.panes.iter().any(|pane| {
        pane.id == target && pane.workspace_id.as_deref() == Some(target_workspace.as_str())
    });
    drop(second_release);
    drop(_serving);
    assert!(
        !premature,
        "shared Agent Control bypassed ordinary startup admission"
    );
    assert!(
        preserved,
        "shared control consumed saved membership before automatic restore reached it"
    );
}

#[test]
fn startup_cancellation_replies_before_owned_spawn_cleanup_and_preserves_storage_failures() {
    for poison_storage in [false, true] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        let registry = Registry::open(&state).unwrap();
        let socket = root.path().join("daemon.sock");
        let bound = bind(&socket).unwrap();
        let (spawn_entered, spawn_release) = registry.test_pause_next_spawn();
        let initial = spec(&root.path().join("home"));
        let serving = registry.clone();
        let worker = thread::spawn(move || bound.serve_bootstrap(serving, Some(initial)));
        let mut server = Serving {
            registry: registry.clone(),
            socket: socket.clone(),
            worker: Some(worker),
        };
        let release = ReleaseSpawn(Some(spawn_release));
        spawn_entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let control_entered = registry.test_next_request_wait();
        let queued_socket = socket.clone();
        let queued = thread::spawn(move || {
            wire(
                &queued_socket,
                Operation::WorkspaceCreate {
                    name: Some("cancelled-before-dispatch".into()),
                    initial: None,
                },
            )
        });
        control_entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        if poison_storage {
            fs::remove_file(state.join("registry.json")).unwrap();
            fs::create_dir(state.join("registry.json")).unwrap();
            assert!(
                registry
                    .request(Request::new(Operation::WorkspaceCreate {
                        name: Some("storage-failure".into()),
                        initial: None,
                    }))
                    .is_err()
            );
        }
        let stopping_socket = socket.clone();
        let stopping = thread::spawn(move || wire(&stopping_socket, Operation::Shutdown));
        let closing = registry.test_wait_for_closing();
        // The queued request must receive its failure while Shutdown still waits
        // for the owned startup process, rather than relying on process exit/EOF.
        let rejected: Response = serde_json::from_value(queued.join().unwrap().unwrap()).unwrap();
        drop(release);
        let stopped: Response = serde_json::from_value(stopping.join().unwrap().unwrap()).unwrap();
        let result = server.worker.take().unwrap().join().unwrap();
        assert!(closing);
        assert!(!rejected.ok);
        assert!(rejected.result.is_none());
        assert!(rejected.error.is_some());
        assert_eq!(
            rejected.failure,
            if poison_storage {
                None
            } else {
                Some(RequestFailure::SessionClosing {
                    outcome: RequestOutcome::NotDispatched,
                })
            }
        );
        assert_eq!(stopped.ok, !poison_storage, "{stopped:?}");
        assert!(stopped.failure.is_none());
        assert_eq!(result.is_err(), poison_storage);
        if !poison_storage {
            assert!(registry.snapshot().unwrap().workspaces.is_empty());
        }
    }
}
