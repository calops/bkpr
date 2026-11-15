use super::*;
use bkpr_protocol::{RegistryResult, Request, Response, Size};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    host: HostDaemon,
    serving: Option<thread::JoinHandle<Result<()>>>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("bh").tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let host = HostDaemon::bind(HostConfig {
            socket: root.path().join("d"),
            state_root: root.path().join("state"),
            bkpr: std::env::current_exe().unwrap(),
        })
        .unwrap();
        let serving_host = host.clone();
        let serving = thread::spawn(move || serving_host.serve());
        Self {
            root,
            host,
            serving: Some(serving),
        }
    }
    fn spec(&self, script: &str) -> PaneSpawnSpec {
        PaneSpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: self.root.path().to_owned(),
            home: self.root.path().join("home"),
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
    fn root(&self, name: &str) -> PathBuf {
        self.root
            .path()
            .join("state/sessions")
            .join(bkpr_protocol::session_key(name).unwrap())
    }
    fn open(&self, name: &str, initial: Option<PaneSpawnSpec>) -> SessionEndpoint {
        self.host.open_session(name.into(), None, initial).unwrap()
    }
    fn finish(&mut self) -> Result<()> {
        self.host.request_shutdown();
        self.serving.take().unwrap().join().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.request_shutdown();
        if let Some(worker) = self.serving.take() {
            let _ = worker.join();
        }
    }
}
fn wire(socket: &Path, operation: Operation) -> Response {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut codec = bkpr_protocol::wire::Codec::new();
    bkpr_protocol::wire::write_frame(
        &mut stream,
        bkpr_protocol::wire::Kind::Request,
        codec.encode_request(&Request::new(operation)).unwrap(),
    )
    .unwrap();
    let bytes = bkpr_protocol::wire::read_frame(
        &mut stream,
        bkpr_protocol::wire::Kind::Response,
        bkpr_protocol::wire::MAX_RESPONSE,
    )
    .unwrap();
    bkpr_protocol::wire::decode_response_owned(bytes).unwrap()
}
fn value(socket: &Path, operation: Operation) -> serde_json::Value {
    let response = wire(socket, operation);
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap().into_json().unwrap()
}
fn snapshot(socket: &Path) -> bkpr_protocol::RegistrySnapshot {
    serde_json::from_value::<RegistryResult>(value(socket, Operation::Inspect))
        .unwrap()
        .registry
}
fn wait_for(path: &Path, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(path).ok().as_deref() != Some(expected) {
        assert!(
            Instant::now() < deadline,
            "owned child did not publish fixture marker"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sessions_share_host_identity_but_stop_reopen_and_registration_are_independent() {
    let mut fixture = Fixture::new();
    let alpha = fixture.open("alpha", Some(fixture.spec("exec sleep 60")));
    let beta = fixture.open("beta", Some(fixture.spec("exec sleep 60")));
    assert_eq!(alpha.session.daemon_id, beta.session.daemon_id);
    assert_eq!(alpha.session.daemon_id, fixture.host.info().daemon_id);
    assert_ne!(alpha.session.session_id, beta.session.session_id);
    assert_ne!(
        alpha.session.session_instance_id,
        beta.session.session_instance_id
    );
    assert_eq!(
        value(&fixture.host.inner.config.socket, Operation::DaemonInfo)["daemon"]["pid"],
        std::process::id()
    );
    assert!(!wire(&alpha.socket, Operation::DaemonInfo).ok);
    assert!(!wire(&fixture.host.inner.config.socket, Operation::Inspect).ok);
    assert!(
        !wire(
            &fixture.host.inner.config.socket,
            Operation::DaemonShutdown {
                daemon_id: Uuid::new_v4().to_string()
            }
        )
        .ok
    );
    assert!(snapshot(&beta.socket).panes[0].status == bkpr_protocol::PaneStatus::Running);
    assert!(wire(&alpha.socket, Operation::Shutdown).ok);
    let reopened = fixture.open("alpha", None);
    assert_eq!(alpha.session.session_id, reopened.session.session_id);
    assert_eq!(alpha.session.daemon_id, reopened.session.daemon_id);
    assert_ne!(
        alpha.session.session_instance_id,
        reopened.session.session_instance_id
    );
    assert!(
        !wire(
            &reopened.socket,
            Operation::ClientRegister {
                session_id: reopened.session.session_id.clone(),
                daemon_id: reopened.session.daemon_id.clone(),
                session_instance_id: alpha.session.session_instance_id,
            }
        )
        .ok
    );
    assert!(
        wire(
            &reopened.socket,
            Operation::ClientRegister {
                session_id: reopened.session.session_id.clone(),
                daemon_id: reopened.session.daemon_id.clone(),
                session_instance_id: reopened.session.session_instance_id.clone(),
            }
        )
        .ok
    );
    assert_eq!(snapshot(&beta.socket).panes.len(), 1);
    assert!(wire(&reopened.socket, Operation::Shutdown).ok);
    assert!(wire(&beta.socket, Operation::Shutdown).ok);
    let gamma = fixture.open("gamma", None);
    assert!(snapshot(&gamma.socket).panes.is_empty());
    fixture.finish().unwrap();
    assert!(!gamma.socket.exists());
    assert!(!beta.socket.exists());
    assert!(Registry::open_named(&fixture.root("alpha"), "alpha").is_ok());
}

#[test]
fn same_name_opening_is_single_flight_and_does_not_lock_unrelated_startup() {
    let mut fixture = Fixture::new();
    let (entered, opened) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let resume = Mutex::new(resume);
    *fixture.host.inner.opening.lock().unwrap() = Some(Arc::new(move |name, _| {
        if name == "alpha" {
            entered.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    }));
    let gate = Arc::new(std::sync::Barrier::new(3));
    let mut callers = Vec::new();
    for _ in 0..2 {
        let host = fixture.host.clone();
        let spec = fixture.spec("printf once >> runs; exec sleep 60");
        let gate = gate.clone();
        callers.push(thread::spawn(move || {
            gate.wait();
            host.open_session("alpha".into(), None, Some(spec))
        }));
    }
    gate.wait();
    opened.recv_timeout(Duration::from_secs(5)).unwrap();
    let beta = fixture.open("beta", None);
    assert!(snapshot(&beta.socket).panes.is_empty());
    release.send(()).unwrap();
    let first = callers.remove(0).join().unwrap().unwrap();
    let second = callers.remove(0).join().unwrap().unwrap();
    assert_eq!(first, second);
    assert_eq!(snapshot(&first.socket).panes.len(), 1);
    wait_for(&fixture.root.path().join("runs"), b"once");
    let unchanged = fixture
        .host
        .open_session(
            "alpha".into(),
            Some(SpawnDefaults {
                argv: vec!["/bad/new-default".into()],
                cwd: fixture.root.path().to_owned(),
                home: fixture.root.path().join("other-home"),
                isolated_environment: true,
            }),
            Some(fixture.spec("printf replayed >> runs; exec sleep 60")),
        )
        .unwrap();
    assert_eq!(unchanged, first);
    assert!(value(&first.socket, Operation::SessionInfo)["spawn_defaults"].is_null());
    assert_eq!(fs::read(fixture.root.path().join("runs")).unwrap(), b"once");
    fixture.finish().unwrap();
}

#[test]
fn failed_storage_and_exec_openings_leave_other_sessions_usable_and_release_ownership() {
    let mut fixture = Fixture::new();
    let stable = fixture.open("stable", Some(fixture.spec("exec sleep 60")));
    let bad_root = fixture.root("broken");
    fs::create_dir(&bad_root).unwrap();
    fs::set_permissions(&bad_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(bad_root.join("registry.json")).unwrap();
    assert!(
        fixture
            .host
            .open_session("broken".into(), None, None)
            .is_err()
    );
    fs::remove_dir(bad_root.join("registry.json")).unwrap();
    let recovered = fixture.open("broken", None);
    assert!(snapshot(&recovered.socket).panes.is_empty());
    let mut bad = fixture.spec("exec sleep 60");
    bad.argv = vec!["/no-such-bkpr-host-test-program".into()];
    assert!(
        fixture
            .host
            .open_session("exec-failure".into(), None, Some(bad))
            .is_err()
    );
    assert!(Registry::open_named(&fixture.root("exec-failure"), "exec-failure").is_ok());
    assert_eq!(snapshot(&stable.socket).panes.len(), 1);
    let foreign_root = fixture.root("foreign");
    let foreign = Registry::open_named(&foreign_root, "foreign").unwrap();
    assert!(
        fixture
            .host
            .open_session("foreign".into(), None, None)
            .is_err()
    );
    assert!(foreign.snapshot().unwrap().panes.is_empty());
    drop(foreign);
    fixture.open("foreign", None);
    fixture.finish().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn host_shutdown_retains_opening_lease_until_accepted_spawn_cleanup_finishes() {
    let mut fixture = Fixture::new();
    let (barrier_tx, barrier_rx) = mpsc::sync_channel(1);
    *fixture.host.inner.opening.lock().unwrap() = Some(Arc::new(move |name, registry| {
        if name == "pending" {
            barrier_tx
                .send((registry.clone(), registry.test_pause_next_spawn()))
                .unwrap();
        }
    }));
    let host = fixture.host.clone();
    let spec = fixture.spec("exec sleep 60");
    let opening = thread::spawn(move || host.open_session("pending".into(), None, Some(spec)));
    let (pending, (entered, release)) = barrier_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    fixture.host.request_shutdown();
    assert!(pending.test_wait_for_closing());
    assert!(Registry::open_named(&fixture.root("pending"), "pending").is_err());
    release.send(()).unwrap();
    drop(pending);
    assert!(opening.join().unwrap().is_err());
    fixture.finish().unwrap();
    let registry = Registry::open_named(&fixture.root("pending"), "pending").unwrap();
    assert!(
        !registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .any(|pane| pane.status == bkpr_protocol::PaneStatus::Running)
    );
    let info: bkpr_protocol::SessionInfo = serde_json::from_value(
        registry
            .request(Request::new(Operation::SessionInfo))
            .unwrap()
            .into_json()
            .unwrap()["session"]
            .clone(),
    )
    .unwrap();
    assert_ne!(info.daemon_id, fixture.host.info().daemon_id);
}

#[test]
fn pinned_host_shutdown_reports_cleanup_failure_after_reaping_every_session() {
    let mut fixture = Fixture::new();
    let broken = fixture.open("poisoned", Some(fixture.spec("exec sleep 60")));
    let stable = fixture.open("healthy", Some(fixture.spec("exec sleep 60")));
    let state = snapshot(&broken.socket);
    let pane = value(
        &broken.socket,
        Operation::Pane {
            pane_id: state.panes[0].id.clone(),
            request: bkpr_protocol::PaneRequest::Inspect,
        },
    );
    let root = fixture.root("poisoned");
    fs::remove_file(root.join("registry.json")).unwrap();
    fs::create_dir(root.join("registry.json")).unwrap();
    assert!(
        !wire(
            &broken.socket,
            Operation::WorkspaceCreate {
                name: Some("not durable".into()),
                initial: None
            }
        )
        .ok
    );
    assert_eq!(snapshot(&stable.socket).panes.len(), 1);
    let response = wire(
        &fixture.host.inner.config.socket,
        Operation::DaemonShutdown {
            daemon_id: fixture.host.info().daemon_id.clone(),
        },
    );
    assert!(
        !response.ok,
        "failed cleanup published successful host shutdown"
    );
    assert!(!broken.socket.exists());
    assert!(!stable.socket.exists());
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new("/proc")
            .join(pane["pid"].as_u64().unwrap().to_string())
            .exists()
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pane;
    assert!(fixture.finish().is_err());
    fs::remove_dir(root.join("registry.json")).unwrap();
    assert!(Registry::open_named(&root, "poisoned").is_ok());
}

#[test]
fn malformed_open_intents_are_rejected_before_creating_or_reconfiguring_a_session() {
    let mut fixture = Fixture::new();
    let active = fixture.open("active", None);
    let invalid = SpawnDefaults {
        argv: Vec::new(),
        cwd: fixture.root.path().to_owned(),
        home: fixture.root.path().join("home"),
        isolated_environment: true,
    };
    for name in ["active", "new"] {
        assert!(
            fixture
                .host
                .open_session(name.into(), Some(invalid.clone()), None)
                .is_err()
        );
        let mut initial = fixture.spec("exec sleep 60");
        initial.size.cols = 0;
        assert!(
            fixture
                .host
                .open_session(name.into(), None, Some(initial))
                .is_err()
        );
    }
    assert!(!fixture.root("new").exists());
    assert!(fixture.host.open_session("".into(), None, None).is_err());
    let info = value(&active.socket, Operation::SessionInfo);
    assert!(info["spawn_defaults"].is_null());
    assert_eq!(
        info["session"]["session_instance_id"],
        active.session.session_instance_id
    );
    assert!(snapshot(&active.socket).panes.is_empty());
    fixture.finish().unwrap();
}

#[test]
fn stopping_and_reopening_a_session_cancels_its_retained_partial_maximum_frame() {
    use std::io::{Read, Write};
    let mut fixture = Fixture::new();
    let old = fixture.open("partial-frame", None);
    let mut peer = UnixStream::connect(&old.socket).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let send_bytes = 4096_i32;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&peer),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&send_bytes as *const i32).cast(),
                std::mem::size_of::<i32>() as libc::socklen_t,
            )
        },
        0
    );
    let mut header =
        bkpr_protocol::wire::encode_frame(bkpr_protocol::wire::Kind::Request, &[]).unwrap();
    header[12..16].copy_from_slice(&(bkpr_protocol::wire::MAX_REQUEST as u32).to_le_bytes());
    assert_eq!(
        bkpr_protocol::wire::frame_payload_len(
            &header,
            bkpr_protocol::wire::Kind::Request,
            bkpr_protocol::wire::MAX_REQUEST
        )
        .unwrap(),
        bkpr_protocol::wire::MAX_REQUEST
    );
    peer.write_all(&header).unwrap();
    for _ in 0..16 {
        peer.write_all(&[0; 64 * 1024]).unwrap();
    }
    assert!(wire(&old.socket, Operation::Shutdown).ok);
    let reopened = fixture.open("partial-frame", None);
    assert_eq!(old.session.session_id, reopened.session.session_id);
    assert_ne!(
        old.session.session_instance_id,
        reopened.session.session_instance_id
    );
    assert_eq!(
        peer.read(&mut [0]).unwrap(),
        0,
        "stopped instance retained its partial-frame reader"
    );
    assert!(snapshot(&reopened.socket).panes.is_empty());
    assert!(
        wire(
            &reopened.socket,
            Operation::WorkspaceCreate {
                name: Some("usable".into()),
                initial: None
            }
        )
        .ok
    );
    fixture.finish().unwrap();
}
