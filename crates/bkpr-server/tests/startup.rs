use bkpr_protocol::{
    CaptureEvent, CaptureGrant, ConversationSource, HarnessKind, LayoutNode, Operation,
    PaneRequest, PaneSpawnEntry, PaneSpawnSpec, PaneStatus, RegistryResult, RegistrySnapshot,
    Request, Response, Size, SpawnDefaults, SpawnLayoutNode,
};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn append_spawn_layout(layout: Option<&LayoutNode>) -> SpawnLayoutNode {
    fn existing(layout: &LayoutNode) -> SpawnLayoutNode {
        match layout {
            LayoutNode::Leaf { pane_id } => SpawnLayoutNode::Existing {
                pane_id: pane_id.clone(),
            },
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => SpawnLayoutNode::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(existing(first)),
                second: Box::new(existing(second)),
            },
        }
    }
    let new = SpawnLayoutNode::New {
        reference: "pane".into(),
    };
    match layout {
        None => new,
        Some(layout) => SpawnLayoutNode::Split {
            axis: bkpr_protocol::Axis::Horizontal,
            ratio: 500,
            first: Box::new(existing(layout)),
            second: Box::new(new),
        },
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    socket: PathBuf,
    utility: PathBuf,
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
            utility: std::env::current_exe().unwrap(),
        }
    }
    fn daemon_command(&self) -> Command {
        let executable = std::env::var_os("BKPR_TEST_SERVER")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_bkprd").into());
        let mut command = Command::new(executable);
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
            .arg(&self.utility)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }
    fn command(&self, argv: &[&str]) -> Command {
        let mut command = self.daemon_command();
        command
            .arg("--home")
            .arg(&self.home)
            .arg("--cwd")
            .arg(&self.cwd);
        if !argv.is_empty() {
            command.arg("--").args(argv);
        }
        command
    }
    fn start(&self, argv: &[&str]) -> Daemon {
        self.start_command(self.command(argv))
    }
    fn start_command(&self, mut command: Command) -> Daemon {
        let child = command.spawn().unwrap();
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
        response.result.unwrap().into_json().unwrap()
    }
    fn snapshot(&self) -> RegistrySnapshot {
        let response: RegistryResult =
            serde_json::from_value(self.send(Operation::Inspect)).unwrap();
        response.registry
    }
    fn spawn_defaults(&self) -> Option<SpawnDefaults> {
        serde_json::from_value(self.send(Operation::SessionInfo)["spawn_defaults"].clone()).unwrap()
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
    send_request(&mut stream, operation)?;
    read_response(&mut stream)
}
fn send_request(stream: &mut UnixStream, operation: Operation) -> anyhow::Result<()> {
    let mut codec = bkpr_protocol::wire::Codec::new();
    let bytes = codec.encode_request(&Request::new(operation))?;
    bkpr_protocol::wire::write_frame(stream, bkpr_protocol::wire::Kind::Request, bytes)
}
fn read_response(stream: &mut impl std::io::Read) -> anyhow::Result<Response> {
    let bytes = bkpr_protocol::wire::read_frame(
        stream,
        bkpr_protocol::wire::Kind::Response,
        96 * 1024 * 1024,
    )?;
    bkpr_protocol::wire::decode_response_owned(bytes)
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
    assert_eq!(
        daemon.spawn_defaults(),
        Some(SpawnDefaults {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf fresh > fresh; exec sleep 60".into(),
            ],
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            isolated_environment: true,
        })
    );
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
    assert_eq!(first.spawn_defaults(), None);
    assert!(first.snapshot().workspaces.is_empty());
    assert!(first.snapshot().panes.is_empty());
    let (_, pane) = historical(&fixture, &first);
    let before = first.snapshot();
    first.stop();
    let mut second = fixture.start(&[]);
    assert_eq!(second.spawn_defaults(), None);
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
fn headless_startup_captures_daemon_environment_without_running_its_shell() {
    let fixture = Fixture::new();
    let shell = fixture.home.join("owned-startup-shell");
    let marker = fixture.cwd.join("unexpected-shell-execution");
    fs::write(
        &shell,
        b"#!/bin/sh\nprintf executed > unexpected-shell-execution\n",
    )
    .unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = fixture.daemon_command();
    command.current_dir(&fixture.cwd).env("SHELL", &shell);
    let mut daemon = fixture.start_command(command);
    assert_eq!(
        daemon.spawn_defaults(),
        Some(SpawnDefaults {
            argv: vec![shell.into_os_string().into_string().unwrap()],
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            isolated_environment: false,
        })
    );
    let snapshot = daemon.snapshot();
    assert!(snapshot.panes.is_empty());
    assert!(snapshot.workspaces.is_empty());
    assert!(!marker.exists());
    daemon.stop();
    assert!(!marker.exists());
}

#[test]
fn configured_headless_restart_uses_current_daemon_inputs_not_saved_panes() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    historical(&fixture, &first);
    first.stop();
    let shell = fixture.home.join("current-shell-does-not-need-to-exist");
    let mut command = fixture.command(&[]);
    command
        .current_dir(&fixture.home)
        .env("SHELL", &shell)
        .env("HOME", fixture.home.join("not-the-explicit-home"));
    let mut daemon = fixture.start_command(command);
    assert_eq!(
        daemon.spawn_defaults(),
        Some(SpawnDefaults {
            argv: vec![shell.into_os_string().into_string().unwrap()],
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            isolated_environment: true,
        })
    );
    let snapshot = daemon.snapshot();
    assert_eq!(snapshot.panes.len(), 1);
    assert_eq!(snapshot.panes[0].status, PaneStatus::Stopped);
    assert_eq!(
        fs::read(fixture.cwd.join("original-runs")).unwrap(),
        b"original"
    );
    daemon.stop();
}

#[test]
fn unavailable_headless_defaults_remain_unconfigured_without_creating_paths() {
    for (variable, value) in [
        ("SHELL", None),
        ("SHELL", Some("")),
        ("HOME", None),
        ("HOME", Some("")),
        ("HOME", Some("relative-home")),
    ] {
        let fixture = Fixture::new();
        let mut command = fixture.daemon_command();
        command.current_dir(&fixture.cwd).env("SHELL", "/bin/sh");
        match value {
            Some(value) => {
                command.env(variable, value);
            }
            None => {
                command.env_remove(variable);
            }
        }
        let mut daemon = fixture.start_command(command);
        assert_eq!(daemon.spawn_defaults(), None, "{variable}={value:?}");
        assert!(daemon.snapshot().panes.is_empty());
        assert!(!fixture.cwd.join("relative-home").exists());
        daemon.stop();
    }
}

#[test]
fn explicit_startup_without_home_preserves_the_prerequisite_failure() {
    let fixture = Fixture::new();
    let output = fixture
        .daemon_command()
        .env_remove("HOME")
        .current_dir(&fixture.cwd)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf executed > unexpected-execution",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HOME is unavailable; supply --home"));
    assert!(!fixture.cwd.join("unexpected-execution").exists());
    assert!(!fixture.socket.exists());
}

#[test]
fn explicit_loaded_bootstrap_restores_owned_layout_with_current_shell_and_latest_cwd() {
    let fixture = Fixture::new();
    let later = fixture.home.join("later");
    let mut first = fixture.start(&[]);
    let (workspace, original) = historical(&fixture, &first);
    fs::create_dir(&later).unwrap();
    let mut panes = vec![original];
    for title in ["middle", "last", "hidden"] {
        let snapshot = first.snapshot();
        let window = &snapshot.workspaces[0].windows[0];
        let mut spec = sleeping_spec(&fixture);
        spec.argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("cd \"$HOME/later\"; printf ready > {title}-cwd-ready; exec sleep 60"),
        ];
        spec.title = Some(title.into());
        let result = first.send(Operation::PaneSpawn {
            workspace_id: Some(workspace.clone()),
            window_id: Some(window.id.clone()),
            expected_revision: Some(window.revision),
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec,
            }],
            layout: Some(append_spawn_layout(window.layout.as_ref())),
        });
        panes.push(result["pane_id"].as_str().unwrap().to_owned());
    }
    for title in ["middle", "last", "hidden"] {
        wait_for(&later.join(format!("{title}-cwd-ready")), b"ready");
    }
    let snapshot = first.snapshot();
    let window = &snapshot.workspaces[0].windows[0];
    let layout = LayoutNode::Split {
        axis: bkpr_protocol::Axis::Vertical,
        ratio: 317,
        first: Box::new(LayoutNode::Leaf {
            pane_id: panes[0].clone(),
        }),
        second: Box::new(LayoutNode::Split {
            axis: bkpr_protocol::Axis::Horizontal,
            ratio: 683,
            first: Box::new(LayoutNode::Leaf {
                pane_id: panes[1].clone(),
            }),
            second: Box::new(LayoutNode::Leaf {
                pane_id: panes[2].clone(),
            }),
        }),
    };
    first.send(Operation::LayoutUpdate {
        workspace_id: workspace.clone(),
        window_id: window.id.clone(),
        expected_revision: window.revision,
        layout: Some(layout.clone()),
    });
    let independent_cwd = fixture.home.join("independent");
    fs::create_dir(&independent_cwd).unwrap();
    let mut spec = sleeping_spec(&fixture);
    spec.cwd = independent_cwd.clone();
    spec.title = Some("independent".into());
    first.send(Operation::WindowCreate {
        workspace_id: workspace.clone(),
        expected_revision: first.snapshot().workspaces[0].revision,
        name: Some("independent".into()),
        spec,
    });
    first.send(Operation::WorkspaceCreate {
        name: Some("admin".into()),
        initial: None,
    });
    let before = first.snapshot();
    first.stop();
    let mut restored = fixture.start(&[
        "/bin/sh",
        "-c",
        "pwd -P > restored-cwd; printf current >> current-runs; exec sleep 60",
    ]);
    assert_eq!(
        restored.spawn_defaults(),
        Some(SpawnDefaults {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "pwd -P > restored-cwd; printf current >> current-runs; exec sleep 60".into(),
            ],
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            isolated_environment: true,
        })
    );
    wait_for(&fixture.cwd.join("current-runs"), b"current");
    wait_for(&later.join("current-runs"), b"currentcurrentcurrent");
    wait_for(&independent_cwd.join("current-runs"), b"current");
    let after = restored.snapshot();
    assert_eq!(after.workspaces.len(), before.workspaces.len());
    assert_eq!(after.panes.len(), before.panes.len());
    let owned = after.workspaces.iter().find(|w| w.id == workspace).unwrap();
    assert_eq!(owned.name, "Main");
    assert_eq!(owned.windows[0].id, before.workspaces[0].windows[0].id);
    assert_eq!(owned.windows[0].layout, Some(layout));
    assert_eq!(owned.windows.len(), before.workspaces[0].windows.len());
    for window in &before.workspaces[0].windows {
        let current = owned
            .windows
            .iter()
            .find(|candidate| candidate.id == window.id)
            .unwrap();
        assert_eq!(current.name, window.name);
        assert_eq!(current.layout, window.layout);
    }
    for old in &before.panes {
        let pane = after.panes.iter().find(|p| p.id == old.id).unwrap();
        assert_eq!(pane.status, PaneStatus::Running);
        assert_eq!(pane.title, old.title);
        assert_eq!(pane.workspace_id, old.workspace_id);
        assert_eq!(pane.window_id, old.window_id);
        assert_eq!(pane.home, old.home);
    }
    assert_eq!(
        fs::read_to_string(later.join("restored-cwd"))
            .unwrap()
            .trim_end(),
        later.to_str().unwrap()
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
        panes: vec![PaneSpawnEntry {
            reference: "pane".into(),
            spec: spec.clone(),
        }],
        layout: Some(append_spawn_layout(window.layout.as_ref())),
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
    let mut automatic = fixture.start(&["/bin/sh", "-c", "exec sleep 60"]);
    let after = automatic.snapshot();
    let retired = after.panes.iter().find(|pane| pane.id == old).unwrap();
    assert_ne!(retired.status, PaneStatus::Running);
    assert!(retired.workspace_id.is_none() && retired.window_id.is_none());
    assert_eq!(
        after
            .panes
            .iter()
            .filter(|pane| pane.status == PaneStatus::Running)
            .count(),
        2,
        "only still-owned panes may restart; retired and explicitly removed panes stay retired"
    );
    assert_eq!(
        fs::read(fixture.cwd.join("original-runs")).unwrap(),
        b"original"
    );
    automatic.stop();
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
    send_request(
        &mut lifetime,
        Operation::ClientRegister {
            session_id: identity["session"]["session_id"].as_str().unwrap().into(),
            daemon_id: identity["session"]["daemon_id"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    let registered = read_response(&mut lifetime).unwrap();
    assert!(registered.ok, "{:?}", registered.error);
    let standalone = daemon.send(Operation::PaneSpawn {
        workspace_id: None,
        window_id: None,
        expected_revision: None,
        panes: vec![PaneSpawnEntry {
            reference: "pane".into(),
            spec: sleeping_spec(&fixture),
        }],
        layout: None,
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
    let bytes = bkpr_protocol::wire::read_frame(
        &mut lifetime,
        bkpr_protocol::wire::Kind::ClientEvent,
        8192,
    )
    .unwrap();
    let event = bkpr_protocol::wire::decode_client_event(&bytes).unwrap();
    assert_eq!(event.event, "client_detached");
    assert_eq!(event.reason, "shutdown");
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
    send_request(
        &mut lifetime,
        Operation::ClientRegister {
            session_id: identity["session"]["session_id"].as_str().unwrap().into(),
            daemon_id: identity["session"]["daemon_id"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    assert!(read_response(&mut lifetime).unwrap().ok);
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
    send_request(
        &mut stream,
        Operation::PaneTerminate {
            pane_id: snapshot.panes[0].id.clone(),
        },
    )
    .unwrap();
    // No response read and no lifetime acknowledgement can gate session teardown.
    drop(stream);
    daemon.wait_for_exit();
    let mut restarted = fixture.start(&[]);
    restarted.stop();
}

#[test]
fn missing_saved_directory_does_not_replace_layout_or_block_independent_shell() {
    let fixture = Fixture::new();
    let removed = fixture.home.join("removed");
    let mut first = fixture.start(&[]);
    let (workspace, healthy) = historical(&fixture, &first);
    fs::create_dir(&removed).unwrap();
    let window = first.snapshot().workspaces[0].windows[0].clone();
    let mut spec = sleeping_spec(&fixture);
    spec.cwd = removed.clone();
    spec.title = Some("missing directory".into());
    let missing = first.send(Operation::PaneSpawn {
        workspace_id: Some(workspace.clone()),
        window_id: Some(window.id),
        expected_revision: Some(window.revision),
        panes: vec![PaneSpawnEntry {
            reference: "pane".into(),
            spec,
        }],
        layout: Some(append_spawn_layout(window.layout.as_ref())),
    })["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = first.snapshot();
    first.stop();
    fs::remove_dir(&removed).unwrap();
    let mut restored = fixture.start(&["/bin/sh", "-c", "printf healthy > healthy; exec sleep 60"]);
    wait_for(&fixture.cwd.join("healthy"), b"healthy");
    let after = restored.snapshot();
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    assert_eq!(after.panes.len(), before.panes.len());
    assert_eq!(
        after.panes.iter().find(|p| p.id == healthy).unwrap().status,
        PaneStatus::Running
    );
    let failed = after.panes.iter().find(|p| p.id == missing).unwrap();
    assert_eq!(failed.status, PaneStatus::Stopped);
    assert_eq!(failed.cwd, removed);
    assert_eq!(failed.title, "missing directory");
    assert_eq!(failed.workspace_id.as_deref(), Some(workspace.as_str()));
    assert!(restored.child.try_wait().unwrap().is_none());
    restored.stop();
    fs::create_dir(&removed).unwrap();
    let mut retried = fixture.start(&["/bin/sh", "-c", "exec sleep 60"]);
    let after = retried.snapshot();
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    assert!(
        after
            .panes
            .iter()
            .all(|pane| pane.status == PaneStatus::Running)
    );
    retried.stop();
}

struct NativeFixtureChild(Child);
impl Drop for NativeFixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "owned subprocess fixture; requires its explicit private HOME marker"]
fn owned_native_fixture() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_eq!(
        fs::read(home.join("owned-native-fixture")).unwrap(),
        b"private"
    );
    let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
    let begin = request(
        &socket,
        Operation::HarnessBegin {
            kind: HarnessKind::Codex,
        },
    )
    .unwrap();
    fs::write(
        home.join("native-fixture-stage"),
        format!("begin ok={} error={:?}", begin.ok, begin.error),
    )
    .unwrap();
    assert!(begin.ok, "{:?}", begin.error);
    let grant: CaptureGrant =
        serde_json::from_value(begin.result.unwrap().into_json().unwrap()).unwrap();
    let _native = NativeFixtureChild(
        Command::new(home.join("native/codex"))
            .args(["-c", "printf ready > \"$HOME/native-ready\"; read gate"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    wait_for(&home.join("native-ready"), b"ready");
    let bind = request(
        &socket,
        Operation::HarnessEvent {
            launch_id: grant.launch_id.clone(),
            token: grant.token.clone(),
            event: CaptureEvent::Bind {
                conversation_id: "owned-native-conversation".into(),
                source: ConversationSource::CodexResume,
                expected_revision: None,
            },
        },
    )
    .unwrap();
    fs::write(
        home.join("native-fixture-stage"),
        format!("bind ok={} error={:?}", bind.ok, bind.error),
    )
    .unwrap();
    assert!(bind.ok, "{:?}", bind.error);
    fs::write(
        home.join("native-bound"),
        serde_json::to_vec(&grant).unwrap(),
    )
    .unwrap();
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn native_fixed_resume_refusal_preserves_owned_intent_and_independent_restoration() {
    let mut fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    let (workspace, shell) = historical(&fixture, &first);
    fs::write(fixture.home.join("owned-native-fixture"), b"private").unwrap();
    let executable = fixture.home.join("codex");
    std::os::unix::fs::symlink(&fixture.utility, &executable).unwrap();
    fs::create_dir(fixture.home.join("native")).unwrap();
    fs::copy("/bin/sh", fixture.home.join("native/codex")).unwrap();
    let window = first.snapshot().workspaces[0].windows[0].clone();
    let mut spec = sleeping_spec(&fixture);
    spec.argv = vec![
        executable.to_str().unwrap().into(),
        "--ignored".into(),
        "--exact".into(),
        "owned_native_fixture".into(),
        "--nocapture".into(),
    ];
    spec.title = Some("native".into());
    let native = first.send(Operation::PaneSpawn {
        workspace_id: Some(workspace.clone()),
        window_id: Some(window.id),
        expected_revision: Some(window.revision),
        panes: vec![PaneSpawnEntry {
            reference: "pane".into(),
            spec,
        }],
        layout: Some(append_spawn_layout(window.layout.as_ref())),
    })["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.home.join("native-bound").exists() {
        assert!(
            Instant::now() < deadline,
            "native fixture did not authenticate/bind: {:?}",
            fs::read_to_string(fixture.home.join("native-fixture-stage"))
        );
        thread::sleep(Duration::from_millis(5));
    }
    let before = first.snapshot();
    let recoveries = first.send(Operation::RecoveryList);
    assert_eq!(recoveries["recoveries"].as_array().unwrap().len(), 1);
    first.stop();
    // A deliberately refusing, owned native adapter. Its actual argv must be the
    // fixed CURRENT adapter invocation, never the old fixture flags/program.
    fixture.utility = PathBuf::from("/bin/sh");
    fs::write(
        fixture.cwd.join("run"),
        b"printf '%s\\n' \"$@\" > resumed-argv\nexit 23\n",
    )
    .unwrap();
    let mut restored = fixture.start(&[
        "/bin/sh",
        "-c",
        "printf shell > restored-shell; exec sleep 60",
    ]);
    wait_for(
        &fixture.cwd.join("resumed-argv"),
        b"codex\nresume\nowned-native-conversation\n",
    );
    wait_for(&fixture.cwd.join("restored-shell"), b"shell");
    let deadline = Instant::now() + Duration::from_secs(10);
    let after = loop {
        let snapshot = restored.snapshot();
        if snapshot
            .panes
            .iter()
            .any(|p| p.id == native && p.status == PaneStatus::Stopped)
        {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "refused native execution remained live"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(after.panes.len(), before.panes.len());
    assert_eq!(
        after.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    assert_eq!(
        after.panes.iter().find(|p| p.id == shell).unwrap().status,
        PaneStatus::Running
    );
    let failed = after.panes.iter().find(|p| p.id == native).unwrap();
    assert_eq!(failed.workspace_id.as_deref(), Some(workspace.as_str()));
    assert!(failed.harness.is_none());
    let recoveries = restored.send(Operation::RecoveryList);
    assert_eq!(recoveries["recoveries"].as_array().unwrap().len(), 1);
    assert_eq!(recoveries["recoveries"][0]["pane_id"], native);
    assert_eq!(
        recoveries["recoveries"][0]["harness"]["conversation_id"],
        "owned-native-conversation"
    );
    restored.send(Operation::PaneTerminate { pane_id: shell });
    assert!(restored.child.try_wait().unwrap().is_none());
    let remaining = restored.snapshot();
    assert_eq!(remaining.workspaces.len(), 1);
    assert_eq!(
        remaining
            .panes
            .iter()
            .find(|p| p.id == native)
            .unwrap()
            .status,
        PaneStatus::Stopped
    );
    restored.stop();
}

#[test]
fn restored_last_shell_fast_exit_releases_socket_and_metadata_lease() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    historical(&fixture, &first);
    first.stop();
    // Do not wait for Inspect: a real last-pane exit may close startup before
    // any client attaches. The same state/socket must remain reusable.
    let mut restored = Daemon {
        child: fixture
            .command(&["/bin/sh", "-c", "printf exited > fast-exit"])
            .spawn()
            .unwrap(),
        socket: fixture.socket.clone(),
    };
    restored.wait_for_exit();
    wait_for(&fixture.cwd.join("fast-exit"), b"exited");
    let mut headless = fixture.start(&[]);
    assert!(headless.snapshot().workspaces.is_empty());
    headless.stop();
}

#[test]
fn shutdown_during_cold_restore_never_leaves_an_owned_startup_process_or_lease() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    let (workspace, _) = historical(&fixture, &first);
    for _ in 0..12 {
        let window = first.snapshot().workspaces[0].windows[0].clone();
        first.send(Operation::PaneSpawn {
            workspace_id: Some(workspace.clone()),
            window_id: Some(window.id),
            expected_revision: Some(window.revision),
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: sleeping_spec(&fixture),
            }],
            layout: Some(append_spawn_layout(window.layout.as_ref())),
        });
    }
    let before = first.snapshot();
    first.stop();
    let mut restoring = Daemon {
        child: fixture
            .command(&[
                "/bin/sh",
                "-c",
                "printf started > restore-started; exec sleep 60",
            ])
            .spawn()
            .unwrap(),
        socket: fixture.socket.clone(),
    };
    wait_for(&fixture.cwd.join("restore-started"), b"started");
    // This request is never gated behind bootstrap. It must be safe on either
    // side of the batch's final commit, with no Inspect/client admission first.
    restoring.send(Operation::Shutdown);
    restoring.wait_for_exit();
    let mut headless = fixture.start(&[]);
    let stopped = headless.snapshot();
    assert_eq!(stopped.panes.len(), before.panes.len());
    assert!(
        stopped
            .panes
            .iter()
            .all(|pane| pane.status == PaneStatus::Stopped)
    );
    assert_eq!(
        stopped.workspaces[0].windows[0].layout,
        before.workspaces[0].windows[0].layout
    );
    headless.stop();
}

#[test]
fn failed_fresh_bootstrap_does_not_publish_ready_or_leave_socket_ownership() {
    let fixture = Fixture::new();
    let output = fixture
        .command(&["/no-such-bkpr-owned-test-shell"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"ready\":true"));
    assert!(!fixture.socket.exists());
    let mut headless = fixture.start(&[]);
    assert!(
        headless
            .snapshot()
            .panes
            .iter()
            .all(|pane| pane.status != PaneStatus::Running)
    );
    headless.stop();
}

fn closed_output() -> Stdio {
    let (output, reader) = UnixStream::pair().unwrap();
    drop(reader);
    Stdio::from(std::os::fd::OwnedFd::from(output))
}

fn require_startup_resolution(fixture: &Fixture, daemon: &mut Daemon) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.socket.exists() && daemon.child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "startup never bound its owned socket"
        );
        thread::sleep(Duration::from_millis(5));
    }
    if request(&fixture.socket, Operation::Inspect).is_ok_and(|response| response.ok) {
        daemon.stop();
    } else {
        while daemon.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "startup logging failure stranded the listener, admission gate and lease"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(
        !fixture.socket.exists(),
        "resolved startup retained socket ownership"
    );
}

#[test]
fn closed_startup_stdout_cannot_strand_control_admission_or_session_ownership() {
    let fixture = Fixture::new();
    let mut daemon = Daemon {
        child: fixture
            .command(&["/bin/sh", "-c", "exec sleep 60"])
            .stdout(closed_output())
            .spawn()
            .unwrap(),
        socket: fixture.socket.clone(),
    };
    require_startup_resolution(&fixture, &mut daemon);
    let mut headless = fixture.start(&[]);
    assert!(
        headless
            .snapshot()
            .panes
            .iter()
            .all(|pane| pane.status != PaneStatus::Running)
    );
    headless.stop();
}

#[test]
fn closed_restore_stderr_cannot_strand_failed_saved_intent_or_session_ownership() {
    let fixture = Fixture::new();
    let mut first = fixture.start(&[]);
    let (workspace, _) = historical(&fixture, &first);
    let removed = fixture.home.join("removed");
    fs::create_dir(&removed).unwrap();
    let window = first.snapshot().workspaces[0].windows[0].clone();
    let mut spec = sleeping_spec(&fixture);
    spec.cwd = removed.clone();
    let missing = first.send(Operation::PaneSpawn {
        workspace_id: Some(workspace.clone()),
        window_id: Some(window.id),
        expected_revision: Some(window.revision),
        panes: vec![PaneSpawnEntry {
            reference: "pane".into(),
            spec,
        }],
        layout: Some(append_spawn_layout(window.layout.as_ref())),
    })["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    first.stop();
    fs::remove_dir(&removed).unwrap();
    let mut daemon = Daemon {
        child: fixture
            .command(&["/bin/sh", "-c", "exec sleep 60"])
            .stderr(closed_output())
            .spawn()
            .unwrap(),
        socket: fixture.socket.clone(),
    };
    require_startup_resolution(&fixture, &mut daemon);
    let mut headless = fixture.start(&[]);
    let snapshot = headless.snapshot();
    let failed = snapshot
        .panes
        .iter()
        .find(|pane| pane.id == missing)
        .unwrap();
    assert_eq!(failed.workspace_id.as_deref(), Some(workspace.as_str()));
    assert_ne!(failed.status, PaneStatus::Running);
    headless.stop();
}
