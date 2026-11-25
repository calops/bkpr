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
        let socket = root
            .path()
            .join(bkpr_protocol::session_key(bkpr_protocol::DEFAULT_SESSION_NAME).unwrap())
            .join("s");
        Self {
            _root: root,
            state,
            home,
            cwd,
            socket,
            utility: std::env::current_exe().unwrap(),
        }
    }
    fn host_socket(&self) -> PathBuf {
        self._root.path().join("d")
    }
    fn host_command(&self) -> Command {
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
            .arg(self.host_socket())
            .arg("--state-root")
            .arg(&self.state)
            .arg("--bkpr")
            .arg(&self.utility)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }
    fn daemon_command(&self) -> Command {
        let mut command = self.host_command();
        command
            .arg("--session")
            .arg(bkpr_protocol::DEFAULT_SESSION_NAME);
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
    fn start_host(&self) -> Daemon {
        let mut daemon = Daemon {
            child: self.host_command().spawn().unwrap(),
            socket: self.socket.clone(),
            host_socket: self.host_socket(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !request(&daemon.host_socket, Operation::DaemonInfo).is_ok_and(|response| response.ok)
        {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "host exited before readiness"
            );
            assert!(Instant::now() < deadline, "host did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
        daemon
    }
    fn start(&self, argv: &[&str]) -> Daemon {
        self.start_command(self.command(argv))
    }
    fn start_command(&self, mut command: Command) -> Daemon {
        let child = command.spawn().unwrap();
        let mut daemon = Daemon {
            child,
            socket: self.socket.clone(),
            host_socket: self.host_socket(),
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
    host_socket: PathBuf,
}
impl Daemon {
    fn send(&self, operation: Operation) -> serde_json::Value {
        control(&self.socket, operation)
    }
    fn host_send(&self, operation: Operation) -> serde_json::Value {
        control(&self.host_socket, operation)
    }
    fn open(&self, name: &str, initial: Option<PaneSpawnSpec>) -> bkpr_protocol::SessionEndpoint {
        serde_json::from_value(self.host_send(Operation::SessionOpen {
            name: name.into(),
            defaults: None,
            initial,
        }))
        .unwrap()
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
        self.wait_for_session_end();
    }
    fn shutdown_host(&mut self) {
        let info = request(&self.host_socket, Operation::DaemonInfo)
            .unwrap()
            .result
            .unwrap()
            .into_json()
            .unwrap();
        let response = request(
            &self.host_socket,
            Operation::DaemonShutdown {
                daemon_id: info["daemon"]["daemon_id"].as_str().unwrap().into(),
            },
        )
        .unwrap();
        assert!(response.ok, "{:?}", response.error);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "host shutdown: {status}");
                assert!(!self.host_socket.exists());
                return;
            }
            assert!(Instant::now() < deadline, "owned test host did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait_for_session_end(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.socket.exists() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "session end stopped host"
            );
            assert!(Instant::now() < deadline, "session socket survived");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(self.child.try_wait().unwrap().is_none(), "idle host exited");
        assert!(
            request(&self.host_socket, Operation::DaemonInfo)
                .unwrap()
                .ok
        );
        self.shutdown_host();
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(response) = request(&self.host_socket, Operation::DaemonInfo)
                && let Some(payload) = response.result
                && let Ok(info) = payload.into_json()
                && let Some(daemon_id) = info["daemon"]["daemon_id"].as_str()
            {
                let _ = request(
                    &self.host_socket,
                    Operation::DaemonShutdown {
                        daemon_id: daemon_id.into(),
                    },
                );
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn control(socket: &Path, operation: Operation) -> serde_json::Value {
    let response = request(socket, operation).unwrap();
    assert!(
        response.ok,
        "fixture control rejected: {:?}",
        response.error
    );
    response.result.unwrap().into_json().unwrap()
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
fn explicit_cwd_bootstraps_when_the_invocation_directory_has_been_deleted() {
    let fixture = Fixture::new();
    let invocation = fixture._root.path().join("deleted-invocation");
    fs::create_dir(&invocation).unwrap();
    let server = fixture.command(&["/bin/sh", "-c", "pwd -P > explicit-cwd; exec sleep 60"]);
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args([
            "-c",
            "cd \"$1\" && rmdir \"$1\" && shift && exec \"$@\"",
            "bkpr-deleted-cwd-fixture",
        ])
        .arg(&invocation)
        .arg(server.get_program())
        .args(server.get_args())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in server.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    let mut daemon = fixture.start_command(command);
    assert!(!invocation.exists());
    wait_for(
        &fixture.cwd.join("explicit-cwd"),
        format!("{}\n", fixture.cwd.display()).as_bytes(),
    );
    assert_eq!(daemon.spawn_defaults().unwrap().cwd, fixture.cwd);
    assert_eq!(daemon.snapshot().panes[0].cwd, fixture.cwd);
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
fn passive_last_shell_exit_retires_session_without_stopping_host() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start(&[
        "/bin/sh",
        "-c",
        "printf ready > ready; while test ! -f release; do sleep 0.02; done",
    ]);
    let other = daemon.open("other", Some(sleeping_spec(&fixture)));
    let identity = daemon.send(Operation::SessionInfo);
    wait_for(&fixture.cwd.join("ready"), b"ready");
    // No control requests after releasing the last shell.
    fs::write(fixture.cwd.join("release"), b"exit").unwrap();
    wait_for_retired_session(&mut daemon, &fixture.socket);
    let state: RegistryResult =
        serde_json::from_value(control(&other.socket, Operation::Inspect)).unwrap();
    assert_eq!(state.registry.panes[0].status, PaneStatus::Running);
    let reopened = daemon.open(bkpr_protocol::DEFAULT_SESSION_NAME, None);
    assert_eq!(
        reopened.session.session_id,
        identity["session"]["session_id"]
    );
    assert_ne!(
        reopened.session.session_instance_id,
        identity["session"]["session_instance_id"]
    );
    assert_eq!(reopened.session.daemon_id, other.session.daemon_id);
    daemon.shutdown_host();
}

#[test]
fn last_explicit_workspace_close_notifies_lifetime_and_reaps_session_panes_without_ack() {
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
            session_instance_id: identity["session"]["session_instance_id"]
                .as_str()
                .unwrap()
                .into(),
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
    daemon.wait_for_session_end();
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
            session_instance_id: identity["session"]["session_instance_id"]
                .as_str()
                .unwrap()
                .into(),
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
    daemon.wait_for_session_end();
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
        host_socket: fixture.host_socket(),
    };
    wait_for(&fixture.cwd.join("fast-exit"), b"exited");
    restored.wait_for_session_end();
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
        host_socket: fixture.host_socket(),
    };
    wait_for(&fixture.cwd.join("restore-started"), b"started");
    // This request is never gated behind bootstrap. It must be safe on either
    // side of the batch's final commit, with no Inspect/client admission first.
    restoring.send(Operation::Shutdown);
    restoring.wait_for_session_end();
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
fn failed_fresh_bootstrap_reports_failure_and_releases_host_and_session_ownership() {
    let fixture = Fixture::new();
    let output = fixture
        .command(&["/no-such-bkpr-owned-test-shell"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
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
        host_socket: fixture.host_socket(),
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
        host_socket: fixture.host_socket(),
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

fn wait_for_retired_session(daemon: &mut Daemon, socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while socket.exists() {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "session end terminated its host"
        );
        assert!(Instant::now() < deadline, "session retained its socket");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        daemon.child.try_wait().unwrap().is_none(),
        "idle host exited"
    );
    assert!(
        request(&daemon.host_socket, Operation::DaemonInfo)
            .unwrap()
            .ok
    );
}

#[test]
fn one_host_pid_owns_independent_sessions_and_remains_idle_after_final_session_end() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start_host();
    let host = daemon.host_send(Operation::DaemonInfo);
    assert_eq!(host["daemon"]["pid"], daemon.child.id());
    assert!(
        !fixture.socket.exists(),
        "empty host implicitly opened a default session"
    );
    let mut alpha_spec = sleeping_spec(&fixture);
    alpha_spec.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$PPID\" > alpha-parent; exec sleep 60".into(),
    ];
    let alpha = daemon.open("alpha", Some(alpha_spec));
    let mut beta_spec = sleeping_spec(&fixture);
    beta_spec.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$PPID\" > beta-parent; exec sleep 60".into(),
    ];
    let beta = daemon.open("beta", Some(beta_spec));
    let host_pid = daemon.child.id().to_string();
    wait_for(&fixture.cwd.join("alpha-parent"), host_pid.as_bytes());
    wait_for(&fixture.cwd.join("beta-parent"), host_pid.as_bytes());
    assert_eq!(alpha.session.daemon_id, beta.session.daemon_id);
    assert_eq!(alpha.session.daemon_id, host["daemon"]["daemon_id"]);
    assert_ne!(alpha.session.session_id, beta.session.session_id);
    assert_ne!(
        alpha.session.session_instance_id,
        beta.session.session_instance_id
    );
    assert_ne!(alpha.socket, beta.socket);
    assert!(
        !request(
            &daemon.host_socket,
            Operation::DaemonShutdown {
                daemon_id: "wrong-incarnation".into()
            }
        )
        .unwrap()
        .ok
    );
    let alpha_state: RegistryResult =
        serde_json::from_value(control(&alpha.socket, Operation::Inspect)).unwrap();
    let workspace = &alpha_state.registry.workspaces[0];
    control(
        &alpha.socket,
        Operation::WorkspaceClose {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
        },
    );
    wait_for_retired_session(&mut daemon, &alpha.socket);
    let beta_state: RegistryResult =
        serde_json::from_value(control(&beta.socket, Operation::Inspect)).unwrap();
    let beta_pane = &beta_state.registry.panes[0];
    assert_eq!(
        control(
            &beta.socket,
            Operation::Pane {
                pane_id: beta_pane.id.clone(),
                request: PaneRequest::Inspect
            }
        )["lifecycle"],
        "running"
    );
    let reopened = daemon.open("alpha", None);
    assert_eq!(reopened.session.session_id, alpha.session.session_id);
    assert_eq!(reopened.session.daemon_id, alpha.session.daemon_id);
    assert_ne!(
        reopened.session.session_instance_id,
        alpha.session.session_instance_id
    );
    assert!(
        !request(
            &reopened.socket,
            Operation::ClientRegister {
                session_id: reopened.session.session_id.clone(),
                daemon_id: reopened.session.daemon_id.clone(),
                session_instance_id: alpha.session.session_instance_id,
            }
        )
        .unwrap()
        .ok
    );
    control(&reopened.socket, Operation::Shutdown);
    wait_for_retired_session(&mut daemon, &reopened.socket);
    control(&beta.socket, Operation::Shutdown);
    wait_for_retired_session(&mut daemon, &beta.socket);
    let gamma = daemon.open("after-idle", Some(sleeping_spec(&fixture)));
    assert_eq!(gamma.session.daemon_id, alpha.session.daemon_id);
    let state: RegistryResult =
        serde_json::from_value(control(&gamma.socket, Operation::Inspect)).unwrap();
    let pane = control(
        &gamma.socket,
        Operation::Pane {
            pane_id: state.registry.panes[0].id.clone(),
            request: PaneRequest::Inspect,
        },
    );
    assert_eq!(pane["lifecycle"], "running");
    daemon.shutdown_host();
    assert!(!gamma.socket.exists());
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc")
            .join(pane["pid"].as_u64().unwrap().to_string())
            .exists()
    );
    let root = fixture
        .state
        .join("sessions")
        .join(bkpr_protocol::session_key("after-idle").unwrap());
    let recovered = bkpr_core::registry::Registry::open_named(&root, "after-idle").unwrap();
    assert_eq!(
        recovered.snapshot().unwrap().panes[0].status,
        PaneStatus::Stopped
    );
}

#[test]
fn concurrent_session_open_requests_bootstrap_one_instance_once() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start_host();
    let mut spec = sleeping_spec(&fixture);
    spec.argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf once >> single-flight; exec sleep 60".into(),
    ];
    let barrier = std::sync::Barrier::new(5);
    let endpoints = thread::scope(|scope| {
        let callers = (0..4)
            .map(|_| {
                let barrier = &barrier;
                let socket = &daemon.host_socket;
                let spec = spec.clone();
                scope.spawn(move || {
                    barrier.wait();
                    serde_json::from_value::<bkpr_protocol::SessionEndpoint>(control(
                        socket,
                        Operation::SessionOpen {
                            name: "concurrent".into(),
                            defaults: None,
                            initial: Some(spec),
                        },
                    ))
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        callers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(endpoints.iter().all(|endpoint| endpoint == &endpoints[0]));
    wait_for(&fixture.cwd.join("single-flight"), b"once");
    let state: RegistryResult =
        serde_json::from_value(control(&endpoints[0].socket, Operation::Inspect)).unwrap();
    assert_eq!(state.registry.panes.len(), 1);
    daemon.shutdown_host();
}

#[test]
fn failed_session_open_reports_storage_and_exec_errors_without_stopping_other_sessions() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start_host();
    let stable = daemon.open("stable", Some(sleeping_spec(&fixture)));
    let root = fixture
        .state
        .join("sessions")
        .join(bkpr_protocol::session_key("broken").unwrap());
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.join("registry.json")).unwrap();
    assert!(
        !request(
            &daemon.host_socket,
            Operation::SessionOpen {
                name: "broken".into(),
                defaults: None,
                initial: None
            }
        )
        .unwrap()
        .ok
    );
    fs::remove_dir(root.join("registry.json")).unwrap();
    let fixed = daemon.open("broken", None);
    assert!(request(&fixed.socket, Operation::Inspect).unwrap().ok);
    let mut invalid = sleeping_spec(&fixture);
    invalid.argv = vec!["/no-such-bkpr-private-test-program".into()];
    assert!(
        !request(
            &daemon.host_socket,
            Operation::SessionOpen {
                name: "bad-exec".into(),
                defaults: None,
                initial: Some(invalid)
            }
        )
        .unwrap()
        .ok
    );
    assert!(request(&stable.socket, Operation::Inspect).unwrap().ok);
    let foreign_key = bkpr_protocol::session_key("foreign").unwrap();
    let foreign_socket = fixture
        .host_socket()
        .parent()
        .unwrap()
        .join(&foreign_key)
        .join("s");
    fs::create_dir(foreign_socket.parent().unwrap()).unwrap();
    fs::set_permissions(
        foreign_socket.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let foreign = std::os::unix::net::UnixListener::bind(&foreign_socket).unwrap();
    assert!(
        !request(
            &daemon.host_socket,
            Operation::SessionOpen {
                name: "foreign".into(),
                defaults: None,
                initial: None
            }
        )
        .unwrap()
        .ok
    );
    assert!(!fixture.state.join("sessions").join(&foreign_key).exists());
    drop(foreign);
    fs::remove_file(&foreign_socket).unwrap();
    daemon.open("foreign", None);
    daemon.shutdown_host();
}

#[test]
fn host_sigterm_drains_all_session_processes_and_releases_every_owned_endpoint() {
    let fixture = Fixture::new();
    let mut daemon = fixture.start_host();
    let mut endpoints = Vec::new();
    let mut pids = Vec::new();
    for name in ["signal-alpha", "signal-beta"] {
        let endpoint = daemon.open(name, Some(sleeping_spec(&fixture)));
        let state: RegistryResult =
            serde_json::from_value(control(&endpoint.socket, Operation::Inspect)).unwrap();
        let pane = control(
            &endpoint.socket,
            Operation::Pane {
                pane_id: state.registry.panes[0].id.clone(),
                request: PaneRequest::Inspect,
            },
        );
        pids.push(pane["pid"].as_u64().unwrap());
        endpoints.push(endpoint);
    }
    assert!(
        Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "kill -TERM \"$1\"", "bkpr-signal-fixture"])
            .arg(daemon.child.id().to_string())
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            assert!(status.success(), "signal shutdown failed: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "host signal cleanup did not finish"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!daemon.host_socket.exists());
    for endpoint in endpoints {
        assert!(!endpoint.socket.exists());
        let root = fixture
            .state
            .join("sessions")
            .join(bkpr_protocol::session_key(&endpoint.session.name).unwrap());
        assert!(bkpr_core::registry::Registry::open_named(&root, &endpoint.session.name).is_ok());
    }
    #[cfg(target_os = "linux")]
    for pid in pids {
        assert!(!Path::new("/proc").join(pid.to_string()).exists());
    }
}

#[test]
fn corrupt_notification_storage_does_not_disable_session_execution() {
    let fixture = Fixture::new();
    let notification_root = fixture.state.join("notifications");
    fs::create_dir(&fixture.state).unwrap();
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&notification_root).unwrap();
    fs::set_permissions(&notification_root, fs::Permissions::from_mode(0o700)).unwrap();
    let database = notification_root.join("notifications.sqlite3");
    let corrupt = b"deliberately invalid private notification database";
    fs::write(&database, corrupt).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

    let mut daemon = fixture.start_host();
    let health = daemon.host_send(Operation::Notifications {
        request: bkpr_protocol::NotificationRequest::Health {},
    });
    assert_eq!(health["healthy"], false);
    assert!(health["error"].is_string());
    let rejected = request(
        &daemon.host_socket,
        Operation::Notifications {
            request: bkpr_protocol::NotificationRequest::Inspect {
                query: bkpr_protocol::NotificationQuery::default(),
                after: None,
            },
        },
    )
    .unwrap();
    assert!(!rejected.ok);
    assert!(rejected.result.is_none());

    let endpoint = daemon.open(
        bkpr_protocol::DEFAULT_SESSION_NAME,
        Some(sleeping_spec(&fixture)),
    );
    let snapshot = daemon.snapshot();
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.status == PaneStatus::Running)
        .unwrap();
    let execution = control(
        &endpoint.socket,
        Operation::Pane {
            pane_id: pane.id.clone(),
            request: PaneRequest::Inspect,
        },
    );
    let pid = execution["pid"].as_u64().expect("real pane process") as u32;
    assert_ne!(pid, 0);
    #[cfg(target_os = "linux")]
    assert!(Path::new("/proc").join(pid.to_string()).exists());
    let stopped = request(
        &daemon.host_socket,
        Operation::DaemonShutdown {
            daemon_id: endpoint.session.daemon_id,
        },
    )
    .unwrap();
    assert!(
        !stopped.ok,
        "notification failure must remain explicit at shutdown"
    );
    assert!(!daemon.child.wait().unwrap().success());
    assert_eq!(fs::read(database).unwrap(), corrupt);
    #[cfg(target_os = "linux")]
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
}

fn register_channel_client(
    endpoint: &bkpr_protocol::SessionEndpoint,
) -> (UnixStream, bkpr_protocol::ClientAuthority) {
    let mut lifetime = UnixStream::connect(&endpoint.socket).unwrap();
    lifetime
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send_request(
        &mut lifetime,
        Operation::ClientRegister {
            session_id: endpoint.session.session_id.clone(),
            daemon_id: endpoint.session.daemon_id.clone(),
            session_instance_id: endpoint.session.session_instance_id.clone(),
        },
    )
    .unwrap();
    let response = read_response(&mut lifetime).unwrap();
    assert!(response.ok, "{:?}", response.error);
    let registered = response.result.unwrap().into_json().unwrap();
    (
        lifetime,
        serde_json::from_value(registered["client"].clone()).unwrap(),
    )
}

fn as_channel_client(authority: &bkpr_protocol::ClientAuthority, request: Operation) -> Operation {
    Operation::Client {
        authority: authority.clone(),
        request: Box::new(request),
    }
}

fn activate_channel_client(
    endpoint: &bkpr_protocol::SessionEndpoint,
    authority: &bkpr_protocol::ClientAuthority,
) {
    control(
        &endpoint.socket,
        as_channel_client(
            authority,
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: None,
            },
        ),
    );
}

#[test]
fn channel_history_reopens_on_the_same_session_only() {
    use bkpr_protocol::{
        Payload,
        channels::{
            ChannelPost, ChannelRequest, ChannelResponse, ChannelSettings,
            ChannelSubscriptionStatus, MembershipMode, WakePolicy,
        },
    };

    let fixture = Fixture::new();
    let mut daemon = fixture.start_host();
    let endpoint = daemon.open(
        bkpr_protocol::DEFAULT_SESSION_NAME,
        Some(sleeping_spec(&fixture)),
    );
    let workspace = daemon.snapshot().workspaces.into_iter().next().unwrap();
    let operation = |request| Operation::Channels { request };
    let (lifetime, authority) = register_channel_client(&endpoint);
    let send = |socket: &Path, authority: &bkpr_protocol::ClientAuthority, channel_request| {
        let response = request(
            socket,
            as_channel_client(authority, operation(channel_request)),
        )
        .unwrap();
        assert!(response.ok, "{:?}", response.error);
        let Some(Payload::Channels(result)) = response.result else {
            panic!("expected a typed channel response");
        };
        result
    };
    let create = ChannelRequest::Create {
        workspace_id: workspace.id.clone(),
        name: "build review".into(),
        membership: MembershipMode::WorkspaceWide,
        members: vec![],
        settings: Default::default(),
    };
    let bare = request(&endpoint.socket, operation(create.clone())).unwrap();
    assert!(bare.error.unwrap().contains("registered user client"));
    assert!(
        !request(
            &endpoint.socket,
            as_channel_client(&authority, operation(create.clone()))
        )
        .unwrap()
        .ok
    );
    activate_channel_client(&endpoint, &authority);
    let list = || ChannelRequest::List {
        workspace_id: workspace.id.clone(),
        after: None,
        limit: 8,
    };
    let ChannelResponse::Listed {
        channels,
        next_after: None,
    } = send(&endpoint.socket, &authority, list())
    else {
        panic!("expected the workspace presets");
    };
    assert_eq!(channels.len(), 2);
    let defaults = |name| {
        channels
            .iter()
            .find(|channel| channel.name == name)
            .unwrap()
    };
    let desk = defaults("#desk");
    let team = defaults("#team");
    assert_eq!(desk.membership, MembershipMode::WorkspaceWide);
    assert_eq!(team.membership, MembershipMode::WorkspaceWide);
    assert_eq!(
        team.settings,
        ChannelSettings {
            user_wake: WakePolicy::AddressedOrAll,
            agent_wake: WakePolicy::AddressedOrAll,
            attention_questions: false,
            attention_reports: false,
        }
    );
    let custom = ChannelSettings {
        user_wake: WakePolicy::AddressedOnly,
        agent_wake: WakePolicy::None,
        ..desk.settings
    };
    send(
        &endpoint.socket,
        &authority,
        ChannelRequest::SetSettings {
            channel_id: desk.id.clone(),
            settings: custom,
        },
    );
    let ChannelResponse::Channel { channel } = send(&endpoint.socket, &authority, create) else {
        panic!("channel was not created");
    };
    let post = ChannelRequest::Post {
        channel_id: channel.id.clone(),
        post: ChannelPost {
            idempotency_key: "review-1".into(),
            text: "Ready for review.".into(),
            intent: Default::default(),
            mentions: vec![],
            mention_spans: vec![],
            reply_to: None,
            attachments: vec![],
            source: None,
        },
    };
    let receipt = send(&endpoint.socket, &authority, post.clone());
    let ChannelResponse::Posted { message, .. } = &receipt else {
        panic!("post was not accepted");
    };
    let ChannelResponse::History {
        messages, cursor, ..
    } = send(
        &endpoint.socket,
        &authority,
        ChannelRequest::History {
            channel_id: channel.id.clone(),
            before: None,
            after: None,
            limit: 8,
        },
    )
    else {
        panic!("expected renderable history");
    };
    assert_eq!(messages[0].text, "Ready for review.");
    daemon.send(Operation::WorkspaceCreate {
        name: Some("Keep session open".into()),
        initial: Some(sleeping_spec(&fixture)),
    });
    daemon.send(Operation::WorkspaceClose {
        workspace_id: workspace.id.clone(),
        expected_revision: workspace.revision,
    });
    assert!(matches!(
        send(
            &endpoint.socket,
            &authority,
            ChannelRequest::Subscribe {
                channel_id: channel.id.clone(),
                after: Some(cursor),
                limit: 8,
                wait_ms: 0,
            }
        ),
        ChannelResponse::Changes {
            status: ChannelSubscriptionStatus::Closed,
            ..
        }
    ));
    let read = ChannelRequest::Read {
        channel_id: channel.id,
        message_id: message.id.clone(),
    };
    let other = daemon.open("unrelated", None);
    let (other_lifetime, other_authority) = register_channel_client(&other);
    activate_channel_client(&other, &other_authority);
    assert!(
        !request(
            &other.socket,
            as_channel_client(&other_authority, operation(read.clone()))
        )
        .unwrap()
        .ok
    );
    control(
        &endpoint.socket,
        as_channel_client(&authority, Operation::ClientDetach),
    );
    assert!(
        !request(
            &endpoint.socket,
            as_channel_client(&authority, operation(read.clone()))
        )
        .unwrap()
        .ok
    );
    drop((lifetime, other_lifetime));

    daemon.shutdown_host();
    let mut daemon = fixture.start_host();
    let reopened = daemon.open(bkpr_protocol::DEFAULT_SESSION_NAME, None);
    assert_eq!(reopened.session.session_id, endpoint.session.session_id);
    assert_ne!(
        reopened.session.session_instance_id,
        endpoint.session.session_instance_id
    );
    let (_lifetime, authority) = register_channel_client(&reopened);
    activate_channel_client(&reopened, &authority);
    let ChannelResponse::ArchivedWorkspaces { workspace_ids, .. } = send(
        &reopened.socket,
        &authority,
        ChannelRequest::ListArchivedWorkspaces {
            after: None,
            limit: 8,
        },
    ) else {
        panic!("expected archived workspaces");
    };
    let ChannelResponse::Listed {
        channels: archived,
        next_after: None,
    } = send(&reopened.socket, &authority, list())
    else {
        panic!("expected the retained channels");
    };
    assert_eq!(workspace_ids, [workspace.id]);
    assert_eq!(archived.len(), 3);
    assert_eq!(
        archived
            .iter()
            .find(|channel| channel.id == desk.id)
            .unwrap()
            .settings,
        custom
    );
    assert!(archived.iter().any(|channel| channel == team));
    assert_eq!(send(&reopened.socket, &authority, post), receipt);
    let ChannelResponse::Message { message } = send(&reopened.socket, &authority, read) else {
        panic!("message did not survive reopening");
    };
    assert_eq!(message.text, "Ready for review.");
    daemon.shutdown_host();
}

fn channel_list_operation(workspace_id: String) -> Operation {
    Operation::Channels {
        request: bkpr_protocol::channels::ChannelRequest::List {
            workspace_id,
            after: None,
            limit: 8,
        },
    }
}

#[test]
#[ignore = "owned subprocess fixture; requires its private working-directory marker"]
fn owned_channel_client_fixture() {
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        fs::read(cwd.join("owned-channel-client")).unwrap(),
        b"private"
    );
    let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
    let identity = control(&socket, Operation::SessionInfo);
    let endpoint = bkpr_protocol::SessionEndpoint {
        session: serde_json::from_value(identity["session"].clone()).unwrap(),
        socket,
    };
    let (_lifetime, authority) = register_channel_client(&endpoint);
    activate_channel_client(&endpoint, &authority);
    let response = request(
        &endpoint.socket,
        as_channel_client(
            &authority,
            channel_list_operation(std::env::var("BKPR_WORKSPACE_ID").unwrap()),
        ),
    )
    .unwrap();
    assert!(
        response
            .error
            .unwrap()
            .contains("daemon-owned processes cannot act as the channel user")
    );
    fs::write(cwd.join("channel-client-denied"), b"denied").unwrap();
    // Keep the workspace open for the positive check from outside the daemon.
    std::io::stdin().read_line(&mut String::new()).unwrap();
}

#[test]
fn pane_process_cannot_obtain_user_channel_authority_by_registering() {
    use bkpr_protocol::{
        Payload,
        channels::{ChannelRequest, ChannelResponse, MembershipMode},
    };

    let fixture = Fixture::new();
    fs::write(fixture.cwd.join("owned-channel-client"), b"private").unwrap();
    let mut daemon = fixture.start_host();
    let mut spec = sleeping_spec(&fixture);
    spec.argv = vec![
        fixture.utility.to_str().unwrap().into(),
        "--exact".into(),
        "owned_channel_client_fixture".into(),
        "--ignored".into(),
    ];
    let endpoint = daemon.open(bkpr_protocol::DEFAULT_SESSION_NAME, Some(spec));
    wait_for(&fixture.cwd.join("channel-client-denied"), b"denied");
    let workspace = daemon.snapshot().workspaces.into_iter().next().unwrap();
    let (_lifetime, authority) = register_channel_client(&endpoint);
    activate_channel_client(&endpoint, &authority);
    let created = request(
        &endpoint.socket,
        as_channel_client(
            &authority,
            Operation::Channels {
                request: ChannelRequest::Create {
                    workspace_id: workspace.id.clone(),
                    name: "Authority check".into(),
                    membership: MembershipMode::WorkspaceWide,
                    members: Vec::new(),
                    settings: Default::default(),
                },
            },
        ),
    )
    .unwrap();
    assert!(created.ok, "{:?}", created.error);
    let Some(Payload::Channels(ChannelResponse::Channel { channel })) = created.result else {
        panic!("expected the workspace's new channel");
    };
    let response = request(
        &endpoint.socket,
        as_channel_client(&authority, channel_list_operation(workspace.id)),
    )
    .unwrap();
    assert!(response.ok, "{:?}", response.error);
    assert!(matches!(
        response.result,
        Some(Payload::Channels(ChannelResponse::Listed {
            channels, ..
        })) if channels.iter().any(|entry| entry.id == channel.id)
    ));
    daemon.shutdown_host();
}

#[test]
fn channel_catalog_resumes_after_detach_and_restart_in_its_own_session() {
    use bkpr_protocol::{
        Payload,
        channels::{ChannelCatalogChange, ChannelRequest, ChannelResponse},
    };

    let fixture = Fixture::new();
    let name = bkpr_protocol::DEFAULT_SESSION_NAME;
    let mut daemon = fixture.start_host();
    let endpoint = daemon.open(name, Some(sleeping_spec(&fixture)));
    let initial = daemon.snapshot().workspaces.pop().unwrap();
    daemon.send(Operation::WorkspaceCreate {
        name: Some("Unselected workspace".into()),
        initial: Some(sleeping_spec(&fixture)),
    });
    let hidden = daemon
        .snapshot()
        .workspaces
        .into_iter()
        .find(|workspace| workspace.id != initial.id)
        .unwrap();
    let (first_lifetime, first) = register_channel_client(&endpoint);
    let snapshot = || ChannelRequest::CatalogSnapshot {
        after: None,
        limit: 1,
    };
    let payload = |reply: Response| {
        assert!(reply.ok, "{:?}", reply.error);
        let Some(Payload::Channels(reply)) = reply.result else {
            panic!("expected catalog payload");
        };
        reply
    };
    let send = |endpoint: &bkpr_protocol::SessionEndpoint,
                authority: &bkpr_protocol::ClientAuthority,
                request_body| {
        let reply = request(
            &endpoint.socket,
            as_channel_client(
                authority,
                Operation::Channels {
                    request: request_body,
                },
            ),
        )
        .unwrap();
        payload(reply)
    };
    assert!(
        !request(
            &endpoint.socket,
            Operation::Channels {
                request: snapshot()
            }
        )
        .unwrap()
        .ok
    );
    assert!(
        !request(
            &endpoint.socket,
            as_channel_client(
                &first,
                Operation::Channels {
                    request: snapshot()
                }
            )
        )
        .unwrap()
        .ok
    );
    activate_channel_client(&endpoint, &first);
    let ChannelResponse::CatalogSnapshot {
        cursor,
        next_after: Some(partial),
        ..
    } = send(&endpoint, &first, snapshot())
    else {
        panic!("expected paged catalog");
    };
    let ChannelResponse::CatalogSnapshot {
        channels,
        next_after: None,
        ..
    } = send(
        &endpoint,
        &first,
        ChannelRequest::CatalogSnapshot {
            after: None,
            limit: 8,
        },
    )
    else {
        panic!("expected all four presets");
    };
    assert_eq!(
        channels
            .iter()
            .filter(|channel| channel.workspace_id == hidden.id)
            .count(),
        2
    );
    control(
        &endpoint.socket,
        as_channel_client(&first, Operation::ClientDetach),
    );
    drop(first_lifetime);

    let (second_lifetime, second) = register_channel_client(&endpoint);
    activate_channel_client(&endpoint, &second);
    let mut waiting = UnixStream::connect(&endpoint.socket).unwrap();
    send_request(
        &mut waiting,
        as_channel_client(
            &second,
            Operation::Channels {
                request: ChannelRequest::CatalogSubscribe {
                    after: cursor.clone(),
                    limit: 4,
                    wait_ms: bkpr_protocol::channels::MAX_WAIT_MS,
                },
            },
        ),
    )
    .unwrap();
    // Wait for the server to read the caught-up cursor and commit to waiting.
    let diagnostics = fixture.state.join("diagnostics/events.jsonl");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fs::read_to_string(&diagnostics).is_ok_and(|events| {
        events.lines().any(|line| {
            line.contains(r#""operation":"channels.catalog""#)
                && line.contains(r#""outcome":"waiting""#)
        })
    }) {
        assert!(Instant::now() < deadline, "catalog subscriber did not wait");
        thread::sleep(Duration::from_millis(10));
    }
    daemon.send(Operation::WorkspaceClose {
        workspace_id: hidden.id.clone(),
        expected_revision: hidden.revision,
    });
    waiting
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let ChannelResponse::CatalogChanges {
        changes,
        cursor: after_close,
        more: false,
    } = payload(read_response(&mut waiting).unwrap())
    else {
        panic!("expected closure delta from the waiting subscriber");
    };
    assert_eq!(
        changes,
        [ChannelCatalogChange::WorkspaceClosed {
            workspace_id: hidden.id.clone()
        }]
    );
    assert_eq!(
        send(
            &endpoint,
            &second,
            ChannelRequest::CatalogSnapshot {
                after: Some(partial),
                limit: 1
            }
        ),
        ChannelResponse::CatalogReset {}
    );
    let resume =
        |after: &bkpr_protocol::channels::ChannelCatalogCursor| ChannelRequest::CatalogSubscribe {
            after: after.clone(),
            limit: 4,
            wait_ms: 0,
        };
    let another = daemon.open("other-catalog", None);
    let (other_lifetime, other) = register_channel_client(&another);
    activate_channel_client(&another, &other);
    assert_eq!(
        send(&another, &other, resume(&cursor)),
        ChannelResponse::CatalogReset {}
    );
    drop((second_lifetime, other_lifetime));
    daemon.shutdown_host();

    let mut daemon = fixture.start_host();
    let resumed = daemon.open(name, None);
    let (_lifetime, authority) = register_channel_client(&resumed);
    activate_channel_client(&resumed, &authority);
    assert_eq!(
        send(&resumed, &authority, resume(&after_close)),
        ChannelResponse::CatalogChanges {
            cursor: after_close,
            changes: vec![],
            more: false,
        }
    );
    let ChannelResponse::CatalogSnapshot {
        channels,
        next_after: None,
        ..
    } = send(
        &resumed,
        &authority,
        ChannelRequest::CatalogSnapshot {
            after: None,
            limit: 8,
        },
    )
    else {
        panic!("expected catalog without archived rows");
    };
    assert_eq!(channels.len(), 2);
    assert!(
        channels
            .iter()
            .all(|channel| channel.workspace_id == initial.id)
    );
    daemon.shutdown_host();
}
