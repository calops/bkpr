use super::*;

fn target() -> RemoteTarget {
    RemoteTarget {
        destination: "user@fixture-alias".into(),
        session_name: "../exact session 🐈".into(),
        helper: None,
        ssh_config: None,
        forward_agent: false,
        detach_others: false,
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn inspection_rejects_attachment_side_effects_before_transport_start() {
    let mut forwarded = target();
    forwarded.forward_agent = true;
    let error = RemoteSession::inspect(forwarded).unwrap_err();
    assert!(error.to_string().contains("cannot forward an SSH agent"));
    let mut takeover = target();
    takeover.detach_others = true;
    let error = RemoteSession::inspect(takeover).unwrap_err();
    assert!(error.to_string().contains("cannot detach other clients"));
}

#[test]
fn helper_shell_invocation_preserves_literal_executable_and_fixed_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory
        .path()
        .join("helper's ;$(touch injected) executable");
    std::fs::write(&executable, b"#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut target = target();
    target.helper = Some(executable);
    target.session_name = "'; touch injected; #".into();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(helper_command(&target).unwrap())
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"__remote-helper\0--stdio\0");
    assert!(!directory.path().join("injected").exists());
}

#[test]
fn destination_and_helper_injection_are_rejected_before_spawn() {
    for destination in [
        "",
        "-oProxyCommand=bad",
        "alias\ncommand",
        "alias command",
        "alias\0bad",
    ] {
        let mut target = target();
        target.destination = destination.into();
        assert!(validate_target(&target).is_err());
    }
    let mut target = target();
    target.helper = Some("/helper\nbad".into());
    assert!(validate_target(&target).is_err());
    assert!(validate_target(&super::tests::target()).is_ok());
}

struct MuxFixture {
    _drivers: JoinSet<Result<()>>,
    _client_opens: mpsc::Sender<MuxOpen>,
    _server_opens: mpsc::Sender<MuxOpen>,
}
async fn streams() -> (MuxFixture, yamux::Stream, yamux::Stream) {
    let (left, right) = tokio::io::duplex(CHUNK * 2);
    let (client_opens, client_rx) = mpsc::channel(STREAMS);
    let (server_opens, server_rx) = mpsc::channel(STREAMS);
    let (client_in, _client_in_rx) = mpsc::channel(STREAMS);
    let (server_in, mut server_in_rx) = mpsc::channel(STREAMS);
    let mut drivers = JoinSet::new();
    drivers.spawn(drive_mux(
        yamux::Connection::new(left.compat(), mux_config(), yamux::Mode::Client),
        client_rx,
        client_in,
    ));
    drivers.spawn(drive_mux(
        yamux::Connection::new(right.compat(), mux_config(), yamux::Mode::Server),
        server_rx,
        server_in,
    ));
    let mut left = open_mux(&client_opens).await.unwrap();
    futures::io::AsyncWriteExt::write_all(&mut left, b"S")
        .await
        .unwrap();
    let mut right = timeout(OPEN_TIMEOUT, server_in_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let mut sentinel = [0];
    futures::io::AsyncReadExt::read_exact(&mut right, &mut sentinel)
        .await
        .unwrap();
    assert_eq!(&sentinel, b"S");
    (
        MuxFixture {
            _drivers: drivers,
            _client_opens: client_opens,
            _server_opens: server_opens,
        },
        left,
        right,
    )
}
fn socket_pair() -> (tokio::net::UnixStream, tokio::net::UnixStream) {
    let (left, right) = UnixStream::pair().unwrap();
    socket_bounds(&left).unwrap();
    socket_bounds(&right).unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    (
        tokio::net::UnixStream::from_std(left).unwrap(),
        tokio::net::UnixStream::from_std(right).unwrap(),
    )
}

#[test]
fn opaque_reply_larger_than_receive_window_drains_without_truncation() {
    runtime().block_on(async {
        let (_fixture, left, right) = streams().await;
        let (mut caller, local_relay) = socket_pair();
        let (native_relay, mut daemon) = socket_pair();
        let mut bridges = JoinSet::new();
        bridges.spawn(bridge(local_relay, left, false));
        bridges.spawn(bridge(native_relay, right, true));
        let request: Vec<u8> = (0..65537).map(|index| (index % 251) as u8).collect();
        let response: Vec<u8> = (0..(2 * yamux::DEFAULT_CREDIT as usize + 43))
            .map(|index| (index % 239) as u8)
            .collect();
        let expected_request = request.clone();
        let expected_response = response.clone();
        let server = async move {
            let mut bytes = vec![0; expected_request.len()];
            daemon.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, expected_request);
            daemon.write_all(&response).await.unwrap();
            drop(daemon);
        };
        let client = async move {
            caller.write_all(&request).await.unwrap();
            let mut received = Vec::new();
            caller.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, expected_response);
        };
        timeout(OPEN_TIMEOUT, async {
            tokio::join!(server, client);
        })
        .await
        .unwrap();
    });
}

#[test]
fn cancelled_local_pair_resets_remote_stream_and_closes_native_uds() {
    runtime().block_on(async {
        let (_fixture, left, right) = streams().await;
        let (mut caller, local_relay) = socket_pair();
        let (native_relay, mut daemon) = socket_pair();
        let mut bridges = JoinSet::new();
        bridges.spawn(bridge(local_relay, left, false));
        bridges.spawn(bridge(native_relay, right, true));
        caller.write_all(b"blocked-observation").await.unwrap();
        let mut request = [0; 19];
        daemon.read_exact(&mut request).await.unwrap();
        drop(caller);
        let mut byte = [0];
        assert_eq!(
            timeout(OPEN_TIMEOUT, daemon.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
}

#[test]
fn connection_death_drops_all_native_pairs_without_replay() {
    runtime().block_on(async {
        let (fixture, left, right) = streams().await;
        let (mut caller, local_relay) = socket_pair();
        let (native_relay, mut daemon) = socket_pair();
        let mut bridges = JoinSet::new();
        bridges.spawn(bridge(local_relay, left, false));
        bridges.spawn(bridge(native_relay, right, true));
        caller.write_all(b"accepted-once").await.unwrap();
        let mut request = [0; 13];
        daemon.read_exact(&mut request).await.unwrap();
        drop(fixture);
        let mut byte = [0];
        assert_eq!(
            timeout(OPEN_TIMEOUT, caller.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(
            timeout(OPEN_TIMEOUT, daemon.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(&request, b"accepted-once");
    });
}

#[test]
fn helper_receive_lease_expires_without_parent_ping() {
    runtime().block_on(async {
        let (_parent, helper) = tokio::io::duplex(CHUNK);
        let error = timeout(
            Duration::from_secs(1),
            helper_lease(helper, Duration::from_millis(20)),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(format!("{error:#}").contains("receive lease expired"));
    });
}

#[test]
fn helper_heartbeat_rejects_replay_instead_of_renewing_lease() {
    runtime().block_on(async {
        let (mut parent, helper) = tokio::io::duplex(CHUNK);
        let task = tokio::spawn(helper_lease(helper, Duration::from_secs(1)));
        write_control(&mut parent, &Control::Ping { sequence: 1 })
            .await
            .unwrap();
        assert!(matches!(
            read_control(&mut parent).await.unwrap(),
            Control::Pong { sequence: 1 }
        ));
        write_control(&mut parent, &Control::Ping { sequence: 1 })
            .await
            .unwrap();
        let error = timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid remote parent control"));
    });
}

#[test]
fn malformed_control_is_bounded_before_payload_allocation() {
    runtime().block_on(async {
        let (mut parent, mut helper) = tokio::io::duplex(CHUNK);
        // A full invalid header cannot become an enormous body read/allocation.
        parent
            .write_all(&[0xff; remote_wire::HEADER_LEN])
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_secs(1), read_control(&mut helper))
                .await
                .unwrap()
                .is_err()
        );
    });
}

#[test]
fn client_ping_schedule_does_not_wait_for_previous_pong() {
    runtime().block_on(async {
        let (client, mut helper) = tokio::io::duplex(CHUNK);
        let task = tokio::spawn(client_lease(
            client,
            Duration::from_millis(10),
            Duration::from_secs(1),
        ));
        assert!(matches!(
            read_control(&mut helper).await.unwrap(),
            Control::Ping { sequence: 1 }
        ));
        // Withholding Pong(1) cannot delay the next scheduled Ping.
        assert!(matches!(
            timeout(Duration::from_millis(250), read_control(&mut helper))
                .await
                .unwrap()
                .unwrap(),
            Control::Ping { sequence: 2 }
        ));
        write_control(&mut helper, &Control::Pong { sequence: 1 })
            .await
            .unwrap();
        write_control(&mut helper, &Control::Pong { sequence: 2 })
            .await
            .unwrap();
        task.abort();
        let _ = task.await;
    });
}

#[test]
fn transport_failure_retires_every_endpoint_without_reconnect_or_empty_inspection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Once {
        stream: Mutex<Option<UnixStream>>,
        opens: AtomicUsize,
    }
    impl Connector for Once {
        fn remote(&self) -> bool {
            true
        }
        fn connect(&self, _: ConnectionPurpose) -> Result<UnixStream> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.stream
                .lock()
                .unwrap()
                .take()
                .context("unexpected reconnect")
        }
    }
    let (lifetime, mut daemon) = UnixStream::pair().unwrap();
    let connector = Arc::new(Once {
        stream: Mutex::new(Some(lifetime)),
        opens: AtomicUsize::new(0),
    });
    let identity = SessionInfo {
        name: "remote".into(),
        session_id: "existing-session".into(),
        daemon_id: "existing-daemon".into(),
    };
    let expected = identity.clone();
    let (close, close_now) = sync::sync_channel(1);
    let server =
        thread::spawn(move || {
            let request = crate::transport::test_wire::read_request(&mut daemon).unwrap();
            assert!(matches!(
                request.operation,
                bkpr_protocol::Operation::ClientRegister { .. }
            ));
            crate::transport::test_wire::write_response(&mut daemon, &bkpr_protocol::Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(serde_json::json!({
                "session": expected,
                "client": {"client_id": "remote-attachment", "token": "test-only-token"},
            }).into()),
            error: None,
            failure: None,
        }).unwrap();
            close_now.recv_timeout(Duration::from_secs(3)).unwrap();
        });
    let client = RegisteredClient::register_connector(connector.clone(), identity, None).unwrap();
    let (stop, _receiver) = watch::channel(false);
    let mut remote = RemoteSession {
        client,
        supervisor: Supervisor {
            stop,
            thread: None,
            failure: Arc::new(Mutex::new(Some("SSH connection lost: fixture".into()))),
        },
    };
    let endpoint = remote.endpoint();
    let worker_endpoint = endpoint.clone();
    close.send(()).unwrap();
    server.join().unwrap();
    let error = remote.poll_lifecycle().unwrap_err();
    assert!(format!("{error:#}").contains("SSH connection lost: fixture"));
    assert!(endpoint.is_revoked());
    assert!(
        worker_endpoint
            .checked(bkpr_protocol::Operation::Inspect)
            .is_err()
    );
    assert!(
        worker_endpoint
            .checked(bkpr_protocol::Operation::ClientActivate {
                detach_others: true,
                agent_socket: None,
            })
            .is_err()
    );
    assert_eq!(connector.opens.load(Ordering::SeqCst), 1);
    remote.disconnect().unwrap();
}
