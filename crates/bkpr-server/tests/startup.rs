use bkpr_protocol::{
    LayoutNode, Operation, PaneRequest, PaneSpawnSpec, PaneStatus, RegistryResult,
    RegistrySnapshot, Request, Response, Size,
};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    socket: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("bk").tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        let home = root.path().join("home");
        let cwd = root.path().join("cwd");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&cwd).unwrap();
        let socket = root.path().join("sock");
        Self {
            _root: root,
            state,
            home,
            cwd,
            socket,
        }
    }
    fn command(&self, argv: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bkprd"));
        command
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("HOME", &self.home)
            .env("TERM", "xterm-256color")
            .arg("--socket")
            .arg(&self.socket)
            .arg("--state-root")
            .arg(&self.state)
            .arg("--bkpr")
            .arg(std::env::current_exe().unwrap())
            .arg("--home")
            .arg(&self.home)
            .arg("--cwd")
            .arg(&self.cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if !argv.is_empty() {
            command.arg("--").args(argv);
        }
        command
    }
    fn start(&self, argv: &[&str]) -> Daemon {
        let child = self.command(argv).spawn().unwrap();
        let mut daemon = Daemon {
            child,
            socket: self.socket.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(snapshot) = request(&self.socket, Operation::Inspect) {
                assert!(snapshot.ok);
                return daemon;
            }
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "owned test daemon exited during startup"
            );
            assert!(
                Instant::now() < deadline,
                "owned test daemon did not become available"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
struct Daemon {
    child: Child,
    socket: PathBuf,
}
impl Daemon {
    fn send(&self, operation: Operation) -> serde_json::Value {
        let response = request(&self.socket, operation).unwrap();
        assert!(
            response.ok,
            "daemon rejected fixture request: {:?}",
            response.error
        );
        response.result.unwrap()
    }
    fn snapshot(&self) -> RegistrySnapshot {
        let response: RegistryResult =
            serde_json::from_value(self.send(Operation::Inspect)).unwrap();
        response.registry
    }
    fn stop(&mut self) {
        self.send(Operation::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "owned test daemon did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "session-end daemon exit: {status}");
                assert!(!self.socket.exists(), "session-end socket survived");
                return;
            }
            assert!(Instant::now() < deadline, "session-end did not stop daemon");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = request(&self.socket, Operation::Shutdown);
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn request(socket: &Path, operation: Operation) -> anyhow::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(&mut stream, &Request::new(operation))?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
fn wait_for(path: &Path, bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while fs::read(path).ok().as_deref() != Some(bytes) {
        assert!(
            Instant::now() < deadline,
            "explicit child did not write {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn historical(fixture: &Fixture, daemon: &Daemon) -> (String, String) {
    let created = daemon.send(Operation::WorkspaceCreate {
        name: Some("Main".into()),
        initial: Some(PaneSpawnSpec {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf original >> original-runs; exec sleep 60".into(),
            ],
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            size: Size {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16,
            },
            title: Some("old explicit command".into()),
            isolated_environment: true,
        }),
    });
    let workspace = created["workspace_id"].as_str().unwrap().to_owned();
    let pane = created["pane_id"].as_str().unwrap().to_owned();
    wait_for(&fixture.cwd.join("original-runs"), b"original");
    (workspace, pane)
}
fn workspaces(snapshot: &RegistrySnapshot) -> serde_json::Value {
    serde_json::to_value(&snapshot.workspaces).unwrap()
}

#[test]
fn explicit_fresh_bootstrap_creates_a_usable_pane_and_second_daemon_cannot_reset_it() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start(&["/bin/sh", "-c", "printf fresh > fresh; exec sleep 60"]);
    wait_for(&fixture.cwd.join("fresh"), b"fresh");
    let before = daemon.snapshot();
    assert_eq!(before.workspaces.len(), 1);
    assert_eq!(before.workspaces[0].name, "Main");
    assert_eq!(before.panes.len(), 1);
    assert_eq!(before.panes[0].status, PaneStatus::Running);
    let pane = &before.panes[0];
    assert_eq!(pane.cwd, fixture.cwd);
    assert_eq!(pane.home, fixture.home);
    let scene = daemon.send(Operation::Pane {
        pane_id: pane.id.clone(),
        request: PaneRequest::Inspect,
    });
    assert_eq!(scene["lifecycle"], "running");
    let duplicate = fixture
        .command(&[
            "/bin/sh",
            "-c",
            "printf replayed > duplicate; exec sleep 60",
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(!fixture.cwd.join("duplicate").exists());
    let after = daemon.snapshot();
    assert_eq!(workspaces(&after), workspaces(&before));
    assert_eq!(after.panes.len(), before.panes.len());
    assert_eq!(after.panes[0].id, pane.id);
    daemon.stop();
}

#[test]
fn headless_fresh_and_loaded_startup_never_spawns_or_replays_history() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    assert!(first.snapshot().workspaces.is_empty());
    assert!(first.snapshot().panes.is_empty());
    let (_, pane) = historical(&fixture, &first);
    let before = first.snapshot();
    first.stop();
    let mut second = fixture.start(&[]);
    let restored = second.snapshot();
    assert_eq!(workspaces(&restored), workspaces(&before));
    assert_eq!(restored.panes.len(), before.panes.len());
    assert_eq!(restored.panes[0].id, pane);
    assert_eq!(restored.panes[0].status, PaneStatus::Stopped);
    assert!(
        !restored
            .panes
            .iter()
            .any(|pane| pane.status == PaneStatus::Running)
    );
    assert_eq!(
        fs::read(fixture.cwd.join("original-runs")).unwrap(),
        b"original"
    );
    second.stop();
}

#[test]
fn explicit_loaded_bootstrap_preserves_history_and_adds_uniquely_named_current_execution() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    let (workspace, pane) = historical(&fixture, &first);
    first.send(Operation::WorkspaceCreate {
        name: Some("Main 2".into()),
        initial: None,
    });
    let before = first.snapshot();
    first.stop();
    let mut restored = fixture.start(&["/bin/sh", "-c", "printf current > current; exec sleep 60"]);
    wait_for(&fixture.cwd.join("current"), b"current");
    let after = restored.snapshot();
    assert_eq!(after.workspaces.len(), before.workspaces.len() + 1);
    assert_eq!(after.panes.len(), before.panes.len() + 1);
    for old in &before.workspaces {
        assert_eq!(
            serde_json::to_value(after.workspaces.iter().find(|w| w.id == old.id).unwrap())
                .unwrap(),
            serde_json::to_value(old).unwrap()
        );
    }
    let history = after.panes.iter().find(|p| p.id == pane).unwrap();
    assert_eq!(history.status, PaneStatus::Stopped);
    assert_eq!(history.title, "old explicit command");
    assert_eq!(history.workspace_id.as_deref(), Some(workspace.as_str()));
    let live = after
        .panes
        .iter()
        .filter(|p| p.status == PaneStatus::Running)
        .collect::<Vec<_>>();
    assert_eq!(live.len(), 1);
    assert_ne!(live[0].id, pane);
    let boot = after
        .workspaces
        .iter()
        .find(|w| Some(w.id.as_str()) == live[0].workspace_id.as_deref())
        .unwrap();
    assert_eq!(boot.name, "Main 3");
    assert_eq!(
        boot.windows[0].layout,
        Some(LayoutNode::Leaf {
            pane_id: live[0].id.clone()
        })
    );
    assert_eq!(
        fs::read(fixture.cwd.join("original-runs")).unwrap(),
        b"original"
    );
    restored.stop();
}

#[test]
fn graceful_shutdown_preserves_exact_layout_intent_for_fresh_execution_reopen() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    let (workspace, old) = historical(&fixture, &first);
    let initial = first.snapshot();
    let window = initial
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .unwrap()
        .windows[0]
        .clone();
    let spec = PaneSpawnSpec {
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf reopened > reopened; exec sleep 60".into(),
        ],
        cwd: fixture.cwd.clone(),
        home: fixture.home.clone(),
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        title: Some("fresh shell".into()),
        isolated_environment: true,
    };
    let other = first.send(Operation::PaneSpawn {
        workspace_id: Some(workspace.clone()),
        window_id: Some(window.id.clone()),
        expected_revision: Some(window.revision),
        spec: spec.clone(),
    })["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for(&fixture.cwd.join("reopened"), b"reopened");
    let historical_layout = LayoutNode::Split {
        axis: bkpr_protocol::Axis::Vertical,
        ratio: 317,
        first: Box::new(LayoutNode::Leaf {
            pane_id: old.clone(),
        }),
        second: Box::new(LayoutNode::Leaf {
            pane_id: other.clone(),
        }),
    };
    first.send(Operation::LayoutUpdate {
        workspace_id: workspace.clone(),
        window_id: window.id.clone(),
        expected_revision: window.revision + 1,
        layout: Some(historical_layout.clone()),
    });
    // A distinct owned window remains unchanged when reopening this window.
    let second = first.send(Operation::WindowCreate {
        workspace_id: workspace.clone(),
        expected_revision: first
            .snapshot()
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .revision,
        name: Some("independent".into()),
        spec: spec.clone(),
    });
    let independent = second["window_id"].as_str().unwrap().to_owned();
    let before = first.snapshot();
    assert!(
        before
            .panes
            .iter()
            .all(|pane| pane.status == PaneStatus::Running)
    );
    first.stop();

    let mut restored = fixture.start(&[]);
    let stopped = restored.snapshot();
    assert_eq!(workspaces(&stopped), workspaces(&before));
    assert_eq!(stopped.panes.len(), before.panes.len());
    assert!(
        stopped
            .panes
            .iter()
            .all(|pane| pane.status == PaneStatus::Stopped)
    );
    assert!(
        !request(
            &fixture.socket,
            Operation::Pane {
                pane_id: old.clone(),
                request: PaneRequest::Inspect,
            },
        )
        .unwrap()
        .ok
    );
    let selected_workspace = stopped
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .unwrap();
    let revision = selected_workspace
        .windows
        .iter()
        .find(|w| w.id == window.id)
        .unwrap()
        .revision;
    let historical_exit_code = stopped
        .panes
        .iter()
        .find(|p| p.id == old)
        .unwrap()
        .exit_code;
    let peer = serde_json::to_value(
        selected_workspace
            .windows
            .iter()
            .find(|w| w.id == independent)
            .unwrap(),
    )
    .unwrap();
    fs::remove_file(fixture.cwd.join("reopened")).unwrap();
    let reopened: RegistryResult = serde_json::from_value(restored.send(Operation::PaneReopen {
        workspace_id: workspace.clone(),
        window_id: window.id.clone(),
        expected_revision: revision,
        pane_id: old.clone(),
        spec,
    }))
    .unwrap();
    let new = reopened.pane_id.unwrap();
    assert_ne!(new, old);
    assert_ne!(new, other);
    wait_for(&fixture.cwd.join("reopened"), b"reopened");
    let selected = reopened
        .registry
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .unwrap();
    let selected_window = selected.windows.iter().find(|w| w.id == window.id).unwrap();
    assert_eq!(selected_window.revision, revision + 1);
    assert_eq!(
        selected_window.layout,
        Some(LayoutNode::Split {
            axis: bkpr_protocol::Axis::Vertical,
            ratio: 317,
            first: Box::new(LayoutNode::Leaf {
                pane_id: new.clone()
            }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: other.clone()
            }),
        })
    );
    let historical = reopened
        .registry
        .panes
        .iter()
        .find(|p| p.id == old)
        .unwrap();
    assert_eq!(historical.exit_code, historical_exit_code);
    assert!(historical.workspace_id.is_none() && historical.window_id.is_none());
    assert_eq!(
        serde_json::to_value(
            reopened
                .registry
                .workspaces
                .iter()
                .find(|w| w.id == workspace)
                .unwrap()
                .windows
                .iter()
                .find(|w| w.id == independent)
                .unwrap()
        )
        .unwrap(),
        peer
    );
    assert_eq!(
        fs::read(fixture.cwd.join("original-runs")).unwrap(),
        b"original"
    );
    let live = reopened
        .registry
        .panes
        .iter()
        .filter(|p| p.status == PaneStatus::Running)
        .collect::<Vec<_>>();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, new);

    // Explicit termination removes only the selected window's owned position.
    restored.send(Operation::PaneTerminate { pane_id: new });
    let after_termination = restored.snapshot();
    assert_eq!(
        after_termination
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .windows
            .iter()
            .find(|w| w.id == window.id)
            .unwrap()
            .layout,
        Some(LayoutNode::Leaf { pane_id: other })
    );
    assert_eq!(
        serde_json::to_value(
            after_termination
                .workspaces
                .iter()
                .find(|w| w.id == workspace)
                .unwrap()
                .windows
                .iter()
                .find(|w| w.id == independent)
                .unwrap()
        )
        .unwrap(),
        peer
    );
    restored.stop();
}

fn sleeping_spec(fixture: &Fixture) -> PaneSpawnSpec {
    PaneSpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
        cwd: fixture.cwd.clone(),
        home: fixture.home.clone(),
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
fn passive_last_shell_exit_stops_daemon_without_inspect_or_wait_and_other_session_survives() {
    let fixture = Fixture::new();
    let other_fixture = Fixture::new();
    let mut other = other_fixture.start(&[]);
    let mut daemon = fixture.start(&[
        "/bin/sh",
        "-c",
        "printf ready > ready; while test ! -f release; do sleep 0.02; done",
    ]);
    wait_for(&fixture.cwd.join("ready"), b"ready");
    // No control requests after releasing the last shell.
    fs::write(fixture.cwd.join("release"), b"exit").unwrap();
    daemon.wait_for_exit();
    assert!(other.child.try_wait().unwrap().is_none());
    assert!(other.snapshot().workspaces.is_empty());
    // The socket and metadata lease are both reusable after normal return.
    let mut restarted = fixture.start(&[]);
    restarted.stop();
    other.stop();
}

#[test]
fn last_explicit_workspace_close_notifies_lifetime_and_reaps_standalone_without_ack() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start(&["/bin/sh", "-c", "exec sleep 60"]);
    let snapshot = daemon.snapshot();
    let identity = daemon.send(Operation::SessionInfo);
    let mut lifetime = UnixStream::connect(&fixture.socket).unwrap();
    lifetime
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    serde_json::to_writer(
        &mut lifetime,
        &Request::new(Operation::ClientRegister {
            session_id: identity["session"]["session_id"].as_str().unwrap().into(),
            daemon_id: identity["session"]["daemon_id"].as_str().unwrap().into(),
        }),
    )
    .unwrap();
    lifetime.write_all(b"\n").unwrap();
    let mut lifetime = BufReader::new(lifetime);
    let mut line = String::new();
    lifetime.read_line(&mut line).unwrap();
    let registered: Response = serde_json::from_str(&line).unwrap();
    assert!(registered.ok, "{:?}", registered.error);
    let standalone = daemon.send(Operation::PaneSpawn {
        workspace_id: None,
        window_id: None,
        expected_revision: None,
        spec: sleeping_spec(&fixture),
    });
    let standalone_scene = daemon.send(Operation::Pane {
        pane_id: standalone["pane_id"].as_str().unwrap().into(),
        request: PaneRequest::Inspect,
    });
    let workspace = &snapshot.workspaces[0];
    let closed = request(
        &fixture.socket,
        Operation::WorkspaceClose {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
        },
    )
    .unwrap();
    assert!(closed.ok, "{:?}", closed.error);
    // Keep the lifetime open and send no acknowledgement. Shutdown cannot wait
    // for client participation, or for this reader to drain its event.
    daemon.wait_for_exit();
    line.clear();
    lifetime.read_line(&mut line).unwrap();
    let event: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(event["event"], "client_detached");
    assert_eq!(event["reason"], "shutdown");
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc")
            .join(standalone_scene["pid"].as_i64().unwrap().to_string())
            .exists()
    );
    #[cfg(not(target_os = "linux"))]
    let _ = standalone_scene;
}

#[test]
fn administrative_documents_detach_and_layout_removal_do_not_end_session() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start(&[]);
    daemon.send(Operation::WorkspaceCreate {
        name: Some("admin".into()),
        initial: None,
    });
    let created = daemon.send(Operation::WorkspaceCreate {
        name: None,
        initial: Some(sleeping_spec(&fixture)),
    });
    let snapshot = daemon.snapshot();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| w.id == created["workspace_id"].as_str().unwrap())
        .unwrap();
    let window = &workspace.windows[0];
    daemon.send(Operation::LayoutUpdate {
        workspace_id: workspace.id.clone(),
        window_id: window.id.clone(),
        expected_revision: window.revision,
        layout: None,
    });
    // Register a real process-bound lifetime, then detach it. The execution
    // belongs to its workspace, not to the lifetime of this connection.
    let identity = daemon.send(Operation::SessionInfo);
    let mut lifetime = UnixStream::connect(&fixture.socket).unwrap();
    lifetime
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    serde_json::to_writer(
        &mut lifetime,
        &Request::new(Operation::ClientRegister {
            session_id: identity["session"]["session_id"].as_str().unwrap().into(),
            daemon_id: identity["session"]["daemon_id"].as_str().unwrap().into(),
        }),
    )
    .unwrap();
    lifetime.write_all(b"\n").unwrap();
    let mut lifetime = BufReader::new(lifetime);
    let mut line = String::new();
    lifetime.read_line(&mut line).unwrap();
    assert!(serde_json::from_str::<Response>(&line).unwrap().ok);
    drop(lifetime);
    let after = daemon.snapshot();
    assert_eq!(after.workspaces.len(), 2);
    assert_eq!(
        after
            .panes
            .iter()
            .filter(|p| p.status == PaneStatus::Running)
            .count(),
        1
    );
    assert!(daemon.child.try_wait().unwrap().is_none());
    daemon.stop();
}

#[test]
fn last_pane_termination_ends_session_even_when_requester_disconnects_before_response() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start(&["/bin/sh", "-c", "exec sleep 60"]);
    let snapshot = daemon.snapshot();
    let mut stream = UnixStream::connect(&fixture.socket).unwrap();
    serde_json::to_writer(
        &mut stream,
        &Request::new(Operation::PaneTerminate {
            pane_id: snapshot.panes[0].id.clone(),
        }),
    )
    .unwrap();
    stream.write_all(b"\n").unwrap();
    // No response read and no lifetime acknowledgement can gate session teardown.
    drop(stream);
    daemon.wait_for_exit();
    let mut restarted = fixture.start(&[]);
    restarted.stop();
}
