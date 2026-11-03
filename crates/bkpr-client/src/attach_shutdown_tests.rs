#[test]
fn initial_snapshot_shutdown_and_rollback_failures_keep_separate_provenance() {
    use bkpr_protocol::{Request, RequestFailure, RequestOutcome, Response};
    use std::{io::{BufRead, BufReader, Write}, os::unix::net::UnixListener, thread, time::Duration};
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
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes = Vec::new();
                BufReader::new(stream.try_clone().unwrap()).read_until(b'\n', &mut bytes).unwrap();
                let request: Request = serde_json::from_slice(&bytes).unwrap();
                assert!(matches!((step, request.operation),
                    (0, Operation::Pane { request: PaneRequest::Attach { .. }, .. })
                    | (1, Operation::Pane { request: PaneRequest::Poll { .. }, .. })
                    | (2, Operation::Pane { request: PaneRequest::Detach { .. }, .. })));
                let reply = match step {
                    0 => Response { version: PROTOCOL_VERSION, ok: true, result: Some(serde_json::json!({"view_id":"admitted-view"})), error: None, failure: None },
                    1 if malformed_snapshot => Response { version: PROTOCOL_VERSION, ok: true, result: Some(serde_json::json!({"invalid_snapshot":true})), error: None, failure: None },
                    _ => Response {
                        version: PROTOCOL_VERSION,
                        ok: false,
                        result: None,
                        error: Some("operation could not complete".into()),
                        failure: if step == 1 { Some(RequestFailure::SessionClosing { outcome: RequestOutcome::Unknown }) } else { cleanup_outcome.map(|outcome| RequestFailure::SessionClosing { outcome }) },
                    },
                };
                writeln!(stream, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
            }
        });
        let (mut app, _received) = fixture();
        attach_fixture(&mut app);
        let spec = app.views["focused-pane"].spec.clone();
        let endpoint = Endpoint::unmanaged(&path);
        let error = attach(&endpoint, "pane", spec).err().unwrap();
        assert_eq!(transport::expected_shutdown(&error, true), clean);
        assert_eq!(transport::outcome_unknown(&error), !malformed_snapshot);
        assert!(matches!(error.downcast_ref::<AttachOwnership>(), Some(AttachOwnership::Unconfirmed)));
        server.join().unwrap();
    }
}
