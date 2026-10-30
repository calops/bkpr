use super::*;

#[test]
fn blocked_clipboard_is_pane_scoped_and_wrong_client_cannot_answer() {
    let fixture = Fixture::new();
    let registry = &fixture.registry;
    let home = fixture.home("clipboard");
    let mut first = spec(&home);
    first.argv = vec!["/bin/sh".into(), "-c".into(),
        r"while [ ! -f query ]; do sleep 0.02; done; printf '\033]52;c;?\007'; printf AFTER; exec sleep 60".into()];
    let one = send(
        registry,
        Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: first,
        },
    )["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let owner = view(registry, &one, 80);
    let two = spawn(registry, &fixture.home("unrelated"), None, None);
    let other = view(registry, &two, 90);
    fs::write(home.join("query"), b"query").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let pending = loop {
        let result = snapshot(registry, &one);
        if result["clipboard_pending"] == true {
            let pane = registry
                .inner
                .state
                .lock()
                .unwrap()
                .live
                .get(&one)
                .unwrap()
                .clone();
            let pending = pane
                .shared
                .clipboard
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .info
                .clone();
            break pending;
        }
        assert!(
            Instant::now() < deadline,
            "clipboard request never became pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let started = Instant::now();
    send(
        registry,
        Operation::Pane {
            pane_id: two.clone(),
            request: PaneRequest::Input {
                view_id: other.clone(),
                data: b"x\n".to_vec(),
            },
        },
    );
    create(registry, "while clipboard blocked");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "unrelated pane or registry blocked on clipboard"
    );
    assert!(
        registry
            .request(Request::new(Operation::Pane {
                pane_id: one.clone(),
                request: PaneRequest::Clipboard {
                    view_id: other,
                    request_id: pending.request_id.clone(),
                    response: ClipboardResponse::denied(),
                }
            }))
            .is_err()
    );
    send(
        registry,
        Operation::Pane {
            pane_id: one.clone(),
            request: PaneRequest::Clipboard {
                view_id: owner,
                request_id: pending.request_id,
                response: ClipboardResponse::denied(),
            },
        },
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let result = snapshot(registry, &one);
        if result["clipboard_pending"] == false {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
