use super::*;

#[path = "control_tests.rs"]
mod control_tests;
fn seed_launch(fixture: &Fixture) -> (String, CaptureGrant, Peer) {
    seed_launch_command(
        fixture,
        HarnessKind::Claude,
        "while [ ! -f exit-gate ]; do sleep 0.02; done; exit 7",
    )
}
fn seed_launch_command(
    fixture: &Fixture,
    kind: HarnessKind,
    script: &str,
) -> (String, CaptureGrant, Peer) {
    fixture
        .registry
        .inner
        .observer_started
        .store(true, Ordering::Release);
    let result = fixture
        .registry
        .request(Request::new(Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            panes: vec![PaneSpawnEntry {
                reference: "pane".into(),
                spec: PaneSpawnSpec {
                    argv: vec![
                        if kind == HarnessKind::Claude {
                            fixture.home.join("native/claude").to_str().unwrap().into()
                        } else {
                            "/bin/sh".into()
                        },
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
            }],
            layout: None,
        }))
        .unwrap()
        .into_json()
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
        .insert(root.clone(), root.clone(), id.clone(), kind)
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
                kind,
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
fn native_prompt_fact_is_launch_scoped_and_never_invents_semantic_status() {
    let fixture = Fixture::new();
    let (pane_id, grant, peer) =
        seed_launch_command(&fixture, HarnessKind::Claude, "IFS= read -r hold");
    let event = |event| {
        Request::new(Operation::HarnessEvent {
            launch_id: grant.launch_id.clone(),
            token: grant.token.clone(),
            event,
        })
    };
    let published = || {
        fixture
            .registry
            .snapshot()
            .unwrap()
            .panes
            .into_iter()
            .find(|pane| pane.id == pane_id)
            .unwrap()
    };
    assert!(
        published()
            .harness
            .unwrap()
            .managed
            .unwrap()
            .first_prompt_submitted_at_ms
            .is_none()
    );
    fixture
        .registry
        .request_authenticated(
            event(CaptureEvent::Bind {
                conversation_id: "native-one".into(),
                source: ConversationSource::ClaudeHook,
            }),
            &peer,
        )
        .unwrap();
    assert!(
        fixture
            .registry
            .request_authenticated(
                event(CaptureEvent::PromptSubmitted {
                    conversation_id: "wrong-native".into(),
                    source: ConversationSource::ClaudeHook,
                }),
                &peer
            )
            .is_err()
    );
    for _ in 0..2 {
        fixture
            .registry
            .request_authenticated(
                event(CaptureEvent::PromptSubmitted {
                    conversation_id: "native-one".into(),
                    source: ConversationSource::ClaudeHook,
                }),
                &peer,
            )
            .unwrap();
    }
    let first = published();
    assert!(first.harness_status.is_none() && first.harness_status_changed_at_ms.is_none());
    let first_time = first
        .harness
        .unwrap()
        .managed
        .unwrap()
        .first_prompt_submitted_at_ms
        .unwrap();
    fixture
        .registry
        .request_authenticated(
            event(CaptureEvent::Bind {
                conversation_id: "native-two".into(),
                source: ConversationSource::ClaudeHook,
            }),
            &peer,
        )
        .unwrap();
    assert!(
        fixture
            .registry
            .request_authenticated(
                event(CaptureEvent::PromptSubmitted {
                    conversation_id: "native-one".into(),
                    source: ConversationSource::ClaudeHook,
                }),
                &peer
            )
            .is_err()
    );
    fixture
        .registry
        .request_authenticated(
            event(CaptureEvent::PromptSubmitted {
                conversation_id: "native-two".into(),
                source: ConversationSource::ClaudeHook,
            }),
            &peer,
        )
        .unwrap();
    let after = published();
    assert!(after.harness_status.is_none() && after.harness_status_changed_at_ms.is_none());
    assert_eq!(
        after
            .harness
            .unwrap()
            .managed
            .unwrap()
            .first_prompt_submitted_at_ms,
        Some(first_time)
    );
    fixture
        .registry
        .request_authenticated(event(CaptureEvent::End), &peer)
        .unwrap();
    assert!(
        fixture
            .registry
            .request_authenticated(
                event(CaptureEvent::PromptSubmitted {
                    conversation_id: "native-two".into(),
                    source: ConversationSource::ClaudeHook,
                }),
                &peer
            )
            .is_err()
    );
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
                window_id: None,
                expected_revision: None,
                panes: vec![PaneSpawnEntry {
                    reference: "pane".into(),
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
                }],
                layout: None,
            }))
            .unwrap()
            .into_json()
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
            .unwrap()
            .into_json()
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

#[test]
fn authenticated_bind_storage_failure_revokes_capture_and_poison_does_not_stop_native_cleanup() {
    let fixture = Fixture::new();
    let (pane_id, grant, peer) = seed_launch(&fixture);
    let event = |id: &str| {
        Request::new(Operation::HarnessEvent {
            launch_id: grant.launch_id.clone(),
            token: grant.token.clone(),
            event: CaptureEvent::Bind {
                conversation_id: id.into(),
                source: ConversationSource::ClaudeHook,
            },
        })
    };
    fixture
        .registry
        .request_authenticated(event("verified-first"), &peer)
        .unwrap()
        .into_json()
        .unwrap();
    // Native turn-boundary callbacks repeat the same facts. With unrelated
    // project publication stopped, neither durability nor registry revision changes.
    fixture.registry.stop_projects();
    let before_revision = fixture.registry.snapshot().unwrap().revision;
    let before_bytes = fs::read(fixture._root.path().join("registry.json")).unwrap();
    fixture
        .registry
        .request_authenticated(event("verified-first"), &peer)
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        fixture.registry.snapshot().unwrap().revision,
        before_revision
    );
    assert_eq!(
        fs::read(fixture._root.path().join("registry.json")).unwrap(),
        before_bytes
    );
    let pane = fixture.registry.capture_pane(&pane_id).unwrap();
    fs::remove_file(fixture._root.path().join("registry.json")).unwrap();
    fs::create_dir(fixture._root.path().join("registry.json")).unwrap();
    assert!(
        fixture
            .registry
            .request_authenticated(event("uncommitted-second"), &peer)
            .is_err()
    );
    assert!(
        fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .lookup(&grant.launch_id, &grant.token)
            .is_err()
    );
    assert!(
        fixture
            .registry
            .request(Request::new(Operation::Inspect))
            .is_err()
    );
    assert_eq!(
        fixture
            .registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .recoveries[&pane_id]
            .conversation_id,
        "verified-first"
    );
    assert!(pane.request(PaneRequest::Inspect).is_ok());
    let _ = fixture.registry.shutdown();
    assert!(proc::process(peer.pid).unwrap().is_none());
}

#[test]
fn authenticated_end_revokes_even_if_agent_retirement_storage_fails_and_keeps_verified_recovery() {
    let fixture = Fixture::new();
    let (pane_id, grant, peer) = seed_launch(&fixture);
    let launch = fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
    fixture
        .registry
        .agent_register(&launch, &fixture.home)
        .unwrap();
    fixture
        .registry
        .request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                event: CaptureEvent::Bind {
                    conversation_id: "verified-native".into(),
                    source: ConversationSource::ClaudeHook,
                },
            }),
            &peer,
        )
        .unwrap()
        .into_json()
        .unwrap();
    let path = fixture._root.path().join("agents/state.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(
        fixture
            .registry
            .request_authenticated(
                Request::new(Operation::HarnessEvent {
                    launch_id: grant.launch_id.clone(),
                    token: grant.token.clone(),
                    event: CaptureEvent::End,
                }),
                &peer
            )
            .is_err()
    );
    assert!(
        fixture
            .registry
            .inner
            .captures
            .lock()
            .unwrap()
            .lookup(&grant.launch_id, &grant.token)
            .is_err()
    );
    assert_eq!(
        fixture.registry.recovery_candidates().unwrap()[0]
            .harness
            .conversation_id,
        "verified-native"
    );
    assert!(
        fixture
            .registry
            .snapshot()
            .unwrap()
            .panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .unwrap()
            .harness
            .is_none()
    );
}

#[test]
fn agent_recovery_control_defaults_to_live_workspace_and_cannot_resume_foreign_or_run_arbitrary_operations()
 {
    let fixture = Fixture::new();
    fixture
        .registry
        .configure_harnesses(&fixture.socket, &std::env::current_exe().unwrap())
        .unwrap();
    let (pane_id, grant, peer) =
        seed_launch_command(&fixture, HarnessKind::Claude, "IFS= read -r hold");
    let launch = fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
    fixture
        .registry
        .agent_register(&launch, &fixture.home)
        .unwrap();
    let control = |operation, scope| {
        Request::new(Operation::Agent {
            launch_id: grant.launch_id.clone(),
            token: grant.token.clone(),
            request: AgentRequest::Control {
                request: Box::new(operation),
                scope,
                reply: bkpr_protocol::control::ControlReply::Full,
            },
        })
    };
    assert!(
        fixture
            .registry
            .request_authenticated(
                control(Operation::RecoveryList, AgentScope::Workspace),
                &peer
            )
            .is_err()
    );
    let own_workspace = Uuid::new_v4().to_string();
    let mut saved = Vec::new();
    {
        let _transaction = fixture.registry.inner.transaction.lock().unwrap();
        let mut next = fixture.registry.inner.state.lock().unwrap().record.clone();
        for workspace_id in [&own_workspace, &Uuid::new_v4().to_string()] {
            let id = Uuid::new_v4().to_string();
            let window_id = Uuid::new_v4().to_string();
            let mut pane = next
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == pane_id)
                .unwrap()
                .clone();
            pane.id = id.clone();
            pane.status = PaneStatus::Stopped;
            pane.harness = None;
            pane.foreground_cwd = None;
            pane.workspace_id = Some(workspace_id.clone());
            pane.window_id = Some(window_id.clone());
            let mut layout = LayoutNode::Leaf {
                pane_id: id.clone(),
            };
            if workspace_id == &own_workspace {
                let own = next
                    .registry
                    .panes
                    .iter_mut()
                    .find(|pane| pane.id == pane_id)
                    .unwrap();
                own.workspace_id = Some(workspace_id.clone());
                own.window_id = Some(window_id.clone());
                layout = LayoutNode::Split {
                    axis: Axis::Horizontal,
                    ratio: 500,
                    first: Box::new(LayoutNode::Leaf {
                        pane_id: pane_id.clone(),
                    }),
                    second: Box::new(layout),
                };
            }
            next.registry.panes.push(pane);
            next.registry.workspaces.push(Workspace {
                id: workspace_id.clone(),
                name: workspace_id.clone(),
                revision: 1,
                windows: vec![Window {
                    id: window_id,
                    name: "saved".into(),
                    revision: 1,
                    layout: Some(layout),
                }],
            });
            let descriptor = HarnessRecovery {
                kind: HarnessKind::Omp,
                conversation_id: id.clone(),
                cwd: fixture.home.clone(),
            };
            next.recoveries.insert(id.clone(), descriptor.clone());
            saved.push((id, descriptor));
        }
        next.registry.revision = next_revision(next.registry.revision).unwrap();
        validate_record(&next).unwrap();
        commit(&fixture.registry.inner, next, |_| {}).unwrap();
    }
    let local = fixture
        .registry
        .request_authenticated(
            control(Operation::RecoveryList, AgentScope::Workspace),
            &peer,
        )
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(local["recoveries"].as_array().unwrap().len(), 1);
    assert_eq!(local["recoveries"][0]["pane_id"], saved[0].0);
    let wider = fixture
        .registry
        .request_authenticated(control(Operation::RecoveryList, AgentScope::Session), &peer)
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(wider["recoveries"].as_array().unwrap().len(), 2);
    let before = fixture.registry.snapshot().unwrap().panes.len();
    assert!(
        fixture
            .registry
            .request_authenticated(
                control(
                    Operation::PaneResume {
                        pane_id: saved[1].0.clone(),
                        expected: saved[1].1.clone()
                    },
                    AgentScope::Workspace
                ),
                &peer
            )
            .is_err()
    );
    assert_eq!(fixture.registry.snapshot().unwrap().panes.len(), before);
    assert!(
        fixture
            .registry
            .request_authenticated(control(Operation::Shutdown, AgentScope::Session), &peer)
            .is_err()
    );
    assert!(fixture.registry.capture_pane(&pane_id).is_ok());
    assert!(
        fixture
            .registry
            .request_authenticated(
                control(Operation::RecoveryList, AgentScope::Repository),
                &peer
            )
            .is_err()
    );
}

fn bind_native(registry: &Registry, grant: &CaptureGrant, peer: &Peer, id: &str) -> Result<Value> {
    registry
        .request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                event: CaptureEvent::Bind {
                    conversation_id: id.into(),
                    source: match grant.kind {
                        HarnessKind::Codex => ConversationSource::CodexResume,
                        _ => ConversationSource::ClaudeHook,
                    },
                },
            }),
            peer,
        )
        .and_then(Payload::into_json)
}
fn own_windows(registry: &Registry, panes: &[String]) -> (String, Vec<String>) {
    let workspace = Uuid::new_v4().to_string();
    let windows = panes
        .iter()
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    registry
        .mutate(|snapshot| {
            for (pane_id, window_id) in panes.iter().zip(&windows) {
                let pane = snapshot
                    .panes
                    .iter_mut()
                    .find(|pane| &pane.id == pane_id)
                    .unwrap();
                pane.workspace_id = Some(workspace.clone());
                pane.window_id = Some(window_id.clone());
            }
            snapshot.workspaces.push(Workspace {
                id: workspace.clone(),
                name: "owned".into(),
                revision: 1,
                windows: panes
                    .iter()
                    .zip(&windows)
                    .map(|(pane, window)| Window {
                        id: window.clone(),
                        name: "native".into(),
                        revision: 1,
                        layout: Some(LayoutNode::Leaf {
                            pane_id: pane.clone(),
                        }),
                    })
                    .collect(),
            });
            Ok(())
        })
        .unwrap();
    (workspace, windows)
}

#[test]
fn codex_supervisor_binding_uses_tui_cwd_not_same_depth_app_server() {
    let fixture = Fixture::new();
    fs::copy("/bin/sh", fixture.home.join("native/codex")).unwrap();
    fs::create_dir(fixture.home.join("server-cwd")).unwrap();
    fs::write(
        fixture.home.join("server-cwd/app-server"),
        b"printf ready > server-ready; while :; do sleep 1; done",
    )
    .unwrap();
    let (_, grant, peer) = seed_launch_command(
        &fixture,
        HarnessKind::Codex,
        "(cd \"$HOME/server-cwd\"; exec \"$HOME/native/codex\" app-server) & (cd \"$HOME/native-cwd\"; exec \"$HOME/native/codex\" -c 'printf ready > tui-ready; while :; do sleep 1; done') & wait",
    );
    await_file(&fixture.home.join("server-cwd/server-ready"));
    await_file(&fixture.home.join("native-cwd/tui-ready"));
    bind_native(&fixture.registry, &grant, &peer, "owned-codex-thread").unwrap();
    assert_eq!(
        fixture.registry.recovery_candidates().unwrap()[0]
            .harness
            .cwd,
        fixture.home.join("native-cwd")
    );
}

#[test]
fn view_detach_during_termination_durability_never_strands_live_capture_or_loses_recovery() {
    let fixture = Fixture::new();
    let (id, grant, peer) = seed_launch(&fixture);
    bind_native(
        &fixture.registry,
        &grant,
        &peer,
        "verified-before-terminate",
    )
    .unwrap();
    let attached = fixture
        .registry
        .request(Request::new(Operation::Pane {
            pane_id: id.clone(),
            request: PaneRequest::Attach {
                spec: ViewSpec {
                    size: Size {
                        cols: 80,
                        rows: 24,
                        cell_width: 8,
                        cell_height: 16,
                    },
                    visible: true,
                    interactive: true,
                    input: true,
                    clipboard: true,
                },
            },
        }))
        .unwrap()
        .into_json()
        .unwrap();
    let view = attached["view_id"].as_str().unwrap().to_owned();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *fixture.registry.inner.persist_barrier.lock().unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let terminating_registry = fixture.registry.clone();
    let terminating_id = id.clone();
    let terminating_view = view.clone();
    let terminating = thread::spawn(move || {
        terminating_registry.request(Request::new(Operation::Pane {
            pane_id: terminating_id,
            request: PaneRequest::Terminate {
                view_id: terminating_view,
            },
        }))
    });
    let blocked = entered_rx.recv_timeout(Duration::from_secs(8));
    let detached = fixture.registry.request(Request::new(Operation::Pane {
        pane_id: id.clone(),
        request: PaneRequest::Detach { view_id: view },
    }));
    let _ = resume.send(());
    let result = terminating.join().unwrap();
    blocked.unwrap();
    detached.unwrap();
    if result.is_err() {
        assert!(
            fixture
                .registry
                .recovery_candidates()
                .unwrap()
                .iter()
                .any(|candidate| candidate.pane_id == id)
        );
        bind_native(
            &fixture.registry,
            &grant,
            &peer,
            "verified-before-terminate",
        )
        .unwrap();
        assert!(proc::process(peer.pid).unwrap().is_some());
    } else {
        assert!(fixture.registry.recovery_candidates().unwrap().is_empty());
        assert!(proc::process(peer.pid).unwrap().is_none());
    }
}

#[test]
fn closing_one_window_keeps_other_window_native_binding_and_agent_context_live() {
    let fixture = Fixture::new();
    let (closing_id, _, _) = seed_launch(&fixture);
    let (surviving_id, grant, peer) = seed_launch(&fixture);
    let launch = fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
    fixture
        .registry
        .agent_register(&launch, &fixture.home)
        .unwrap();
    let (workspace, windows) = own_windows(
        &fixture.registry,
        &[closing_id.clone(), surviving_id.clone()],
    );
    let closing_pane = fixture.registry.capture_pane(&closing_id).unwrap();
    let hold = closing_pane.shared.requests.lock().unwrap();
    let closing_window = windows[0].clone();
    let closing_registry = fixture.registry.clone();
    let close = thread::spawn(move || {
        closing_registry.request(Request::new(Operation::WindowClose {
            workspace_id: workspace,
            window_id: windows[0].clone(),
            expected_revision: 1,
        }))
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let reserved = fixture
            .registry
            .inner
            .state
            .lock()
            .unwrap()
            .closing_windows
            .contains(&closing_window);
        if reserved {
            break;
        }
        if Instant::now() >= deadline {
            drop(hold);
            panic!("window close never reached native termination");
        }
        thread::yield_now();
    }
    let binding = bind_native(&fixture.registry, &grant, &peer, "surviving-window-thread");
    let context = fixture.registry.request_authenticated(
        Request::new(Operation::Agent {
            launch_id: grant.launch_id.clone(),
            token: grant.token.clone(),
            request: AgentRequest::Whoami { detail: false },
        }),
        &peer,
    );
    drop(hold);
    close.join().unwrap().unwrap();
    binding.unwrap();
    context.unwrap();
    assert!(fixture.registry.capture_pane(&surviving_id).is_ok());
}

#[test]
fn ended_agent_cannot_execute_recovery_after_scope_sampling_before_spawn_reservation() {
    let fixture = Fixture::new();
    let executable = fixture.home.join("owned-resume");
    fs::write(
        &executable,
        b"#!/bin/sh\nprintf executed > resumed-from-retired\nexit 23\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fixture
        .registry
        .configure_harnesses(&fixture.socket, &executable)
        .unwrap();
    let (id, grant, peer) =
        seed_launch_command(&fixture, HarnessKind::Claude, "IFS= read -r _; exit 7");
    let launch = fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
    fixture
        .registry
        .agent_register(&launch, &fixture.home)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let context = foreground_context(&launch);
        if matches!(context, Ok((true, _))) {
            break;
        }
        assert!(Instant::now() < deadline, "fixture foreground: {context:?}");
        thread::sleep(Duration::from_millis(5));
    }
    let source = Uuid::new_v4().to_string();
    let expected = HarnessRecovery {
        kind: HarnessKind::Omp,
        conversation_id: "owned-recovery-thread".into(),
        cwd: fixture.home.clone(),
    };
    {
        let _transaction = fixture.registry.inner.transaction.lock().unwrap();
        let mut next = fixture.registry.inner.state.lock().unwrap().record.clone();
        let mut receipt = next
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .unwrap()
            .clone();
        receipt.id = source.clone();
        receipt.status = PaneStatus::Stopped;
        receipt.harness = None;
        receipt.foreground_cwd = None;
        next.registry.panes.push(receipt);
        next.recoveries.insert(source.clone(), expected.clone());
        next.registry.revision = next_revision(next.registry.revision).unwrap();
        commit(&fixture.registry.inner, next, |_| {}).unwrap();
    }
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *fixture
        .registry
        .inner
        .control_resume_barrier
        .lock()
        .unwrap() = Some(Arc::new(SpawnBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let controlling_registry = fixture.registry.clone();
    let controlling_peer = Peer {
        pid: peer.pid,
        uid: peer.uid,
        birth: peer.birth.clone(),
    };
    let request = Request::new(Operation::Agent {
        launch_id: grant.launch_id.clone(),
        token: grant.token.clone(),
        request: AgentRequest::Control {
            request: Box::new(Operation::PaneResume {
                pane_id: source,
                expected,
            }),
            scope: AgentScope::Session,
            reply: bkpr_protocol::control::ControlReply::Receipt,
        },
    });
    let control = thread::spawn(move || {
        controlling_registry.request_authenticated(request, &controlling_peer)
    });
    let sampled = entered_rx.recv_timeout(Duration::from_secs(8));
    let ended = fixture.registry.request_authenticated(
        Request::new(Operation::HarnessEvent {
            launch_id: grant.launch_id,
            token: grant.token,
            event: CaptureEvent::End,
        }),
        &peer,
    );
    let _ = resume.send(());
    let result = control.join().unwrap();
    assert!(
        sampled.is_ok(),
        "control did not reach reservation barrier: {result:?}"
    );
    ended.unwrap();
    assert!(result.is_err());
    assert!(!fixture.home.join("resumed-from-retired").exists());
    assert_eq!(fixture.registry.snapshot().unwrap().panes.len(), 2);
}

#[test]
#[cfg(target_os = "linux")]
fn exited_auxiliary_process_has_no_native_binding_role() {
    let fixture = Fixture::new();
    let mut child = ChildGuard(
        Command::new("/bin/sh")
            .args(["-c", "kill -STOP $$"])
            .env_clear()
            .env("HOME", &fixture.home)
            .current_dir(&fixture.home)
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id() as i32;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !proc::process(pid)
        .unwrap()
        .is_some_and(|process| process.stopped)
    {
        assert!(Instant::now() < deadline, "owned auxiliary did not stop");
        thread::sleep(Duration::from_millis(5));
    }
    child.0.kill().unwrap();
    while proc::process(pid).unwrap().is_some() {
        assert!(Instant::now() < deadline, "owned auxiliary did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    // The child remains an unreaped zombie: its snapshot can outlive /proc/exe.
    assert_eq!(proc::binding_kind(pid).unwrap(), None);
}

#[test]
fn native_binding_never_substitutes_the_supervisor_working_directory() {
    let fixture = Fixture::new();
    let (pane, grant, peer) = seed_launch_command(&fixture, HarnessKind::Codex, "read -r ignored");
    assert!(
        fixture
            .registry
            .request_authenticated(
                Request::new(Operation::HarnessEvent {
                    launch_id: grant.launch_id,
                    token: grant.token,
                    event: CaptureEvent::Bind {
                        conversation_id: "missing-native-process".into(),
                        source: ConversationSource::CodexResume,
                    },
                }),
                &peer,
            )
            .is_err()
    );
    assert!(
        !fixture
            .registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .recoveries
            .contains_key(&pane)
    );
}
