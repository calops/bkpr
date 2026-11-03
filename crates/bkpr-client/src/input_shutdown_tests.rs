#[test]
fn inspection_shutdown_rejections_preserve_receipts_without_poisoning_lane_finish() {
    use bkpr_protocol::{RequestFailure, RequestOutcome, SelectionAction, SelectionEvent};
    for search in [false, true] {
        for outcome in [Some(RequestOutcome::NotDispatched), None, Some(RequestOutcome::Unknown)] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes = Vec::new();
                BufReader::new(stream.try_clone().unwrap()).read_until(b'\n', &mut bytes).unwrap();
                let request: Request = serde_json::from_slice(&bytes).unwrap();
                assert!(matches!(request.operation, Operation::Pane { request: PaneRequest::Select { .. } | PaneRequest::Search { .. }, .. }));
                writeln!(stream, "{}", serde_json::to_string(&Response {
                    version: PROTOCOL_VERSION,
                    ok: false,
                    result: None,
                    error: Some("inspection rejected".into()),
                    failure: outcome.map(|outcome| RequestFailure::SessionClosing { outcome }),
                }).unwrap()).unwrap();
            });
            let input = InputPump::new(&Endpoint::unmanaged(&path)).unwrap();
            let request = if search {
                PaneRequest::Search { view_id: "view".into(), action: SearchAction::Query { text: "term".into() } }
            } else {
                PaneRequest::Select { view_id: "view".into(), event: SelectionEvent { action: SelectionAction::Press, x: 1.0, y: 1.0, modifiers: 0, time_ms: 0 } }
            };
            let receipt = input.response(Operation::Pane { pane_id: "pane".into(), request }).unwrap();
            let error = receipt.recv_timeout(Duration::from_secs(3)).unwrap().unwrap_err();
            let undispatched = outcome == Some(RequestOutcome::NotDispatched);
            assert_eq!(transport::shutdown_not_dispatched(&error), undispatched);
            assert_eq!(transport::outcome_unknown(&error), outcome == Some(RequestOutcome::Unknown));
            assert_eq!(input.finish().is_ok(), undispatched);
            server.join().unwrap();
        }
    }
}
