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
            cancellation: CancellationToken::new(),
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

#[test]
fn cancellation_kills_and_reaps_stopped_owned_ssh_before_welcome() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("ssh.pid");
    runtime().block_on(async {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf '%s' $$ > \"$1\"; kill -STOP $$", "fake-ssh"])
            .arg(&pid_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (_, receiver) = mpsc::channel(STREAMS);
        let (_stop, stop_rx) = watch::channel(false);
        let (ready, ready_rx) = sync::sync_channel(1);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_ssh(
            command,
            Hello {
                session_name: "existing".into(),
                native_family: PROTOCOL_VERSION,
                required_capabilities: wire::SUPPORTED_CAPABILITIES,
                connection_nonce: [7; 32],
                forward_agent: false,
            },
            receiver,
            stop_rx,
            ready,
            cancellation.clone(),
        ));
        let pid = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.parse::<libc::pid_t>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut status = 0;
                let observed =
                    unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
                if observed == pid && libc::WIFSTOPPED(status) {
                    break;
                }
                assert!(observed >= 0, "owned SSH exited before its stop");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        cancellation.cancel();
        let error = timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("owned SSH transport cancelled"));
        assert!(ready_rx.recv_timeout(Duration::from_millis(50)).is_err());
        let exists = unsafe { libc::kill(pid, 0) };
        let error = std::io::Error::last_os_error();
        assert_eq!(exists, -1, "owned SSH was not reaped");
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
    });
}

#[test]
fn abandoned_transport_completion_cancels_without_joining_ssh_cleanup() {
    let cancellation = CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let (started, started_rx) = sync::sync_channel(1);
    let (finished, finished_rx) = sync::sync_channel(1);
    let (release, blocked) = sync::sync_channel(1);
    let (dropped, dropped_rx) = sync::sync_channel(1);
    let worker = thread::spawn(move || {
        runtime().block_on(worker_cancellation.cancelled());
        started.send(()).unwrap();
        // Model child wait / inherited-stderr cleanup after cancellation.
        let _ = blocked.recv();
        finished.send(()).unwrap();
    });
    let (stop, _stop_rx) = watch::channel(false);
    let supervisor = Supervisor {
        stop,
        thread: Some(worker),
        cancellation: cancellation.clone(),
        failure: Arc::new(Mutex::new(None)),
    };
    let dropper = thread::spawn(move || {
        drop(supervisor);
        let _ = dropped.send(());
    });
    let completed = dropped_rx.recv_timeout(Duration::from_secs(1));
    let cancelled = cancellation.is_cancelled();
    // Release both workers even if Drop failed either invariant.
    cancellation.cancel();
    let _ = release.send(());
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    dropper.join().unwrap();
    assert!(completed.is_ok(), "Drop joined blocked SSH cleanup");
    assert!(cancelled, "Drop did not cancel owned transport");
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_kills_stopped_proxy_descendant_without_touching_unrelated_child() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("proxy.pids");
    runtime().block_on(async {
        let mut unrelated = Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "sh -c 'kill -STOP $$; exec sleep 60' & nested=$!; printf '%s %s' $$ \"$nested\" > \"$1\"; kill -STOP $$; wait",
                "fake-ssh-proxy",
            ])
            .arg(&pid_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (_sender, receiver) = mpsc::channel(STREAMS);
        let (_stop, stop_rx) = watch::channel(false);
        let (ready, _ready_rx) = sync::sync_channel(1);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_ssh(
            command,
            Hello {
                session_name: "existing".into(),
                native_family: PROTOCOL_VERSION,
                required_capabilities: wire::SUPPORTED_CAPABILITIES,
                connection_nonce: [9; 32],
                forward_agent: false,
            },
            receiver,
            stop_rx,
            ready,
            cancellation.clone(),
        ));
        let (leader, nested) = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file) {
                    let mut pids = text.split_whitespace().filter_map(|s| s.parse::<libc::pid_t>().ok());
                    if let (Some(leader), Some(nested)) = (pids.next(), pids.next()) {
                        break (leader, nested);
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                let stopped = [leader, nested].into_iter().all(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/status"))
                        .is_ok_and(|status| status.lines().any(|line| line.starts_with("State:\tT")))
                });
                if stopped { break; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        assert_eq!(unsafe { libc::getpgid(leader) }, leader);
        assert_eq!(unsafe { libc::getpgid(nested) }, leader);
        assert_ne!(unsafe { libc::getpgrp() }, leader);
        cancellation.cancel();
        timeout(Duration::from_secs(1), task).await.unwrap().unwrap().unwrap_err();
        assert_eq!(unsafe { libc::kill(leader, 0) }, -1, "proxy leader not reaped");
        timeout(Duration::from_secs(1), async {
            loop {
                match std::fs::read_to_string(format!("/proc/{nested}/status")) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Ok(status) if status.lines().any(|line| line.starts_with("State:\tZ")) => {
                        // Grandchildren are reaped by their new host parent,
                        // not by this client's waitpid authority.
                        break;
                    }
                    _ => tokio::time::sleep(Duration::from_millis(5)).await,
                }
            }
        }).await.unwrap();
        assert!(unrelated.try_wait().unwrap().is_none(), "unrelated process was killed");
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    });
}

#[test]
fn authoritative_shutdown_drains_original_mutation_receipt_after_owner_drop() {
    async fn native_request(
        stream: &mut (impl tokio::io::AsyncRead + Unpin),
    ) -> bkpr_protocol::Request {
        let mut header = [0; wire::HEADER_LEN];
        stream.read_exact(&mut header).await.unwrap();
        let length =
            wire::frame_payload_len(&header, wire::Kind::Request, wire::MAX_REQUEST).unwrap();
        let mut payload = vec![0; length];
        stream.read_exact(&mut payload).await.unwrap();
        wire::decode_request(&payload).unwrap()
    }
    fn response(value: serde_json::Value) -> Vec<u8> {
        let mut codec = wire::Codec::new();
        wire::encode_frame(
            wire::Kind::Response,
            codec
                .encode_response(&bkpr_protocol::Response {
                    version: PROTOCOL_VERSION,
                    ok: true,
                    result: Some(value.into()),
                    error: None,
                    failure: None,
                })
                .unwrap(),
        )
        .unwrap()
    }
    let identity = SessionInfo {
        name: "remote".into(),
        session_id: "persistent".into(),
        daemon_id: "daemon".into(),
    };
    let expected = identity.clone();
    let (sender, receiver) = mpsc::channel(STREAMS - 1);
    let connector = Arc::new(RemoteConnector {
        sender,
        lifetime: Arc::new(Semaphore::new(1)),
        control: Arc::new(Semaphore::new(15)),
        observations: Arc::new(Semaphore::new(79)),
    });
    let (stop, stop_rx) = watch::channel(false);
    let cancellation = CancellationToken::new();
    let actor_cancellation = cancellation.clone();
    let (ready, ready_rx) = sync::sync_channel(1);
    let (accepted, accepted_rx) = sync::sync_channel(1);
    let (release, release_rx) = oneshot::channel();
    let (finished, finished_rx) = sync::sync_channel(1);
    let thread = thread::spawn(move || {
        let result = runtime().block_on(async {
            let (client_io, server_io) = tokio::io::duplex(CHUNK * 2);
            let (read, write) = tokio::io::split(client_io);
            let client = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = actor_cancellation.cancelled() => bail!("premature transport cancellation"),
                    result = client_transport(
                        Duplex { read, write },
                        Hello {
                            session_name: "remote".into(),
                            native_family: PROTOCOL_VERSION,
                            required_capabilities: wire::SUPPORTED_CAPABILITIES,
                            connection_nonce: [3; 32],
                            forward_agent: false,
                        },
                        receiver, stop_rx, ready,
                    ) => result,
                }
            });
            let (_opens, open_rx) = mpsc::channel(STREAMS);
            let (incoming, mut incoming_rx) = mpsc::channel(STREAMS);
            let mut drivers = JoinSet::new();
            drivers.spawn(drive_mux(
                yamux::Connection::new(server_io.compat(), mux_config(), yamux::Mode::Server),
                open_rx,
                incoming,
            ));
            let mut control = incoming_rx.recv().await.unwrap().compat();
            let Control::Hello(hello) = read_control(&mut control).await? else {
                bail!("missing Hello");
            };
            write_control(
                &mut control,
                &Control::Welcome(Welcome {
                    session: expected.clone(),
                    connection_nonce: hello.connection_nonce,
                    capabilities: wire::SUPPORTED_CAPABILITIES,
                    agent_socket: None,
                    heartbeat_interval_ms: HEARTBEAT_MS,
                    lease_timeout_ms: LEASE_MS,
                }),
            )
            .await?;
            let mut lifetime = incoming_rx.recv().await.unwrap().compat();
            assert!(matches!(
                native_request(&mut lifetime).await.operation,
                bkpr_protocol::Operation::ClientRegister { .. }
            ));
            lifetime
                .write_all(&response(serde_json::json!({
                    "session": expected,
                    "client": {"client_id": "attachment", "token": "test-only"},
                })))
                .await?;
            let mut mutation = incoming_rx.recv().await.unwrap().compat();
            assert!(matches!(native_request(&mut mutation).await.operation,
                bkpr_protocol::Operation::Client { request, .. }
                if matches!(*request, bkpr_protocol::Operation::WorkspaceRename { .. })));
            let mut codec = wire::Codec::new();
            let event = wire::encode_frame(
                wire::Kind::ClientEvent,
                codec.encode_client_event(&bkpr_protocol::ClientEvent {
                    event: "client_detached".into(),
                    reason: "shutdown".into(),
                })?,
            )?;
            lifetime.write_all(&event).await?;
            accepted.send(()).unwrap();
            release_rx.await?;
            mutation
                .write_all(&response(serde_json::json!({"committed": true})))
                .await?;
            mutation.shutdown().await?;
            timeout(Duration::from_secs(2), client).await???;
            Ok::<_, anyhow::Error>(())
        });
        finished.send(result).unwrap();
    });
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    let client = RegisteredClient::register_connector(connector, identity, None).unwrap();
    let mut remote = RemoteSession {
        client,
        supervisor: Supervisor {
            stop,
            thread: Some(thread),
            cancellation: cancellation.clone(),
            failure: Arc::new(Mutex::new(None)),
        },
    };
    let endpoint = remote.endpoint();
    let mutation = thread::spawn(move || {
        endpoint.checked(bkpr_protocol::Operation::WorkspaceRename {
            workspace_id: "workspace".into(),
            expected_revision: 1,
            name: "renamed".into(),
        })
    });
    accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let lifecycle = loop {
        let lifecycle = remote.poll_lifecycle().unwrap();
        if lifecycle != Lifecycle::Active {
            break lifecycle;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "native retirement did not reach the lifetime socket"
        );
        let mut fd = libc::pollfd {
            fd: remote.lifetime_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        assert!(ready > 0, "native lifetime readiness failed: {ready}");
    };
    assert_eq!(
        lifecycle,
        Lifecycle::Detached {
            reason: "shutdown".into()
        }
    );
    drop(remote);
    assert!(
        !cancellation.is_cancelled(),
        "owner drop cancelled accepted receipt drain"
    );
    release.send(()).unwrap();
    assert_eq!(
        mutation.join().unwrap().unwrap(),
        serde_json::json!({"committed": true})
    );
    finished_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
}

#[test]
fn failed_ssh_admission_preserves_callers_generation_token_and_failure_detail() {
    let parent = CancellationToken::new();
    let attempt = parent.clone();
    let (completed, completion) = sync::sync_channel(1);
    let worker = thread::spawn(move || {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf '%s\\n' 'missing remote helper' >&2; exit 127"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        completed
            .send(RemoteSession::establish_command(target(), attempt, command).map(|_| ()))
            .unwrap();
    });
    let error = completion
        .recv_timeout(Duration::from_secs(2))
        .expect("failed SSH admission did not complete promptly")
        .expect_err("missing helper unexpectedly established a transport");
    worker.join().unwrap();
    assert!(
        !parent.is_cancelled(),
        "transport cleanup cancelled the caller's generation"
    );
    assert!(format!("{error:#}").contains("missing remote helper"));
}
