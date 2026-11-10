use super::*;
use bkpr_protocol::{Size, ViewSpec};
use std::collections::BTreeSet;
use std::sync::mpsc;

fn send(socket: &Path, operation: Operation) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut codec = Codec::new();
    wire::write_frame(
        &mut stream,
        Kind::Request,
        codec.encode_request(&Request::new(operation))?,
    )?;
    Ok(stream)
}
fn receive(stream: &mut UnixStream) -> Result<Response> {
    wire::decode_response(&wire::read_frame(stream, Kind::Response, MAX_RESPONSE)?)
}
fn call(socket: &Path, operation: Operation) -> Result<Payload> {
    let response = receive(&mut send(socket, operation)?)?;
    anyhow::ensure!(response.ok, "{response:?}");
    response.result.context("missing response payload")
}

fn topology(value: &serde_json::Value) -> serde_json::Value {
    let registry = &value["registry"];
    json!({
        "workspaces": registry["workspaces"].as_array().unwrap().iter().map(|workspace| {
            json!({
                "id": workspace["id"],
                "name": workspace["name"],
                "windows": workspace["windows"].as_array().unwrap().iter().map(|window| {
                    json!({"id": window["id"], "name": window["name"], "layout": window["layout"]})
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "panes": registry["panes"].as_array().unwrap().iter().map(|pane| {
            json!({"id": pane["id"], "workspace_id": pane["workspace_id"], "window_id": pane["window_id"]})
        }).collect::<Vec<_>>(),
    })
}

#[derive(Default)]
struct DispatchLatch {
    released: Mutex<bool>,
    changed: Condvar,
}
impl DispatchLatch {
    fn wait(&self) {
        let released = self.released.lock().unwrap();
        drop(
            self.changed
                .wait_while(released, |released| !*released)
                .unwrap(),
        );
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct Serving {
    registry: Registry,
    socket: PathBuf,
    latch: Arc<DispatchLatch>,
    worker: Option<thread::JoinHandle<Result<()>>>,
    _root: tempfile::TempDir,
}
impl Drop for Serving {
    fn drop(&mut self) {
        // Assertion unwinding must release every blocked worker before shutdown.
        self.latch.release();
        // Registry cleanup alone does not stop the bound listener. Use the
        // actual transport shutdown path before waiting for its serving thread.
        if self.worker.is_some() {
            let _ = call(&self.socket, Operation::Shutdown);
        }
        let _ = self.registry.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// A dedicated opt-in fixture avoids spending >250 OS threads alongside normal
// parallel unit tests. It uses the actual bound listener, frames and native pane.
#[test]
#[ignore = "isolated high-concurrency socket smoke; run with --ignored --test-threads=1"]
fn observations_beyond_old_socket_and_poll_counts_leave_controls_available() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    let home = root.path().join("home");
    let size = Size {
        cols: 8,
        rows: 2,
        cell_width: 8,
        cell_height: 16,
    };
    let created = registry
        .request(Request::new(Operation::WorkspaceCreate {
            name: Some("observations".into()),
            initial: Some(PaneSpawnSpec {
                argv: vec!["/bin/sh".into(), "-c".into(), "read line".into()],
                cwd: home.clone(),
                home,
                isolated_environment: true,
                title: Some("observation fixture".into()),
                size,
            }),
        }))
        .unwrap()
        .into_json()
        .unwrap();
    let pane_id = created["pane_id"].as_str().unwrap().to_owned();
    let socket = root.path().join("daemon.sock");
    let mut bound = bind(&socket).unwrap();
    let latch = Arc::new(DispatchLatch::default());
    let blocked = latch.clone();
    let (entered, received) = mpsc::channel();
    bound.dispatch_observer = Some(Arc::new(move |operation| {
        if matches!(
            operation,
            Operation::Wait { .. }
                | Operation::Pane {
                    request: PaneRequest::Poll { .. },
                    ..
                }
        ) {
            let _ = entered.send(());
            blocked.wait();
        }
    }));
    let serving = registry.clone();
    let worker = thread::spawn(move || bound.serve(serving));
    let mut fixture = Serving {
        registry,
        socket,
        latch,
        worker: Some(worker),
        _root: root,
    };
    let identity = call(&fixture.socket, Operation::SessionInfo)
        .unwrap()
        .into_json()
        .unwrap();
    let mut view_ids = BTreeSet::new();
    for _ in 0..140 {
        let attached = call(
            &fixture.socket,
            Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Attach {
                    spec: ViewSpec {
                        size,
                        visible: false,
                        interactive: false,
                        input: false,
                        clipboard: false,
                    },
                },
            },
        )
        .unwrap()
        .into_json()
        .unwrap();
        view_ids.insert(attached["view_id"].as_str().unwrap().to_owned());
    }
    let before = call(&fixture.socket, Operation::Inspect)
        .unwrap()
        .into_json()
        .unwrap();
    let revision = before["registry"]["revision"].as_u64().unwrap();
    let idle = (0..84)
        .map(|_| UnixStream::connect(&fixture.socket).unwrap())
        .collect::<Vec<_>>();
    let mut observations = Vec::new();
    for _ in 0..33 {
        observations.push((
            None,
            send(
                &fixture.socket,
                Operation::Wait {
                    after: revision,
                    wait_ms: 1000,
                },
            )
            .unwrap(),
        ));
    }
    for view_id in &view_ids {
        observations.push((
            Some(view_id.clone()),
            send(
                &fixture.socket,
                Operation::Pane {
                    pane_id: pane_id.clone(),
                    request: PaneRequest::Poll {
                        view_id: view_id.clone(),
                        after: 0,
                        frame_after: 0,
                        resources: Vec::new(),
                        snapshot_after: 0,
                        wait_ms: 1000,
                        row_deltas: false,
                        scene_encoding: None,
                    },
                },
            )
            .unwrap(),
        ));
    }
    for _ in &observations {
        received.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    // All observation requests are decoded and parked without a work permit.
    // 84 idle + 33 registry + 140 pane sockets already exceed the former total.
    assert_eq!(
        call(&fixture.socket, Operation::SessionInfo)
            .unwrap()
            .into_json()
            .unwrap(),
        identity
    );
    let inspect = call(&fixture.socket, Operation::Inspect)
        .unwrap()
        .into_json()
        .unwrap();
    let observed_revision = inspect["registry"]["revision"].as_u64().unwrap();
    assert!(observed_revision >= revision);
    assert_eq!(topology(&inspect), topology(&before));
    let control = call(
        &fixture.socket,
        Operation::Pane {
            pane_id: pane_id.clone(),
            request: PaneRequest::Inspect,
        },
    )
    .unwrap()
    .into_json()
    .unwrap();
    let control_views = control["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|view| view["id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(control_views, view_ids);
    // A disconnected observation must not prevent another consumer's reply.
    let (cancelled_view, cancelled) = observations.pop().unwrap();
    assert!(cancelled_view.is_some());
    drop(cancelled);
    fixture.latch.release();
    for (view_id, mut stream) in observations {
        let reply = receive(&mut stream).unwrap();
        assert!(reply.ok, "{reply:?}");
        assert!(reply.failure.is_none(), "{reply:?}");
        let result = reply.result.unwrap().into_json().unwrap();
        if let Some(view_id) = view_id {
            assert_eq!(result["pane_id"], pane_id);
            assert!(result["revision"].as_u64().unwrap() >= control["revision"].as_u64().unwrap());
            assert!(
                result["views"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|view| view["id"] == view_id)
            );
        } else {
            assert!(result["registry"]["revision"].as_u64().unwrap() >= observed_revision);
            assert_eq!(topology(&result), topology(&before));
        }
    }
    drop(idle);
    assert_eq!(
        call(&fixture.socket, Operation::SessionInfo)
            .unwrap()
            .into_json()
            .unwrap(),
        identity
    );
    call(&fixture.socket, Operation::Shutdown).unwrap();
    fixture.worker.take().unwrap().join().unwrap().unwrap();
}
