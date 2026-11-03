use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Child, Command, Stdio},
    thread,
    time::Instant,
};

const ROOT_PEER: &str = "registry::context::tests::lineage_peer_fixture";
const CHILD_PEER: &str = "registry::context::tests::holding_peer_fixture";
const IMMEDIATE_PEER: &str = "registry::context::tests::immediate_context_peer_fixture";

fn size() -> Size {
    Size {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    }
}

fn wait_for<T>(description: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn await_bytes(path: &Path, bytes: &[u8]) {
    wait_for(&path.display().to_string(), || {
        (fs::read(path).ok().as_deref() == Some(bytes)).then_some(())
    });
}

fn json_file(path: &Path) -> Value {
    wait_for(&path.display().to_string(), || {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    })
}

fn publish_json(home: &Path, name: &str, value: &Value) {
    let temporary = home.join(format!("{name}.pending"));
    fs::write(&temporary, serde_json::to_vec(value).unwrap()).unwrap();
    fs::rename(temporary, home.join(name)).unwrap();
}

fn call(registry: &Registry, operation: Operation) -> Value {
    registry.request(Request::new(operation)).unwrap()
}

fn peer(pid: i32) -> Peer {
    let process = proc::process(pid).unwrap().expect("owned peer is live");
    Peer {
        pid,
        uid: unsafe { libc::geteuid() },
        birth: process.identity,
    }
}

fn selected(registry: &Registry, peer: &Peer, pane_id: Option<&str>) -> Result<PaneContext> {
    let value = registry.request_authenticated(
        Request::new(Operation::Context {
            pane_id: pane_id.map(str::to_owned),
        }),
        peer,
    )?;
    ensure!(
        value.as_object().is_some_and(|fields| fields.len() == 8),
        "unexpected context fields"
    );
    Ok(serde_json::from_value(value)?)
}

fn native_spec(home: &Path) -> PaneSpawnSpec {
    PaneSpawnSpec {
        argv: vec![
            "/bin/sh".into(), "-c".into(),
            "BKPR_PANE_ID=spoofed-pane; BKPR_SESSION_ID=spoofed-session; export BKPR_PANE_ID BKPR_SESSION_ID; printf ready > ready; exec sleep 60".into(),
        ],
        cwd: home.to_owned(), home: home.to_owned(), size: size(),
        title: Some("native title".into()), isolated_environment: true,
    }
}

struct Fixture {
    registry: Registry,
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open_named(root.path(), "context session").unwrap();
        // Keep observation scheduling out of query immutability assertions.
        registry
            .inner
            .observer_started
            .store(true, Ordering::Release);
        Self { registry, root }
    }

    fn home(&self, name: &str) -> PathBuf {
        let home = self.root.path().join(name);
        crate::prepare_home(&home).unwrap();
        fs::write(home.join("context-fixture"), b"owned context fixture").unwrap();
        home
    }

    fn standalone(&self, name: &str) -> String {
        let home = self.home(name);
        let value = call(
            &self.registry,
            Operation::PaneSpawn {
                workspace_id: None,
                window_id: None,
                expected_revision: None,
                spec: native_spec(&home),
            },
        );
        await_bytes(&home.join("ready"), b"ready");
        value["pane_id"].as_str().unwrap().to_owned()
    }

    fn workspace(&self, name: &str) -> (String, String, String) {
        let home = self.home(name);
        let value = call(
            &self.registry,
            Operation::WorkspaceCreate {
                name: Some(name.into()),
                initial: Some(native_spec(&home)),
            },
        );
        await_bytes(&home.join("ready"), b"ready");
        (
            value["workspace_id"].as_str().unwrap().to_owned(),
            value["window_id"].as_str().unwrap().to_owned(),
            value["pane_id"].as_str().unwrap().to_owned(),
        )
    }

    fn peer(&self, id: &str) -> Peer {
        let pane = self.registry.inner.state.lock().unwrap().live[id].clone();
        let root = pane.shared.root.lock().unwrap().clone().unwrap();
        peer(root.pid)
    }

    fn workspace_revision(&self, id: &str) -> u64 {
        self.registry
            .snapshot()
            .unwrap()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap()
            .revision
    }

    fn window_revision(&self, workspace_id: &str, window_id: &str) -> u64 {
        self.registry
            .snapshot()
            .unwrap()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .unwrap()
            .windows
            .iter()
            .find(|window| window.id == window_id)
            .unwrap()
            .revision
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.registry.shutdown();
    }
}

#[test]
fn context_uses_real_peer_or_exact_explicit_pane_and_is_read_only() {
    let fixture = Fixture::new();
    let (workspace, window, a) = fixture.workspace("owner");
    let b = fixture.standalone("unowned");
    let peer_a = fixture.peer(&a);
    let before = serde_json::to_value(fixture.registry.snapshot().unwrap()).unwrap();
    let durable = fs::read(fixture.root.path().join("registry.json")).unwrap();
    let terminal = || {
        call(
            &fixture.registry,
            Operation::Pane {
                pane_id: a.clone(),
                request: PaneRequest::Inspect,
            },
        )
    };
    let before_terminal = terminal();
    let auto = selected(&fixture.registry, &peer_a, None).unwrap();
    assert_eq!(auto.pane_id, a);
    assert_eq!(auto.workspace_id.as_deref(), Some(workspace.as_str()));
    assert_eq!(auto.window_id.as_deref(), Some(window.as_str()));
    assert_eq!(auto.session, fixture.registry.inner.session);
    let explicit = selected(&fixture.registry, &peer_a, Some(&b)).unwrap();
    assert_eq!(explicit.pane_id, b);
    assert!(explicit.workspace_id.is_none() && explicit.workspace_name.is_none());
    assert!(explicit.window_id.is_none() && explicit.window_name.is_none());
    assert!(
        fixture
            .registry
            .request(Request::new(Operation::Context { pane_id: None }))
            .is_err()
    );
    assert!(selected(&fixture.registry, &peer_a, Some("unknown-pane")).is_err());
    assert_eq!(
        before,
        serde_json::to_value(fixture.registry.snapshot().unwrap()).unwrap()
    );
    assert_eq!(
        durable,
        fs::read(fixture.root.path().join("registry.json")).unwrap()
    );
    assert_eq!(before_terminal, terminal());
    assert!(
        fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .is_empty()
    );

    // Another session's actual native PTY cannot select this registry by ancestry
    // or environment, but same-user explicit metadata selection remains allowed.
    let foreign = Fixture::new();
    let foreign_pane = foreign.standalone("foreign-native");
    let foreign_peer = foreign.peer(&foreign_pane);
    assert!(selected(&fixture.registry, &foreign_peer, None).is_err());
    let explicit = selected(&fixture.registry, &foreign_peer, Some(&a)).unwrap();
    assert_eq!(explicit.pane_id, a);
}

#[test]
fn live_labels_and_execution_membership_ignore_projection_removal_and_borrowed_views() {
    let fixture = Fixture::new();
    let (workspace, window, pane) = fixture.workspace("original owner");
    let (other_workspace, other_window, _) = fixture.workspace("other owner");
    let peer = fixture.peer(&pane);
    for _ in 0..2 {
        call(
            &fixture.registry,
            Operation::Pane {
                pane_id: pane.clone(),
                request: PaneRequest::Attach {
                    spec: ViewSpec {
                        size: size(),
                        visible: true,
                        interactive: false,
                        input: false,
                        clipboard: false,
                    },
                },
            },
        );
    }
    assert!(
        fixture
            .registry
            .request(Request::new(Operation::LayoutUpdate {
                workspace_id: other_workspace.clone(),
                window_id: other_window.clone(),
                expected_revision: fixture.window_revision(&other_workspace, &other_window),
                layout: Some(LayoutNode::Leaf {
                    pane_id: pane.clone()
                }),
            }))
            .is_err()
    );
    call(
        &fixture.registry,
        Operation::WorkspaceRename {
            workspace_id: workspace.clone(),
            expected_revision: fixture.workspace_revision(&workspace),
            name: "current owner".into(),
        },
    );
    call(
        &fixture.registry,
        Operation::WindowRename {
            workspace_id: workspace.clone(),
            window_id: window.clone(),
            expected_revision: fixture.window_revision(&workspace, &window),
            name: "current window".into(),
        },
    );
    call(
        &fixture.registry,
        Operation::PaneRename {
            pane_id: pane.clone(),
            title: "current pane".into(),
        },
    );
    call(
        &fixture.registry,
        Operation::LayoutUpdate {
            workspace_id: workspace.clone(),
            window_id: window.clone(),
            expected_revision: fixture.window_revision(&workspace, &window),
            layout: None,
        },
    );
    let context = selected(&fixture.registry, &peer, None).unwrap();
    assert_eq!(context.pane_id, pane);
    assert_eq!(context.pane_title, "current pane");
    assert_eq!(context.workspace_id.as_deref(), Some(workspace.as_str()));
    assert_eq!(context.workspace_name.as_deref(), Some("current owner"));
    assert_eq!(context.window_id.as_deref(), Some(window.as_str()));
    assert_eq!(context.window_name.as_deref(), Some("current window"));
    assert_eq!(
        context.revision,
        fixture.registry.snapshot().unwrap().revision
    );
    assert_eq!(
        selected(&fixture.registry, &peer, Some(&pane)).unwrap(),
        context
    );
    call(
        &fixture.registry,
        Operation::WindowClose {
            workspace_id: workspace.clone(),
            window_id: window.clone(),
            expected_revision: fixture.window_revision(&workspace, &window),
        },
    );
    assert!(selected(&fixture.registry, &peer, Some(&pane)).is_err());
}

#[test]
fn closing_documents_reject_context_before_their_durable_metadata_commit() {
    let fixture = Fixture::new();
    let (workspace, window, pane) = fixture.workspace("closing owner");
    let peer = fixture.peer(&pane);
    let revision = fixture.window_revision(&workspace, &window);
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *fixture.registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = Release(Some(resume));
    let registry = fixture.registry.clone();
    let closing = thread::spawn(move || {
        registry.request(Request::new(Operation::WindowClose {
            workspace_id: workspace,
            window_id: window,
            expected_revision: revision,
        }))
    });
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let before_commit = fixture.registry.snapshot().unwrap();
    let rejected = selected(&fixture.registry, &peer, Some(&pane))
        .unwrap_err()
        .to_string();
    drop(release);
    closing.join().unwrap().unwrap();
    blocked.unwrap();
    assert!(
        before_commit
            .panes
            .iter()
            .any(|info| info.id == pane && info.status == PaneStatus::Running)
    );
    assert!(
        rejected.contains("closing"),
        "unexpected closing rejection: {rejected}"
    );
}

fn fixture_home() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").expect("explicit private fixture HOME"));
    assert_eq!(
        fs::read(home.join("context-fixture")).unwrap(),
        b"owned context fixture"
    );
    home
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child_command(home: &Path) -> Command {
    crate::prepare_home(home).unwrap();
    fs::write(home.join("context-fixture"), b"owned context fixture").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            CHILD_PEER,
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, directory) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        command.env(key, home.join(directory));
    }
    command
}

#[test]
#[ignore = "private native subprocess fixture, invoked by context provenance regression"]
fn holding_peer_fixture() {
    let home = fixture_home();
    fs::write(home.join("ready"), b"ready").unwrap();
    await_bytes(&home.join("release"), b"release");
}

#[test]
#[ignore = "private native subprocess fixture, invoked by context provenance regression"]
fn lineage_peer_fixture() {
    let home = fixture_home();
    let background_home = home.join("background");
    let background = OwnedChild(
        child_command(&background_home)
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let detached_home = home.join("detached");
    let mut detached = child_command(&detached_home);
    unsafe {
        detached.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let detached = OwnedChild(detached.spawn().unwrap());
    let nested_home = home.join("nested");
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let _master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut nested = child_command(&nested_home);
    nested
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        nested.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let nested = OwnedChild(nested.spawn().unwrap());
    for directory in [&background_home, &detached_home, &nested_home] {
        await_bytes(&directory.join("ready"), b"ready");
    }
    publish_json(
        &home,
        "children.json",
        &json!({
            "background": background.0.id(), "detached": detached.0.id(), "nested": nested.0.id(),
        }),
    );
    await_bytes(&home.join("release"), b"release");
}

#[test]
fn peer_birth_and_controlling_pty_reject_stale_foreign_detached_and_nested_peers() {
    let fixture = Fixture::new();
    let home = fixture.home("lineage");
    let mut spec = native_spec(&home);
    spec.argv = vec![
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        "--ignored".into(),
        "--exact".into(),
        ROOT_PEER.into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ];
    let value = call(
        &fixture.registry,
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            spec,
        },
    );
    let pane = value["pane_id"].as_str().unwrap();
    let children = json_file(&home.join("children.json"));
    let root_peer = fixture.peer(pane);
    let background = peer(children["background"].as_i64().unwrap() as i32);
    let process = background.current().unwrap();
    assert_ne!(
        process.group, process.foreground,
        "fixture must really be a background job"
    );
    assert_eq!(
        selected(&fixture.registry, &background, None)
            .unwrap()
            .pane_id,
        pane
    );
    let detached = peer(children["detached"].as_i64().unwrap() as i32);
    assert_eq!(detached.current().unwrap().tty, 0);
    assert!(selected(&fixture.registry, &detached, None).is_err());
    let nested = peer(children["nested"].as_i64().unwrap() as i32);
    let root_process = root_peer.current().unwrap();
    let nested_process = nested.current().unwrap();
    assert_ne!(root_process.tty, nested_process.tty);
    assert!(harness::ancestry(&nested_process, &root_process).is_ok());
    assert!(selected(&fixture.registry, &nested, None).is_err());
    assert!(
        selected(
            &fixture.registry,
            &Peer {
                birth: "stale birth".into(),
                ..peer(background.pid)
            },
            None
        )
        .is_err()
    );
    assert!(
        selected(
            &fixture.registry,
            &Peer {
                uid: unsafe { libc::geteuid() }.wrapping_add(1),
                ..peer(background.pid)
            },
            None
        )
        .is_err()
    );
    assert_eq!(unsafe { libc::kill(background.pid, libc::SIGSTOP) }, 0);
    wait_for("stopped background peer", || {
        background.current().ok().filter(|process| process.stopped)
    });
    let stopped = selected(&fixture.registry, &background, None).is_err();
    assert_eq!(unsafe { libc::kill(background.pid, libc::SIGCONT) }, 0);
    assert!(stopped);
    assert_eq!(unsafe { libc::kill(root_peer.pid, libc::SIGSTOP) }, 0);
    wait_for("stopped native root", || {
        root_peer.current().ok().filter(|process| process.stopped)
    });
    let stopped = selected(&fixture.registry, &background, Some(pane)).is_err();
    assert_eq!(unsafe { libc::kill(root_peer.pid, libc::SIGCONT) }, 0);
    assert!(stopped);
    fs::write(home.join("release"), b"release").unwrap();
}

#[test]
fn contexts_fail_closed_after_termination_shutdown_or_durability_poisoning() {
    let fixture = Fixture::new();
    let pane = fixture.standalone("terminated");
    let peer = fixture.peer(&pane);
    call(
        &fixture.registry,
        Operation::PaneTerminate {
            pane_id: pane.clone(),
        },
    );
    assert!(selected(&fixture.registry, &peer, Some(&pane)).is_err());
    let pane = fixture.standalone("poisoned");
    let peer = fixture.peer(&pane);
    fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .durability_error = Some("owned fixture persistence failure".into());
    assert!(selected(&fixture.registry, &peer, None).is_err());
    assert!(selected(&fixture.registry, &peer, Some(&pane)).is_err());
    fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .durability_error = None;
    fixture.registry.shutdown().unwrap();
    assert!(selected(&fixture.registry, &peer, Some(&pane)).is_err());
}

fn wire(socket: &Path, operation: Operation) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
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

struct Server {
    socket: PathBuf,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
impl Drop for Server {
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

struct Release(Option<std::sync::mpsc::SyncSender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}

#[test]
#[ignore = "private native root subprocess fixture, invoked by startup context regression"]
fn immediate_context_peer_fixture() {
    let home = fixture_home();
    let socket = PathBuf::from(fs::read_to_string(home.join("socket")).unwrap());
    assert_eq!(std::env::var("BKPR_PANE_ID").unwrap(), "spoofed-pane");
    let response = wire(&socket, Operation::Context { pane_id: None });
    publish_json(&home, "context.json", &response);
    await_bytes(&home.join("release"), b"release");
}

#[test]
fn immediate_root_context_waits_for_real_publication_without_retry_or_focus() {
    let fixture = Fixture::new();
    let home = fixture.home("immediate");
    let socket = fixture.root.path().join("context.sock");
    fs::write(home.join("socket"), socket.to_str().unwrap()).unwrap();
    let bound = crate::transport::bind(&socket).unwrap();
    let registry = fixture.registry.clone();
    let _server = Server {
        socket,
        worker: Some(thread::spawn(move || bound.serve(registry))),
    };
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *fixture.registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let release = Release(Some(resume));
    let (admitted, admitted_rx) = std::sync::mpsc::sync_channel(1);
    *fixture
        .registry
        .inner
        .capture_admission_wait
        .lock()
        .unwrap() = Some(admitted);
    let mut spec = native_spec(&home);
    spec.argv = vec![
        "/bin/sh".into(), "-c".into(),
        "BKPR_PANE_ID=spoofed-pane BKPR_SESSION_ID=spoofed-session exec \"$1\" --ignored --exact \"$2\" --nocapture --test-threads=1".into(),
        "context-root".into(), std::env::current_exe().unwrap().to_str().unwrap().into(), IMMEDIATE_PEER.into(),
    ];
    let registry = fixture.registry.clone();
    let spawning = thread::spawn(move || {
        registry.request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            spec,
        }))
    });
    let publication_blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let context_waiting = admitted_rx.recv_timeout(Duration::from_secs(8));
    let premature = home.join("context.json").exists();
    drop(release);
    let spawned = spawning.join().unwrap().unwrap();
    publication_blocked.unwrap();
    context_waiting.unwrap();
    assert!(
        !premature,
        "pending publication must not manufacture a successful context"
    );
    let response = json_file(&home.join("context.json"));
    assert_eq!(response["ok"], true, "one-shot root query: {response}");
    let context: PaneContext = serde_json::from_value(response["result"].clone()).unwrap();
    assert_eq!(context.pane_id, spawned["pane_id"].as_str().unwrap());
    assert_eq!(context.session, fixture.registry.inner.session);
    fs::write(home.join("release"), b"release").unwrap();
}
