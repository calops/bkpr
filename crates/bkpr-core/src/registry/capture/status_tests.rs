use super::*;

fn record(state: &str, app: Option<&str>, id: &str, message: &str) -> ProgramStatus {
    ProgramStatus {
        state: state.into(),
        kind: None,
        app: app.map(str::to_owned),
        message: Some(message.into()),
        id: id.into(),
        title: String::new(),
        progress: None,
    }
}
#[test]
fn status_summary_is_identity_scoped_prioritized_and_last_update_wins() {
    let harness = HarnessInfo {
        kind: HarnessKind::Claude,
        pid: 9,
        birth: "birth".into(),
        cwd: "/tmp".into(),
        managed: None,
    };
    let mut published = crate::Published {
        lifecycle: "running".into(),
        program_identity: Some(ProcessIdentity {
            pid: 9,
            birth: "birth".into(),
        }),
        ..Default::default()
    };
    published.status = vec![
        record("working", Some("claude"), "task", "working"),
        record("error", Some("codex"), "", "foreign app"),
        record("error", None, "nested", "unattributed child"),
        record("invented", Some("claude"), "", "unknown state"),
        record("blocked", Some("claude"), "a", "first blocked"),
        record("blocked", Some("claude"), "b", "last blocked"),
    ];
    assert_eq!(
        harness_summary(Some(&harness), &published)
            .unwrap()
            .message
            .as_deref(),
        Some("last blocked")
    );
    published
        .status
        .push(record("error", None, "", "root error"));
    assert_eq!(
        harness_summary(Some(&harness), &published)
            .unwrap()
            .message
            .as_deref(),
        Some("root error")
    );
    published.status = vec![
        record("done", Some("claude"), "done", "done"),
        record("idle", Some("claude"), "idle", "last idle"),
    ];
    assert_eq!(
        harness_summary(Some(&harness), &published).unwrap().state,
        "idle"
    );
    published.program_identity.as_mut().unwrap().birth = "other birth".into();
    assert!(harness_summary(Some(&harness), &published).is_none());
    published.program_identity = Some(ProcessIdentity {
        pid: 10,
        birth: "birth".into(),
    });
    assert!(harness_summary(Some(&harness), &published).is_none());
    published.program_identity = Some(ProcessIdentity {
        pid: 9,
        birth: "birth".into(),
    });
    published.lifecycle = "fault".into();
    assert!(harness_summary(Some(&harness), &published).is_none());
    assert!(harness_summary(None, &published).is_none());
}

#[test]
fn registry_watcher_receives_hidden_harness_status_without_admitting_any_views() {
    let fixture = Fixture::new();
    let workspace = fixture
        .registry
        .request(Request::new(Operation::WorkspaceCreate {
            name: "not attached".into(),
        }))
        .unwrap()["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let script = r#"while [ ! -f working ]; do sleep 0.01; done; printf '\033]7501;state=working:app=claude\007'; while [ ! -f blocked ]; do sleep 0.01; done; printf '\033]7501;state=blocked:app=claude:id=task\007'; while [ ! -f error ]; do sleep 0.01; done; printf '\033]7501;state=error:app=claude:id=task\007'; while [ ! -f done ]; do sleep 0.01; done; printf '\033]7501;state=clear\007\033]7501;state=done:app=claude:id=result\007'; while [ ! -f clear ]; do sleep 0.01; done; printf '\033]7501;state=clear\007'; exec sleep 60"#;
    let result = fixture
        .registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: Some(workspace),
            expected_revision: Some(1),
            spec: PaneSpawnSpec {
                argv: vec![
                    fixture.home.join("native/claude").to_str().unwrap().into(),
                    "-c".into(),
                    script.into(),
                ],
                cwd: fixture.home.clone(),
                home: fixture.home.clone(),
                title: None,
                isolated_environment: true,
                size: Size {
                    cols: 80,
                    rows: 24,
                    cell_width: 8,
                    cell_height: 16,
                },
            },
        }))
        .unwrap();
    let pane_id = result["pane_id"].as_str().unwrap();
    let pane = fixture.registry.capture_pane(pane_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while fixture.registry.snapshot().unwrap().panes[0]
        .harness
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "hidden harness was not recognized"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let durable = fs::read(fixture._root.path().join("registry.json")).unwrap();
    let mut revision = fixture.registry.snapshot().unwrap().revision;
    for (gate, expected) in [
        ("working", Some("working")),
        ("blocked", Some("blocked")),
        ("error", Some("error")),
        ("done", Some("done")),
        ("clear", None),
    ] {
        fs::write(fixture.home.join(gate), b"go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let response = fixture
                .registry
                .request(Request::new(Operation::Wait {
                    after: revision,
                    wait_ms: 1000,
                }))
                .unwrap();
            let snapshot: RegistryResult = serde_json::from_value(response).unwrap();
            revision = snapshot.registry.revision;
            let info = snapshot
                .registry
                .panes
                .iter()
                .find(|info| info.id == pane_id)
                .unwrap();
            if info
                .harness_status
                .as_ref()
                .map(|status| status.state.as_str())
                == expected
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "hidden status {gate} was not published to registry watchers"
            );
        }
        assert!(
            pane.shared.authority.lock().unwrap().views.is_empty(),
            "monitoring must not create extra view leases"
        );
    }
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        durable,
        "status changes must never write durable metadata"
    );
}
