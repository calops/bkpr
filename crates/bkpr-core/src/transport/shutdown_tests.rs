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
                name: "poison".into()
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
