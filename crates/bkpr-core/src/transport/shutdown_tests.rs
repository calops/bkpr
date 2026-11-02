use super::*;
use bkpr_protocol::{PaneSpawnSpec, Size};
use serde_json::Value;

fn wire(socket: &Path, request: Request) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    serde_json::to_writer(&mut stream, &request).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn explicit_shutdown_stops_poisoned_daemon_and_reaps_live_pane_even_when_response_is_error() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let home = root.path().join("home");
    let created = registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            spec: PaneSpawnSpec {
                argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                cwd: home.clone(),
                home,
                isolated_environment: true,
                title: Some("owned".into()),
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
            },
        }))
        .unwrap();
    let pane_id = created["pane_id"].as_str().unwrap().to_owned();
    let pid = registry
        .request(Request::new(Operation::Pane {
            pane_id,
            request: PaneRequest::Inspect,
        }))
        .unwrap()["pid"]
        .as_i64()
        .unwrap() as i32;
    let socket = root.path().join("daemon.sock");
    let worker_socket = socket.clone();
    let serving_registry = registry.clone();
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = serve(&worker_socket, serving_registry);
        let _ = finished_tx.send(result);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !socket.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not listen"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Force a real rename failure, then send the shutdown through the public wire.
    std::fs::remove_file(root.path().join("registry.json")).unwrap();
    std::fs::create_dir(root.path().join("registry.json")).unwrap();
    assert!(
        registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: Some("poison".into()),
                initial: None,
            }))
            .is_err()
    );
    let wrong_version = wire(
        &socket,
        Request {
            version: PROTOCOL_VERSION + 1,
            operation: Operation::Shutdown,
        },
    );
    assert_eq!(wrong_version["ok"], false);
    // Invalid versions must not trigger shutdown; the next connection still works.
    let inspect = wire(&socket, Request::new(Operation::Inspect));
    assert_eq!(inspect["ok"], false);
    let response = wire(&socket, Request::new(Operation::Shutdown));
    let result = finished_rx.recv_timeout(Duration::from_secs(5));
    if result.is_ok() {
        worker.join().unwrap();
    }
    assert_eq!(
        response["ok"], false,
        "durability failure must remain visible"
    );
    assert!(
        result
            .expect("valid shutdown left poisoned daemon listening")
            .is_err()
    );
    assert!(!socket.exists(), "daemon socket survived shutdown");
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "explicit shutdown did not reap its live child"
    );
}

fn native_spec(root: &Path) -> PaneSpawnSpec {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    PaneSpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
        cwd: home.clone(),
        home,
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
fn session_end_before_or_during_waker_registration_never_loses_shutdown() {
    for before_registration in [true, false] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let created = registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: Some(native_spec(root.path())),
            }))
            .unwrap();
        let close = Request::new(Operation::WorkspaceClose {
            workspace_id: created["workspace_id"].as_str().unwrap().into(),
            expected_revision: created["registry"]["workspaces"][0]["revision"]
                .as_u64()
                .unwrap(),
        });
        let socket = root.path().join("daemon.sock");
        let bound = bind(&socket).unwrap();
        if before_registration {
            registry.request(close.clone()).unwrap();
            assert!(registry.session_end_requested());
        }
        let serving = registry.clone();
        let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _ = finished_tx.send(bound.serve(serving));
        });
        if !before_registration {
            // Deliberately race the serve setup: registration must either observe
            // this committed flag or receive its callback; neither may be lost.
            registry.request(close).unwrap();
        }
        finished_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("session-end wake was lost")
            .unwrap();
        worker.join().unwrap();
        assert!(!socket.exists());
        // Reclaim the listener's independent socket lease, not just its pathname.
        drop(bind(&socket).unwrap());
    }
}

#[test]
fn last_window_close_returns_truthful_success_and_stops_without_followup_request() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let created = registry
        .request(Request::new(Operation::WorkspaceCreate {
            name: None,
            initial: Some(native_spec(root.path())),
        }))
        .unwrap();
    let socket = root.path().join("daemon.sock");
    let bound = bind(&socket).unwrap();
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let _ = finished_tx.send(bound.serve(registry));
    });
    let workspace = &created["registry"]["workspaces"][0];
    let window = &workspace["windows"][0];
    let response = wire(
        &socket,
        Request::new(Operation::WindowClose {
            workspace_id: workspace["id"].as_str().unwrap().into(),
            window_id: window["id"].as_str().unwrap().into(),
            expected_revision: window["revision"].as_u64().unwrap(),
        }),
    );
    assert_eq!(response["ok"], true, "{response}");
    finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("last close still needs client inspection")
        .unwrap();
    worker.join().unwrap();
    assert!(!socket.exists());
}

#[test]
fn session_shutdown_during_client_registration_preserves_response_then_lifetime_event() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let created = registry
        .request(Request::new(Operation::WorkspaceCreate {
            name: None,
            initial: Some(native_spec(root.path())),
        }))
        .unwrap();
    let identity = registry
        .request(Request::new(Operation::SessionInfo))
        .unwrap();
    let (mut server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (lease, response) = registry
        .register_client(
            &server,
            identity["session"]["session_id"].as_str().unwrap(),
            identity["session"]["daemon_id"].as_str().unwrap(),
        )
        .unwrap();
    // Freeze at the real control-loop boundary: registration has committed,
    // but the initial response and handshake_complete have not happened.
    let workspace = &created["registry"]["workspaces"][0];
    registry
        .request(Request::new(Operation::WorkspaceClose {
            workspace_id: workspace["id"].as_str().unwrap().into(),
            expected_revision: workspace["revision"].as_u64().unwrap(),
        }))
        .unwrap();
    assert!(registry.session_end_requested());
    registry.shutdown().unwrap();
    serde_json::to_writer(
        &mut server,
        &json!({
            "version": PROTOCOL_VERSION, "ok": true, "result": response,
        }),
    )
    .unwrap();
    server.write_all(b"\n").unwrap();
    lease.handshake_complete();
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let initial: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(initial["ok"], true);
    assert!(initial["result"]["client"].is_object());
    line.clear();
    reader.read_line(&mut line).unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(event["event"], "client_detached");
    assert_eq!(event["reason"], "shutdown");
    line.clear();
    assert_eq!(reader.read_line(&mut line).unwrap(), 0);
}

#[test]
fn review_regression_response_deadline_bounds_continuing_partial_progress() {
    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    let buffer: libc::c_int = 64 * 1024;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                writer.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&buffer as *const libc::c_int).cast(),
                std::mem::size_of_val(&buffer) as libc::socklen_t,
            )
        },
        0
    );
    let consumer = thread::spawn(move || {
        let mut bytes = [0; 64 * 1024];
        while reader.read(&mut bytes).unwrap_or(0) != 0 {
            thread::sleep(Duration::from_millis(20));
        }
    });
    let payload = vec![b'x'; 4 * 1024 * 1024];
    let result = write_response(&mut writer, &payload, Duration::from_millis(500));
    drop(writer);
    consumer.join().unwrap();
    let error = result.expect_err("partial progress reset the complete response deadline");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
}
