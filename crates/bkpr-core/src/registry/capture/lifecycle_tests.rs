use super::*;

fn seed_launch(fixture: &Fixture) -> (String, CaptureGrant, Peer) {
    fixture
        .registry
        .inner
        .observer_started
        .store(true, Ordering::Release);
    let result = fixture
        .registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            expected_revision: None,
            spec: PaneSpawnSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "while [ ! -f exit-gate ]; do sleep 0.02; done; exit 7".into(),
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
    let id = result["pane_id"].as_str().unwrap().to_owned();
    let pane = fixture.registry.capture_pane(&id).unwrap();
    let root = pane.shared.root.lock().unwrap().clone().unwrap();
    let grant = fixture
        .registry
        .inner
        .captures
        .lock()
        .unwrap()
        .insert(root.clone(), root.clone(), id.clone(), HarnessKind::Claude)
        .unwrap();
    let managed = fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id]
        .managed
        .clone();
    fixture
        .registry
        .publish_facts(
            &id,
            &pane,
            Some(fixture.home.clone()),
            Some(HarnessInfo {
                kind: HarnessKind::Claude,
                pid: root.pid as u32,
                birth: root.identity.clone(),
                cwd: fixture.home.clone(),
                managed: Some(managed),
            }),
            None,
        )
        .unwrap();
    (
        id,
        grant,
        Peer {
            pid: root.pid,
            uid: unsafe { libc::geteuid() },
            birth: root.identity,
        },
    )
}
#[test]
fn execution_exit_revokes_before_late_callbacks_and_preserves_shell_exit() {
    let fixture = Fixture::new();
    let (pane_id, grant, peer) = seed_launch(&fixture);
    fs::write(fixture.home.join("exit-gate"), b"exit").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = fixture.registry.snapshot().unwrap();
        let info = snapshot.panes.iter().find(|p| p.id == pane_id).unwrap();
        if info.status == PaneStatus::Exited {
            assert_eq!(info.exit_code, Some(7));
            assert!(info.harness.is_none() && info.foreground_cwd.is_none());
            break;
        }
        assert!(Instant::now() < deadline, "pane completion not published");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .is_empty()
    );
    assert!(
        fixture
            .registry
            .request_authenticated(
                Request::new(Operation::HarnessEvent {
                    launch_id: grant.launch_id,
                    token: grant.token,
                    event: CaptureEvent::Bind {
                        conversation_id: "too-late".into(),
                        source: ConversationSource::ClaudeHook
                    }
                }),
                &peer
            )
            .is_err()
    );
    let snapshot = fixture.snapshot(&pane_id);
    assert!(snapshot.program_identity.is_none() && snapshot.program_status.is_empty());
}
#[test]
fn fault_revokes_even_when_completion_persistence_fails_and_shutdown_reaps() {
    let fixture = Fixture::new();
    let (pane_id, grant, peer) = seed_launch(&fixture);
    let pane = fixture.registry.capture_pane(&pane_id).unwrap();
    {
        let mut p = pane.shared.published.lock().unwrap();
        p.lifecycle = "fault".into();
        p.error = Some("owned fault fixture".into());
    }
    fs::remove_file(fixture._root.path().join("registry.json")).unwrap();
    fs::create_dir(fixture._root.path().join("registry.json")).unwrap();
    assert!(fixture.registry.record_completion(&pane_id).is_err());
    assert!(
        fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .is_empty()
    );
    assert!(
        fixture
            .registry
            .request_authenticated(
                Request::new(Operation::HarnessContext {
                    launch_id: grant.launch_id,
                    token: grant.token
                }),
                &peer
            )
            .is_err()
    );
    assert!(
        pane.shared
            .published
            .lock()
            .unwrap()
            .program_identity
            .is_none()
    );
    // A poisoned durable registry still owns and reaps its actual PTY child.
    let _ = fixture.registry.shutdown();
    assert!(proc::process(peer.pid).unwrap().is_none());
}

#[test]
fn first_and_after_last_pane_observe_real_foreground_cwd_without_manual_refresh() {
    let fixture = Fixture::new();
    for directory in ["native-cwd", "native"] {
        let target = fixture.home.join(directory);
        let result = fixture
            .registry
            .request(Request::new(Operation::PaneSpawn {
                workspace_id: None,
                expected_revision: None,
                spec: PaneSpawnSpec {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "cd \"$1\"; exec sleep 60".into(),
                        "cwd-fixture".into(),
                        target.to_str().unwrap().into(),
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
        let id = result["pane_id"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let snapshot = fixture.registry.snapshot().unwrap();
            let info = snapshot.panes.iter().find(|p| p.id == id).unwrap();
            if info.foreground_cwd.as_ref() == Some(&target) {
                assert!(info.harness.is_none());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "first pane never received automatic CWD recognition"
            );
            thread::sleep(Duration::from_millis(5));
        }
        fixture
            .registry
            .request(Request::new(Operation::PaneTerminate {
                pane_id: id.into(),
            }))
            .unwrap();
        // Wait for the previous observation owner to retire before exercising restart.
        let deadline = Instant::now() + Duration::from_secs(8);
        while fixture
            .registry
            .inner
            .observer_started
            .load(Ordering::Acquire)
        {
            assert!(
                Instant::now() < deadline,
                "idle process observer failed to retire"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
