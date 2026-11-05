#[test]
fn initial_snapshot_shutdown_and_rollback_failures_keep_separate_provenance() {
    use bkpr_protocol::{RequestFailure, RequestOutcome, Response};
    use std::{os::unix::net::UnixListener, thread, time::Duration};
    for (malformed_snapshot, cleanup_outcome, clean) in [
        (false, Some(RequestOutcome::NotDispatched), true),
        (true, Some(RequestOutcome::NotDispatched), false),
        (false, None, false),
        (false, Some(RequestOutcome::Unknown), false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("attach.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            for step in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = transport::test_wire::read_request(&mut stream).unwrap();
                assert!(matches!(
                    (step, request.operation),
                    (
                        0,
                        Operation::Pane {
                            request: PaneRequest::Attach { .. },
                            ..
                        }
                    ) | (
                        1,
                        Operation::Pane {
                            request: PaneRequest::Poll { .. },
                            ..
                        }
                    ) | (
                        2,
                        Operation::Pane {
                            request: PaneRequest::Detach { .. },
                            ..
                        }
                    )
                ));
                let reply = match step {
                    0 => Response {
                        version: PROTOCOL_VERSION,
                        ok: true,
                        result: Some(serde_json::json!({"view_id":"admitted-view"}).into()),
                        error: None,
                        failure: None,
                    },
                    1 if malformed_snapshot => Response {
                        version: PROTOCOL_VERSION,
                        ok: true,
                        result: Some(bkpr_protocol::Payload::Pane(bkpr_protocol::PanePayload {
                            metadata: serde_json::json!({"invalid_snapshot":true}),
                            scene: None,
                        })),
                        error: None,
                        failure: None,
                    },
                    _ => Response {
                        version: PROTOCOL_VERSION,
                        ok: false,
                        result: None,
                        error: Some("operation could not complete".into()),
                        failure: if step == 1 {
                            Some(RequestFailure::SessionClosing {
                                outcome: RequestOutcome::Unknown,
                            })
                        } else {
                            cleanup_outcome
                                .map(|outcome| RequestFailure::SessionClosing { outcome })
                        },
                    },
                };
                transport::test_wire::write_response(&mut stream, &reply).unwrap();
            }
        });
        let (mut app, _received) = fixture();
        attach_fixture(&mut app);
        let spec = app.views["focused-pane"].spec.clone();
        let endpoint = Endpoint::unmanaged(&path);
        let error = attach(&endpoint, "pane", spec).err().unwrap();
        assert_eq!(transport::expected_shutdown(&error, true), clean);
        assert_eq!(transport::outcome_unknown(&error), !malformed_snapshot);
        assert!(matches!(
            error.downcast_ref::<AttachOwnership>(),
            Some(AttachOwnership::Unconfirmed)
        ));
        server.join().unwrap();
    }
}

#[test]
fn attachment_setup_precedes_takeover_and_failure_never_activates_or_replays() {
    use crate::transport::client_lifetime::{ConnectionPurpose, Connector};
    use std::{
        io::Read,
        os::unix::ffi::OsStringExt,
        os::unix::net::{UnixListener, UnixStream},
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };
    struct RemoteFixture(PathBuf);
    impl Connector for RemoteFixture {
        fn remote(&self) -> bool {
            true
        }
        fn connect(&self, _: ConnectionPurpose) -> Result<UnixStream> {
            Ok(UnixStream::connect(&self.0)?)
        }
    }
    for scenario in ["terminal-failure", "missing", "invalid", "changed", "unsupported", "ready"] {
        let succeeds = scenario == "ready";
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("attachment.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let session = SessionInfo {
            name: "literal ../remote session".into(),
            session_id: "remote-session".into(),
            daemon_id: "remote-daemon".into(),
        };
        let expected = session.clone();
        let remote_cwd = PathBuf::from(std::ffi::OsString::from_vec(
            b"/remote-only/cwd-\xff".to_vec(),
        ));
        let cwd = remote_cwd.clone();
        let (setup_entered, setup_wait) = mpsc::channel();
        let (setup_allowed, setup_ready) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut lifetime, _) = listener.accept().unwrap();
            lifetime.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            assert!(matches!(
                transport::test_wire::read_request(&mut lifetime).unwrap().operation,
                Operation::ClientRegister { session_id, daemon_id }
                    if session_id == expected.session_id && daemon_id == expected.daemon_id
            ));
            transport::test_wire::write_response(&mut lifetime, &Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(serde_json::json!({
                    "session": expected,
                    "client": {"client_id": "only-client", "token": "test-only-token"},
                }).into()),
                error: None,
                failure: None,
            }).unwrap();
            setup_wait.recv_timeout(Duration::from_secs(3)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ), "takeover must not run during terminal setup");
            listener.set_nonblocking(false).unwrap();
            setup_allowed.send(()).unwrap();
            if scenario != "terminal-failure" {
                let (mut query, _) = listener.accept().unwrap();
                assert!(matches!(
                    transport::test_wire::read_request(&mut query).unwrap().operation,
                    Operation::Client { request, .. }
                        if matches!(*request, Operation::SessionInfo)
                ), "spawn policy discovery must precede activation");
                let mut defaults = SpawnDefaults {
                    argv: vec!["/remote/authoritative-shell".into(), "-i".into()],
                    cwd,
                    home: "/remote-only/default-home".into(),
                    isolated_environment: false,
                };
                if scenario == "invalid" {
                    defaults.cwd = "relative/not/remote-authority".into();
                }
                let mut identity = expected.clone();
                if scenario == "changed" {
                    identity.daemon_id = "replacement-daemon".into();
                }
                let mut reply = serde_json::json!({"session": identity});
                if scenario != "missing" {
                    reply["spawn_defaults"] = serde_json::to_value(defaults).unwrap();
                }
                transport::test_wire::write_response(&mut query, &Response {
                    version: PROTOCOL_VERSION,
                    ok: scenario != "unsupported",
                    result: (scenario != "unsupported").then_some(reply.into()),
                    error: (scenario == "unsupported").then(|| "inactive client operation unsupported".into()),
                    failure: None,
                }).unwrap();
            }
            if succeeds {
                let (mut control, _) = listener.accept().unwrap();
                assert!(matches!(
                    transport::test_wire::read_request(&mut control).unwrap().operation,
                    Operation::Client { authority, request }
                        if authority.client_id == "only-client"
                            && matches!(*request, Operation::ClientActivate {
                                detach_others: true, agent_socket: None
                            })
                ));
                transport::test_wire::write_response(&mut control, &Response {
                    version: PROTOCOL_VERSION,
                    ok: true,
                    result: Some(serde_json::json!({
                        "activated": true, "already_active": false
                    }).into()),
                    error: None,
                    failure: None,
                }).unwrap();
            }
            assert_eq!(lifetime.read(&mut [0]).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ), "failed setup, repeated activation and retired endpoints cannot dispatch");
        });
        let registered = RegisteredClient::register_connector(
            Arc::new(RemoteFixture(socket)), session.clone(), None,
        ).unwrap();
        let mut attachment = Attachment::Local(registered);
        let endpoint = attachment.endpoint();
        let ready = attachment.initialize(true, || {
            setup_entered.send(()).unwrap();
            setup_ready.recv_timeout(Duration::from_secs(3)).unwrap();
            if scenario == "terminal-failure" {
                bail!("terminal setup failed");
            }
            let defaults = remote_spawn_defaults(&endpoint, &session)?;
            assert_eq!(defaults.cwd, remote_cwd);
            assert_eq!(defaults.home, PathBuf::from("/remote-only/default-home"));
            assert_eq!(defaults.argv, vec!["/remote/authoritative-shell", "-i"]);
            Ok("initialized terminal resources and authoritative remote policy")
        });
        assert_eq!(ready.is_ok(), succeeds);
        if succeeds {
            assert!(attachment.activate_once(true).is_err());
        }
        drop(attachment);
        assert!(endpoint.is_revoked());
        assert!(endpoint.checked(Operation::Inspect).is_err());
        server.join().unwrap();
    }
}

#[test]
fn remote_window_creation_keeps_remote_paths_and_backend_shell_on_the_selected_endpoint() {
    use crate::transport::client_lifetime::{ConnectionPurpose, Connector};
    use std::{os::unix::net::UnixStream, sync::{Arc, Mutex}, thread};
    struct NativeStreams(Mutex<VecDeque<UnixStream>>);
    impl Connector for NativeStreams {
        fn remote(&self) -> bool {
            true
        }
        fn connect(&self, _: ConnectionPurpose) -> Result<UnixStream> {
            self.0.lock().unwrap().pop_front().context("unexpected extra request")
        }
    }
    let (lifetime, mut registration) = UnixStream::pair().unwrap();
    let (control, mut backend) = UnixStream::pair().unwrap();
    let identity = SessionInfo {
        name: "remote".into(),
        session_id: "remote-session".into(),
        daemon_id: "remote-daemon".into(),
    };
    let expected = identity.clone();
    let registered = thread::spawn(move || {
        assert!(matches!(
            transport::test_wire::read_request(&mut registration).unwrap().operation,
            Operation::ClientRegister { .. }
        ));
        transport::test_wire::write_response(&mut registration, &Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(serde_json::json!({
                "session": expected,
                "client": {"client_id": "selected-remote-client", "token": "test-only-token"},
            }).into()),
            error: None,
            failure: None,
        }).unwrap();
        registration
    });
    let client = RegisteredClient::register_connector(
        Arc::new(NativeStreams(Mutex::new(VecDeque::from([lifetime, control])))),
        identity,
        None,
    ).unwrap();
    let _lifetime = registered.join().unwrap();
    let endpoint = client.endpoint().clone();
    let server = thread::spawn(move || {
        let request = transport::test_wire::read_request(&mut backend).unwrap();
        transport::test_wire::write_response(&mut backend, &Response {
            version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some("remote fixture rejects creation without mutating registry".into()),
            failure: None,
        }).unwrap();
        request.operation
    });
    let (mut app, _input) = fixture();
    unavailable_fixture(&mut app);
    app.socket = &endpoint;
    app.options.shell = vec!["/remote/backend-shell".into(), "--interactive".into()];
    app.options.home = "/remote/default-home".into();
    app.options.isolated_environment = false;
    app.registry.panes[0].foreground_cwd = Some("/remote-only/foreground/cwd".into());
    app.registry.panes[0].home = "/remote-only/pane/home".into();
    app.registry.panes[0].isolated_environment = true;
    app.open_form(FormKind::CreateWindow, String::new());
    assert_eq!(
        app.state.form.as_ref().unwrap().fields.directory(),
        PathBuf::from("/remote-only/foreground/cwd")
    );
    app.apply_form(size()).unwrap();
    let Operation::Client { authority, request } = server.join().unwrap() else {
        panic!("creation lost selected remote client authority");
    };
    assert_eq!(authority.client_id, "selected-remote-client");
    let Operation::WindowCreate { spec, .. } = *request else {
        panic!("window creation was not routed to the selected backend");
    };
    assert_eq!(spec.argv, app.options.shell);
    assert_eq!(spec.cwd, PathBuf::from("/remote-only/foreground/cwd"));
    assert_eq!(spec.home, PathBuf::from("/remote-only/pane/home"));
    assert!(spec.isolated_environment);
    let jobs = std::mem::replace(&mut app.jobs, jobs::Jobs::new(1).unwrap());
    let completion = jobs.finish().unwrap();
    assert_eq!(completion.len(), 1);
    assert!(completion[0].result.is_err());
}
