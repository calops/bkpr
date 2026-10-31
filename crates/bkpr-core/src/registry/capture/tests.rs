//! Owned, local process/PTY fixtures; no provider or installed harness is invoked.
use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Child, Command},
    time::Instant,
};

fn wire(socket: &Path, operation: Operation) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    if matches!(&operation, Operation::HarnessBegin { .. })
        && let Some(home) = fixture_home()
    {
        fs::write(home.join("capture-connected"), b"connected before dispatch").unwrap();
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    serde_json::to_writer(&mut stream, &Request::new(operation)).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn await_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture did not produce {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn fixture_home() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    (fs::read(home.join("capture-fixture")).ok().as_deref() == Some(b"owned capture fixture"))
        .then_some(home)
}
struct ServerGuard {
    socket: PathBuf,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if let Ok(mut stream) = UnixStream::connect(&self.socket) {
                let _ = serde_json::to_writer(&mut stream, &Request::new(Operation::Shutdown));
                let _ = stream.write_all(b"\n");
            }
            let _ = worker.join();
        }
    }
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "subprocess-only capture fixture with explicit private HOME"]
fn nested_peer_fixture() {
    let home = fixture_home().expect("nested fixture requires an owned private HOME");
    let token = std::env::var("FIXTURE_CAPTURE_TOKEN")
        .expect("nested fixture requires explicit capability");
    let launch_id = std::env::var("FIXTURE_CAPTURE_LAUNCH").unwrap();
    let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
    let reply = wire(
        &socket,
        Operation::HarnessEvent {
            launch_id,
            token,
            event: CaptureEvent::Bind {
                conversation_id: "nested-must-not-bind".into(),
                source: ConversationSource::ClaudeHook,
            },
        },
    );
    fs::write(
        home.join("nested-result.json"),
        serde_json::to_vec(&reply).unwrap(),
    )
    .unwrap();
    assert_eq!(reply["ok"], false);
}

#[test]
#[ignore = "subprocess-only capture fixture with explicit private HOME"]
fn managed_peer_fixture() {
    let home = fixture_home().expect("managed fixture requires an owned private HOME");
    let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
    let reply = wire(
        &socket,
        Operation::HarnessBegin {
            kind: HarnessKind::Claude,
        },
    );
    assert_eq!(reply["ok"], true, "begin: {reply}");
    let grant: CaptureGrant = serde_json::from_value(reply["result"].clone()).unwrap();
    assert_eq!(
        wire(
            &socket,
            Operation::HarnessContext {
                launch_id: grant.launch_id.clone(),
                token: "0".repeat(64)
            }
        )["ok"],
        false
    );
    let event = |event| Operation::HarnessEvent {
        launch_id: grant.launch_id.clone(),
        token: grant.token.clone(),
        event,
    };
    assert_eq!(
        wire(
            &socket,
            Operation::HarnessBegin {
                kind: HarnessKind::Claude
            }
        )["ok"],
        false
    );
    assert_eq!(
        wire(
            &socket,
            event(CaptureEvent::Bind {
                conversation_id: "wrong-source".into(),
                source: ConversationSource::OmpExtension
            })
        )["ok"],
        false
    );
    for id in ["native-one", "native-one", "native-two"] {
        assert_eq!(
            wire(
                &socket,
                event(CaptureEvent::Bind {
                    conversation_id: id.into(),
                    source: ConversationSource::ClaudeHook
                })
            )["ok"],
            true
        );
    }
    let context = || {
        wire(
            &socket,
            Operation::HarnessContext {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
            },
        )
    };
    assert_eq!(context()["result"]["session_id"], "native-two");
    let native = home.join("native/claude");
    let nested = home.join("nested/claude");
    let mut child = ChildGuard(Command::new(&native)
        .args(["-c", r#"printf '\033]7501;state=working:app=claude\007'; "$1" -c '"$1" --ignored --exact registry::capture::tests::nested_peer_fixture --nocapture --test-threads=1; result=$?; exit "$result"' nested "$2"; while [ ! -f "$HOME/release-native" ]; do sleep 0.02; done"#, "native"])
        .arg(&nested).arg(std::env::current_exe().unwrap())
        .env("FIXTURE_CAPTURE_TOKEN", &grant.token).env("FIXTURE_CAPTURE_LAUNCH", &grant.launch_id)
        .current_dir(home.join("native-cwd"))
        .spawn().unwrap());
    await_file(&home.join("nested-result.json"));
    fs::write(home.join("ready.json"), serde_json::to_vec(&json!({"launch_id":grant.launch_id,"native_pid":child.0.id(),"context":context()["result"]})).unwrap()).unwrap();
    await_file(&home.join("background"));
    let mut background = ChildGuard(
        Command::new("/bin/sh")
            .args(["-c", "exec sleep 60"])
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
    assert_eq!(unsafe { libc::tcsetpgrp(0, background.0.id() as i32) }, 0);
    assert_eq!(context()["result"]["active"], false);
    fs::write(home.join("background-ready"), b"ready").unwrap();
    await_file(&home.join("restore"));
    assert_eq!(unsafe { libc::tcsetpgrp(0, libc::getpgrp()) }, 0);
    let _ = background.0.kill();
    let _ = background.0.wait();
    // Ordinary output after reactivation must not resurrect the old engine OSC cache.
    println!("RESTORED WITHOUT NEW OSC");
    fs::write(home.join("restored"), b"ready").unwrap();
    await_file(&home.join("end"));
    assert_eq!(wire(&socket, event(CaptureEvent::End))["ok"], true);
    assert_eq!(
        wire(
            &socket,
            event(CaptureEvent::Bind {
                conversation_id: "late".into(),
                source: ConversationSource::ClaudeHook
            })
        )["ok"],
        false
    );
    let _ = child.0.kill();
    let _ = child.0.wait();
    fs::write(home.join("ended"), b"ready").unwrap();
}

struct Fixture {
    registry: Registry,
    _root: tempfile::TempDir,
    socket: PathBuf,
    home: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let socket = root.path().join("capture.sock");
        let home = root.path().join("home");
        crate::prepare_home(&home).unwrap();
        fs::write(home.join("capture-fixture"), b"owned capture fixture").unwrap();
        for dir in ["native", "nested"] {
            fs::create_dir(home.join(dir)).unwrap();
            fs::copy("/bin/sh", home.join(dir).join("claude")).unwrap();
        }
        fs::create_dir(home.join("native-cwd")).unwrap();
        Self {
            registry,
            _root: root,
            socket,
            home,
        }
    }
    fn snapshot(&self, pane_id: &str) -> PaneSnapshot {
        serde_json::from_value(
            self.registry
                .request(Request::new(Operation::Pane {
                    pane_id: pane_id.into(),
                    request: PaneRequest::Inspect,
                }))
                .unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.registry.shutdown();
    }
}

#[test]
fn authenticated_capture_bootstrap_lifecycle_and_startup_status() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let bound = crate::transport::bind(&fixture.socket).unwrap();
    registry
        .configure_harnesses(&fixture.socket, &std::env::current_exe().unwrap())
        .unwrap();
    // Deterministic observation boundaries: assertions invoke the production observation path.
    registry
        .inner
        .observer_started
        .store(true, Ordering::Release);
    let wrapper = registry
        .inner
        .spawn_environment
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .wrappers
        .join("claude");
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let request = Request::new(Operation::PaneSpawn {
        workspace_id: None, expected_revision: None,
        spec: PaneSpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), "\"$1\" --ignored --exact registry::capture::tests::managed_peer_fixture --nocapture --test-threads=1; printf RETURNED; exec sleep 60".into(), "fixture".into(), wrapper.to_str().unwrap().into()],
            cwd: fixture.home.clone(), home: fixture.home.clone(), title: None, isolated_environment: true,
            size: Size { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
        },
    });
    let spawning_registry = registry.clone();
    let spawning = thread::spawn(move || spawning_registry.request(request));
    entered_rx.recv_timeout(Duration::from_secs(8)).unwrap();
    await_file(&fixture.home.join("capture-connected"));
    // The wrapper's connect succeeded while its pane was still uncommitted.
    resume.send(()).unwrap();
    let result = spawning.join().unwrap().unwrap();
    let pane_id = result["pane_id"].as_str().unwrap().to_owned();
    // The initial wrapper has already been allowed to connect, before dispatch starts.
    let server_registry = registry.clone();
    let mut server = ServerGuard {
        socket: fixture.socket.clone(),
        worker: Some(thread::spawn(move || bound.serve(server_registry))),
    };
    await_file(&fixture.home.join("ready.json"));
    let ready: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("ready.json")).unwrap()).unwrap();
    let launch_id = ready["launch_id"].as_str().unwrap();
    let grant = registry.inner.captures.lock().unwrap().launches[launch_id]
        .grant
        .clone();
    let deadline = Instant::now() + Duration::from_secs(8);
    let before = loop {
        let snapshot = fixture.snapshot(&pane_id);
        if !snapshot.program_status.is_empty() {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "startup OSC was not published");
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        before.program_status.len(),
        1,
        "native emitted startup status only once"
    );
    assert_eq!(before.program_status[0].state, "working");
    registry.observe_processes();
    let snapshot = registry.snapshot().unwrap();
    let info = snapshot
        .panes
        .iter()
        .find(|p| p.id == pane_id)
        .unwrap()
        .harness
        .as_ref()
        .unwrap();
    assert_eq!(info.pid, ready["native_pid"].as_u64().unwrap() as u32);
    assert_eq!(info.cwd, fixture.home.join("native-cwd"));
    assert_eq!(
        ready["context"]["cwd"],
        fixture.home.join("native-cwd").to_str().unwrap(),
        "native context must not report the supervisor's original CWD"
    );
    assert_eq!(
        info.managed
            .as_ref()
            .unwrap()
            .conversation
            .as_ref()
            .unwrap()
            .generation,
        2
    );
    assert_eq!(
        snapshot
            .panes
            .iter()
            .find(|p| p.id == pane_id)
            .unwrap()
            .harness_status
            .as_ref()
            .map(|status| status.state.as_str()),
        Some("working"),
        "initial managed-child handoff must preserve the registry summary atomically"
    );
    let after = fixture.snapshot(&pane_id);
    assert_eq!(
        after.program_identity,
        Some(ProcessIdentity {
            pid: info.pid,
            birth: info.birth.clone()
        })
    );
    assert_eq!(
        after.program_status, before.program_status,
        "first native-child discovery must preserve startup OSC"
    );
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains(&grant.token));
    let durable = fs::read_to_string(fixture._root.path().join("registry.json")).unwrap();
    assert!(!durable.contains(&grant.token) && !durable.contains("native-two"));
    assert_eq!(
        wire(
            &fixture.socket,
            Operation::HarnessEvent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                event: CaptureEvent::End
            }
        )["ok"],
        false,
        "a stolen token from outside root lineage is not authority"
    );
    let launch = registry.inner.captures.lock().unwrap().launches[launch_id].clone();
    let stale = Peer {
        pid: launch.root.pid,
        uid: unsafe { libc::geteuid() },
        birth: "stale birth".into(),
    };
    assert!(harness::validate_callback(&stale, &launch).is_err());
    let current = Peer {
        birth: launch.root.identity.clone(),
        ..stale
    };
    let replaced = Launch {
        root: Process {
            identity: "reused root PID".into(),
            ..launch.root.clone()
        },
        ..launch.clone()
    };
    assert!(harness::validate_callback(&current, &replaced).is_err());
    let other = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: PaneSpawnSpec {
                argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                cwd: fixture.home.clone(),
                home: fixture.home.clone(),
                title: None,
                isolated_environment: true,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
            },
        }))
        .unwrap();
    let other_id = other["pane_id"].as_str().unwrap();
    let other_pid = fixture.snapshot(other_id).pid.unwrap() as i32;
    let other_process = proc::process(other_pid).unwrap().unwrap();
    let other_peer = Peer {
        pid: other_pid,
        uid: unsafe { libc::geteuid() },
        birth: other_process.identity,
    };
    assert!(
        registry
            .request_authenticated(
                Request::new(Operation::HarnessContext {
                    launch_id: grant.launch_id.clone(),
                    token: grant.token.clone()
                }),
                &other_peer
            )
            .is_err(),
        "another pane cannot borrow a stolen capability"
    );
    assert!(
        registry
            .request(Request::new(Operation::HarnessBegin {
                kind: HarnessKind::Claude
            }))
            .is_err()
    );
    fs::write(fixture.home.join("background"), b"go").unwrap();
    await_file(&fixture.home.join("background-ready"));
    registry.observe_processes();
    assert!(registry.snapshot().unwrap().panes[0].harness.is_none());
    assert!(
        registry.snapshot().unwrap().panes[0]
            .harness_status
            .is_none()
    );
    assert!(fixture.snapshot(&pane_id).program_status.is_empty());
    assert!(
        registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .contains_key(launch_id),
        "background retains private root authority"
    );
    fs::write(fixture.home.join("restore"), b"go").unwrap();
    await_file(&fixture.home.join("restored"));
    registry.observe_processes();
    assert!(registry.snapshot().unwrap().panes[0].harness.is_some());
    assert!(
        registry.snapshot().unwrap().panes[0]
            .harness_status
            .is_none()
    );
    assert!(
        fixture.snapshot(&pane_id).program_status.is_empty(),
        "old OSC must not resurrect after foreground return"
    );
    fs::write(fixture.home.join("end"), b"go").unwrap();
    await_file(&fixture.home.join("ended"));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        registry.observe_processes();
        if registry.snapshot().unwrap().panes[0].harness.is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "return to shell retained harness identity"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        registry.snapshot().unwrap().panes[0].status,
        PaneStatus::Running
    );
    assert_eq!(
        registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|p| p.id == pane_id)
            .unwrap()
            .foreground_cwd
            .as_ref(),
        Some(&fixture.home)
    );
    assert!(registry.inner.captures.lock().unwrap().launches.is_empty());
    assert!(fixture.snapshot(&pane_id).program_status.is_empty());
    assert_eq!(wire(&fixture.socket, Operation::Shutdown)["ok"], true);
    server.worker.take().unwrap().join().unwrap().unwrap();
}

#[test]
fn restored_foreground_facts_are_stripped_and_metadata_writes_do_not_persist_them() {
    let fixture = Fixture::new();
    let pane = PaneInfo {
        id: Uuid::new_v4().to_string(),
        workspace_id: None,
        title: "old".into(),
        status: PaneStatus::Stopped,
        exit_code: None,
        cwd: fixture.home.clone(),
        home: fixture.home.clone(),
        isolated_environment: true,
        harness_status: Some(ProgramStatus {
            state: "working".into(),
            kind: None,
            app: Some("claude".into()),
            message: Some("private-status-must-not-persist".into()),
            id: String::new(),
            title: String::new(),
            progress: None,
        }),
        foreground_cwd: Some(fixture.home.clone()),
        harness: Some(HarnessInfo {
            kind: HarnessKind::Claude,
            pid: 123,
            birth: "stale".into(),
            cwd: fixture.home.clone(),
            managed: Some(ManagedHarness {
                launch_id: "not-live".into(),
                conversation: Some(NativeConversation {
                    id: "private-native-id".into(),
                    source: ConversationSource::ClaudeHook,
                    generation: 1,
                }),
                error: None,
            }),
        }),
    };
    fixture
        .registry
        .mutate(|r| {
            r.panes.push(pane.clone());
            Ok(())
        })
        .unwrap();
    let bytes = fs::read_to_string(fixture._root.path().join("registry.json")).unwrap();
    assert!(!bytes.contains("private-status-must-not-persist"));
    assert!(!bytes.contains("private-native-id") && !bytes.contains("not-live"));
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    // Simulate older/untrusted on-disk live facts: reopening never adopts them.
    let record = Record {
        version: STORAGE_VERSION,
        registry: RegistrySnapshot {
            revision: 1,
            workspaces: Vec::new(),
            panes: vec![pane],
        },
    };
    fs::write(
        root.path().join("registry.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        root.path().join("registry.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let reopened = Registry::open(root.path()).unwrap();
    assert!(reopened.snapshot().unwrap().panes[0].harness.is_none());
    assert!(
        reopened.snapshot().unwrap().panes[0]
            .foreground_cwd
            .is_none()
    );
    assert!(
        reopened.snapshot().unwrap().panes[0]
            .harness_status
            .is_none()
    );
    assert!(
        !fs::read_to_string(root.path().join("registry.json"))
            .unwrap()
            .contains("private-native-id")
    );
}

#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

#[test]
fn non_utf8_foreground_cwd_round_trips_without_poisoning_or_persisting_live_facts() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let fixture = Fixture::new();
    fixture
        .registry
        .inner
        .observer_started
        .store(true, Ordering::Release);
    let raw = fixture
        .home
        .join(std::ffi::OsString::from_vec(b"directory-\xff".to_vec()));
    fs::create_dir(&raw).unwrap();
    symlink(&raw, fixture.home.join("destination")).unwrap();
    let result = fixture
        .registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: PaneSpawnSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "cd destination; exec sleep 60".into(),
                ],
                cwd: fixture.home.clone(),
                home: fixture.home.clone(),
                title: None,
                isolated_environment: true,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
            },
        }))
        .unwrap();
    let id = result["pane_id"].as_str().unwrap();
    let durable = fs::read(fixture._root.path().join("registry.json")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        fixture.registry.observe_processes();
        if fixture.registry.snapshot().unwrap().panes[0]
            .foreground_cwd
            .as_ref()
            == Some(&raw)
        {
            break;
        }
        assert!(Instant::now() < deadline, "non-UTF8 cwd was not observed");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        durable,
        "live observations must not write the durable file"
    );
    for operation in [
        Operation::Inspect,
        Operation::Wait {
            after: fixture.registry.snapshot().unwrap().revision,
            wait_ms: 0,
        },
    ] {
        let value = fixture.registry.request(Request::new(operation)).unwrap();
        let snapshot: RegistryResult = serde_json::from_value(value).unwrap();
        assert_eq!(
            snapshot
                .registry
                .panes
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .foreground_cwd
                .as_ref(),
            Some(&raw)
        );
    }
    fixture
        .registry
        .request(Request::new(Operation::WorkspaceCreate {
            name: "still healthy".into(),
        }))
        .unwrap();
    assert!(
        !fs::read_to_string(fixture._root.path().join("registry.json"))
            .unwrap()
            .contains("unix_bytes")
    );
}

#[path = "status_tests.rs"]
mod status_tests;
