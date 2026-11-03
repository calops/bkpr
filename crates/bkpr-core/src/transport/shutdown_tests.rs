use super::*;
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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
    let identity = wire(&socket, Request::new(Operation::SessionInfo));
    assert_eq!(identity["ok"], true, "{identity}");
    let expected_identity: bkpr_protocol::SessionInfo =
        serde_json::from_value(identity["result"]["session"].clone()).unwrap();
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
    assert!(inspect["failure"].is_null(), "{inspect}");
    // The normal stop path validates immutable session/daemon identity before
    // issuing Shutdown; a poisoned registry must not strand that caller.
    let validated = wire(&socket, Request::new(Operation::SessionInfo));
    assert_eq!(validated["ok"], true, "{validated}");
    assert!(validated["failure"].is_null(), "{validated}");
    let actual_identity: bkpr_protocol::SessionInfo =
        serde_json::from_value(validated["result"]["session"].clone()).unwrap();
    assert_eq!(actual_identity, expected_identity);
    let response = wire(&socket, Request::new(Operation::Shutdown));
    let result = finished_rx.recv_timeout(Duration::from_secs(5));
    if result.is_ok() {
        worker.join().unwrap();
    }
    assert_eq!(
        response["ok"], false,
        "durability failure must remain visible"
    );
    assert!(response["failure"].is_null(), "{response}");
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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
    assert!(response["failure"].is_null(), "{response}");
    assert_eq!(response["result"]["registry"]["workspaces"], json!([]));
    finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("last close still needs client inspection")
        .unwrap();
    worker.join().unwrap();
    assert!(!socket.exists());
}

#[cfg(target_os = "linux")]
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

#[test]
fn closing_admission_rejects_the_specific_request_without_changing_durable_intent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let before = registry.snapshot().unwrap().revision;
    let socket = root.path().join("daemon.sock");
    let bound = bind(&socket).unwrap();
    // Keep the listener alive while the authoritative backend is already closing.
    registry.shutdown().unwrap();
    let serving = registry.clone();
    let worker = thread::spawn(move || bound.serve(serving));
    let rejected: Response = serde_json::from_value(wire(
        &socket,
        Request::new(Operation::WorkspaceCreate {
            name: Some("never-dispatched".into()),
            initial: None,
        }),
    ))
    .unwrap();
    assert!(!rejected.ok);
    assert!(rejected.result.is_none());
    assert!(rejected.error.is_some());
    assert_eq!(
        rejected.failure,
        Some(RequestFailure::SessionClosing {
            outcome: RequestOutcome::NotDispatched,
        })
    );
    let after = registry.snapshot().unwrap();
    assert_eq!(after.revision, before);
    assert!(after.workspaces.is_empty());
    // A protocol error is still a protocol error while this same daemon closes.
    let invalid: Response = serde_json::from_value(wire(
        &socket,
        Request {
            version: PROTOCOL_VERSION + 1,
            operation: Operation::Inspect,
        },
    ))
    .unwrap();
    assert!(!invalid.ok);
    assert!(invalid.failure.is_none());
    assert_eq!(wire(&socket, Request::new(Operation::Shutdown))["ok"], true);
    worker.join().unwrap().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn in_flight_spawn_closing_is_unknown_but_an_actual_spawn_failure_stays_an_error() {
    for invalid_executable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let socket = root.path().join("daemon.sock");
        let bound = bind(&socket).unwrap();
        let serving = registry.clone();
        let worker = thread::spawn(move || bound.serve(serving));
        let (entered, resume) = registry.test_pause_next_spawn();
        let mut spec = native_spec(root.path());
        if invalid_executable {
            spec.argv = vec![
                root.path()
                    .join("missing-executable")
                    .to_str()
                    .unwrap()
                    .into(),
            ];
        }
        let spawning_socket = socket.clone();
        let spawning = thread::spawn(move || {
            wire(
                &spawning_socket,
                Request::new(Operation::PaneSpawn {
                    workspace_id: None,
                    window_id: None,
                    expected_revision: None,
                    spec,
                }),
            )
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let stopping_socket = socket.clone();
        let stopping =
            thread::spawn(move || wire(&stopping_socket, Request::new(Operation::Shutdown)));
        let closing = registry.test_wait_for_closing();
        // Release before assertions so fixture cleanup cannot strand its owned spawn.
        resume.send(()).unwrap();
        let spawned: Response = serde_json::from_value(spawning.join().unwrap()).unwrap();
        let stopped: Response = serde_json::from_value(stopping.join().unwrap()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(closing);
        assert!(!spawned.ok);
        assert!(spawned.result.is_none());
        assert!(spawned.error.is_some());
        assert_eq!(
            spawned.failure,
            if invalid_executable {
                None
            } else {
                Some(RequestFailure::SessionClosing {
                    outcome: RequestOutcome::Unknown,
                })
            }
        );
        assert!(stopped.ok, "{stopped:?}");
        assert!(stopped.failure.is_none());
        assert!(registry.snapshot().unwrap().panes.is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn pending_registry_read_and_last_pane_or_document_close_keep_their_successful_replies() {
    for terminate_pane in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let created = registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: None,
                initial: Some(native_spec(root.path())),
            }))
            .unwrap();
        let after = created["registry"]["revision"].as_u64().unwrap();
        let socket = root.path().join("daemon.sock");
        let bound = bind(&socket).unwrap();
        let serving = registry.clone();
        let worker = thread::spawn(move || bound.serve(serving));
        let entered = registry.test_next_request_wait();
        let waiting_socket = socket.clone();
        let waiting = thread::spawn(move || {
            wire(
                &waiting_socket,
                Request::new(Operation::Wait {
                    after,
                    wait_ms: 1000,
                }),
            )
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let close = if terminate_pane {
            Operation::PaneTerminate {
                pane_id: created["pane_id"].as_str().unwrap().into(),
            }
        } else {
            let workspace = &created["registry"]["workspaces"][0];
            Operation::WorkspaceClose {
                workspace_id: workspace["id"].as_str().unwrap().into(),
                expected_revision: workspace["revision"].as_u64().unwrap(),
            }
        };
        let closed: Response = serde_json::from_value(wire(&socket, Request::new(close))).unwrap();
        let waited: Response = serde_json::from_value(waiting.join().unwrap()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(closed.ok, "{closed:?}");
        assert!(closed.failure.is_none());
        assert_eq!(closed.result.unwrap()["registry"]["workspaces"], json!([]));
        assert!(waited.ok, "{waited:?}");
        assert!(waited.failure.is_none());
        // A finite read timeout may win before native close finishes; either
        // valid snapshot must remain a successful read, not become a mutation error.
        assert!(
            waited.result.unwrap()["registry"]["revision"]
                .as_u64()
                .unwrap()
                >= after
        );
        assert!(registry.session_end_requested());
    }
}
