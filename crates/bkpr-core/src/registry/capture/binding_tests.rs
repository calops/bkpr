use super::*;

fn call(
    registry: &Registry,
    grant: &CaptureGrant,
    peer: &Peer,
    guard: Option<AgentBindingGuard>,
    request: AgentRequest,
) -> Result<Value> {
    registry
        .request_authenticated(
            Request::new(Operation::Agent {
                launch_id: grant.launch_id.clone(),
                token: grant.token.clone(),
                guard,
                request,
            }),
            peer,
        )?
        .into_json()
}
fn event(f: &ControlFixture, event: CaptureEvent) -> Result<Value> {
    f.fixture
        .registry
        .request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: f.grant.launch_id.clone(),
                token: f.grant.token.clone(),
                event,
            }),
            &f.peer,
        )?
        .into_json()
}
fn binding(f: &ControlFixture) -> NativeConversation {
    f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id]
        .managed
        .conversation
        .clone()
        .unwrap()
}
fn snapshot(f: &ControlFixture) -> NativeBindingSnapshot {
    f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id]
        .binding_snapshot()
}
fn conditional_bind(f: &ControlFixture, id: &str, revision: u64) -> NativeBindingSnapshot {
    let response = event(
        f,
        CaptureEvent::Bind {
            conversation_id: id.into(),
            source: ConversationSource::ClaudeHook,
            expected_revision: Some(revision),
        },
    )
    .unwrap();
    assert_eq!(response["accepted"], true);
    let binding = serde_json::from_value(response["binding"].clone()).unwrap();
    assert_eq!(snapshot(f), binding);
    binding
}
fn assert_cas_rejected(f: &ControlFixture, stale: CaptureEvent) {
    let before = snapshot(f);
    let launch =
        f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
    let version = f.fixture.registry.inner.captures.lock().unwrap().version;
    let registry_revision = f.fixture.registry.snapshot().unwrap().revision;
    let pane = f.fixture.registry.capture_pane(&f.pane).unwrap();
    let pane_revision = pane.shared.published.lock().unwrap().revision;
    let path = f.fixture._root.path().join("agents/state.json");
    let stored = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    let response = event(f, stale).unwrap();
    assert_eq!(response, json!({"accepted":false,"binding":before}));
    assert_eq!(snapshot(f), before);
    let captures = f.fixture.registry.inner.captures.lock().unwrap();
    assert_eq!(captures.version, version);
    let current = &captures.launches[&f.grant.launch_id];
    assert!(current.control.same_authority(&launch.control));
    assert!(launch.control.is_valid());
    drop(captures);
    assert_eq!(
        f.fixture.registry.snapshot().unwrap().revision,
        registry_revision
    );
    assert_eq!(
        pane.shared.published.lock().unwrap().revision,
        pane_revision
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::read(path).unwrap(), stored);
}
fn guard(f: &ControlFixture) -> AgentBindingGuard {
    let conversation = binding(f);
    AgentBindingGuard {
        context_id: conversation.id.clone(),
        conversation,
    }
}
fn bind(f: &ControlFixture, id: &str) {
    bind_native(&f.fixture.registry, &f.grant, &f.peer, id).unwrap();
}
fn own_call(f: &ControlFixture, request: AgentRequest) -> Result<Value> {
    call(
        &f.fixture.registry,
        &f.grant,
        &f.peer,
        Some(guard(f)),
        request,
    )
}
fn other(f: &ControlFixture) -> (CaptureGrant, Peer) {
    let (_, grant, peer) = seed_launch_command(
        &f.fixture,
        HarnessKind::Claude,
        "printf ready > binding-peer-ready; while IFS= read -r line; do :; done",
    );
    await_file(&f.fixture.home.join("binding-peer-ready"));
    let launch =
        f.fixture.registry.inner.captures.lock().unwrap().launches[&grant.launch_id].clone();
    f.fixture
        .registry
        .agent_register(&launch, &f.fixture.home)
        .unwrap();
    (grant, peer)
}
fn send(to: &CaptureGrant, key: &str, delivery: AgentDelivery) -> AgentRequest {
    AgentRequest::SendMessage {
        to: to.launch_id.clone(),
        text: format!("body-{key}"),
        idempotency_key: key.into(),
        scope: AgentScope::All,
        delivery,
    }
}
fn inbox() -> AgentRequest {
    AgentRequest::ReadInbox {
        after: None,
        limit: None,
        ack_ids: Vec::new(),
        include_read: false,
    }
}
fn outbox(f: &ControlFixture, grant: &CaptureGrant, peer: &Peer, id: &str) -> Value {
    call(
        &f.fixture.registry,
        grant,
        peer,
        None,
        AgentRequest::InspectOutbox {
            message_id: Some(id.into()),
            after: None,
            limit: None,
        },
    )
    .unwrap()["message"]
        .clone()
}
fn barrier() -> (
    Arc<SpawnBarrier>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
) {
    let (entered, received) = std::sync::mpsc::sync_channel(1);
    let (resume, resumed) = std::sync::mpsc::sync_channel(1);
    (
        Arc::new(SpawnBarrier {
            entered,
            resume: Mutex::new(resumed),
        }),
        received,
        resume,
    )
}
fn change(f: &ControlFixture, transition: &str) {
    match transition {
        "bind" => bind(f, "root-B"),
        "unbind" => {
            event(
                f,
                CaptureEvent::Unbind {
                    expected_revision: binding(f).generation,
                    conversation: Some(binding(f)),
                },
            )
            .unwrap();
        }
        "aba" => {
            bind(f, "root-B");
            bind(f, "root-A");
        }
        _ => unreachable!(),
    }
}
fn race(f: &ControlFixture, transition: &str, request: AgentRequest, mutation: bool) {
    let (pause, entered, resume) = barrier();
    f.fixture
        .registry
        .inner
        .agents
        .pause_request(mutation, pause);
    let registry = f.fixture.registry.clone();
    let grant = f.grant.clone();
    let peer = Peer {
        pid: f.peer.pid,
        uid: f.peer.uid,
        birth: f.peer.birth.clone(),
    };
    let old_guard = guard(f);
    let worker = thread::spawn(move || call(&registry, &grant, &peer, Some(old_guard), request));
    let blocked = entered.recv_timeout(Duration::from_secs(8));
    if blocked.is_ok() {
        change(f, transition);
    }
    let _ = resume.send(());
    let result = worker.join().unwrap();
    blocked.expect("guarded request did not reach its consumer barrier");
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("binding guard changed"),
        "{error:#}"
    );
    // Inspection after home must use a newly established root, never the old guard.
    bind(f, "root-A");
}

#[test]
fn guarded_read_ack_send_and_cached_returns_are_fenced_at_the_consumer() {
    for transition in ["bind", "unbind", "aba"] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let (other, peer) = other(&f);
        let incoming = call(
            &f.fixture.registry,
            &other,
            &peer,
            None,
            send(&f.grant, "incoming", AgentDelivery::Quiet),
        )
        .unwrap();
        let id = incoming["message_id"].as_str().unwrap();
        for request in [
            inbox(),
            AgentRequest::ReadMessage {
                message_id: id.into(),
                offset: Some(0),
                max_bytes: None,
            },
        ] {
            race(&f, transition, request, true);
            assert!(outbox(&f, &other, &peer, id)["read_at"].is_null());
        }
        own_call(&f, inbox()).unwrap();
        race(
            &f,
            transition,
            AgentRequest::AckMessage {
                ids: vec![id.into()],
            },
            true,
        );
        assert!(outbox(&f, &other, &peer, id)["acknowledged_at"].is_null());
        race(
            &f,
            transition,
            send(&other, "not-sent", AgentDelivery::Quiet),
            true,
        );
        let sent = own_call(
            &f,
            AgentRequest::InspectOutbox {
                message_id: None,
                after: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(sent["messages"].as_array().unwrap().is_empty());
        let sent = own_call(&f, send(&other, "duplicate", AgentDelivery::Quiet)).unwrap();
        race(
            &f,
            transition,
            send(&other, "duplicate", AgentDelivery::Quiet),
            false,
        );
        assert_eq!(
            outbox(&f, &f.grant, &f.peer, sent["message_id"].as_str().unwrap())["delivery_mode"],
            "quiet"
        );
        // Both no-write fast paths must still validate the binding.
        race(
            &f,
            transition,
            AgentRequest::AckMessage { ids: Vec::new() },
            false,
        );
        race(&f, transition, inbox(), false);
        race(
            &f,
            transition,
            AgentRequest::SetTitle {
                title: "must-not-commit".into(),
            },
            true,
        );
        let who = own_call(&f, AgentRequest::Whoami { detail: true }).unwrap();
        assert_ne!(who["title"], "must-not-commit");
    }
}

#[test]
fn guarded_control_rechecks_after_observation_and_revokes_inflight_native_input() {
    for transition in ["bind", "unbind", "aba"] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let old =
            f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
        let lease = old.control_lease().unwrap();
        let (pause, entered, resume) = barrier();
        *f.fixture
            .registry
            .inner
            .control_observe_barrier
            .lock()
            .unwrap() = Some(pause);
        let registry = f.fixture.registry.clone();
        let grant = f.grant.clone();
        let peer = Peer {
            pid: f.peer.pid,
            uid: f.peer.uid,
            birth: f.peer.birth.clone(),
        };
        let old_guard = guard(&f);
        let pane_id = f.pane.clone();
        let worker = thread::spawn(move || {
            call(
                &registry,
                &grant,
                &peer,
                Some(old_guard),
                AgentRequest::Control {
                    request: Box::new(Operation::PaneRename {
                        pane_id,
                        title: "must-not-rename".into(),
                    }),
                    scope: AgentScope::Workspace,
                    reply: ControlReply::Receipt,
                },
            )
        });
        let blocked = entered.recv_timeout(Duration::from_secs(8));
        if blocked.is_ok() {
            change(&f, transition);
        }
        let _ = resume.send(());
        let result = worker.join().unwrap();
        blocked.unwrap();
        assert!(result.is_err());
        assert!(!lease.is_valid());
        assert_ne!(
            f.fixture
                .registry
                .snapshot()
                .unwrap()
                .panes
                .iter()
                .find(|p| p.id == f.pane)
                .unwrap()
                .title,
            "must-not-rename"
        );
        // An already retained pane handle and lease cannot bypass registry admission.
        let pane = f.fixture.registry.capture_pane(&f.pane).unwrap();
        assert!(
            pane.request_control(
                PaneRequest::Attach {
                    spec: ViewSpec {
                        size: Size {
                            cols: 80,
                            rows: 24,
                            cell_width: 8,
                            cell_height: 16
                        },
                        interactive: false,
                        visible: true,
                        input: true,
                        clipboard: false,
                    }
                },
                &lease
            )
            .is_err()
        );
        assert!(!f.fixture.home.join("control-input").exists());
    }
}

#[test]
fn unbind_retains_recovery_and_launch_prompt_but_revokes_every_native_authority() {
    let f = ControlFixture::new();
    bind(&f, "root-A");
    let original = binding(&f);
    for malformed in [
        AgentBindingGuard {
            conversation: NativeConversation {
                generation: 0,
                ..original.clone()
            },
            context_id: original.id.clone(),
        },
        AgentBindingGuard {
            conversation: NativeConversation {
                id: "wrong-root".into(),
                ..original.clone()
            },
            context_id: original.id.clone(),
        },
        AgentBindingGuard {
            conversation: NativeConversation {
                source: ConversationSource::OmpExtension,
                ..original.clone()
            },
            context_id: original.id.clone(),
        },
        AgentBindingGuard {
            conversation: original.clone(),
            context_id: String::new(),
        },
    ] {
        assert!(
            call(
                &f.fixture.registry,
                &f.grant,
                &f.peer,
                Some(malformed),
                AgentRequest::SetTitle {
                    title: "malformed".into()
                }
            )
            .is_err()
        );
    }
    assert!(own_call(&f, AgentRequest::Whoami { detail: true }).unwrap()["title"].is_null());
    event(
        &f,
        CaptureEvent::PromptSubmitted {
            conversation_id: original.id.clone(),
            source: original.source,
        },
    )
    .unwrap();
    let first = f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id]
        .managed
        .first_prompt_submitted_at_ms;
    let recovery = f
        .fixture
        .registry
        .inner
        .state
        .lock()
        .unwrap()
        .record
        .recoveries[&f.pane]
        .clone();
    let child = AgentBindingGuard {
        conversation: original.clone(),
        context_id: "trusted-child".into(),
    };
    assert!(
        call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(child.clone()),
            AgentRequest::SetTitle {
                title: "not-attested".into()
            }
        )
        .is_err()
    );
    event(
        &f,
        CaptureEvent::NativeContext {
            conversation: original.clone(),
            context_id: child.context_id.clone(),
        },
    )
    .unwrap();
    call(
        &f.fixture.registry,
        &f.grant,
        &f.peer,
        Some(child.clone()),
        AgentRequest::SetTitle {
            title: "attested".into(),
        },
    )
    .unwrap();
    let enrolled = f
        .fixture
        .registry
        .register_native_status(
            &Request::new(Operation::HarnessEvent {
                launch_id: f.grant.launch_id.clone(),
                token: f.grant.token.clone(),
                event: CaptureEvent::StatusEnroll {
                    conversation_id: original.id.clone(),
                    source: original.source,
                },
            }),
            &f.peer,
            &|| true,
        )
        .unwrap()
        .0;
    own_call(
        &f,
        AgentRequest::ReceiverRegister {
            session_id: original.id.clone(),
            mode: AgentReceiverMode::ActiveTurn,
            pid: f.peer.pid as u32,
        },
    )
    .unwrap();
    let old =
        f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
    event(
        &f,
        CaptureEvent::Unbind {
            expected_revision: original.generation,
            conversation: Some(original.clone()),
        },
    )
    .unwrap();
    {
        let captures = f.fixture.registry.inner.captures.lock().unwrap();
        let current = captures.lookup(&f.grant.launch_id, &f.grant.token).unwrap();
        assert!(current.managed.conversation.is_none());
        assert_eq!(current.binding_generation, 2);
        assert_eq!(current.managed.first_prompt_submitted_at_ms, first);
        assert!(!captures.native_status.contains_key(&f.grant.launch_id));
    }
    assert!(!old.control.is_valid());
    assert!(enrolled.ensure_active().is_err());
    assert_eq!(
        f.fixture
            .registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .recoveries[&f.pane],
        recovery
    );
    bind(&f, "root-A");
    assert_eq!(binding(&f).generation, 3);
    let unchanged_version = f.fixture.registry.inner.captures.lock().unwrap().version;
    let unchanged_revision = f.fixture.registry.snapshot().unwrap().revision;
    let stale = event(
        &f,
        CaptureEvent::Unbind {
            expected_revision: original.generation,
            conversation: Some(original.clone()),
        },
    )
    .unwrap();
    assert_eq!(stale["accepted"], false);
    assert_eq!(stale["binding"]["revision"], 3);
    assert_eq!(stale["binding"]["conversation"], json!(binding(&f)));
    assert!(
        event(
            &f,
            CaptureEvent::NativeContext {
                conversation: original,
                context_id: child.context_id.clone()
            }
        )
        .is_err()
    );
    assert!(
        call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(child),
            AgentRequest::Whoami { detail: false }
        )
        .is_err()
    );
    let current_child = AgentBindingGuard {
        conversation: binding(&f),
        context_id: "trusted-child".into(),
    };
    assert!(
        call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(current_child),
            AgentRequest::SetTitle {
                title: "old-child".into()
            }
        )
        .is_err()
    );
    assert_eq!(
        f.fixture.registry.inner.captures.lock().unwrap().version,
        unchanged_version
    );
    assert_eq!(
        f.fixture.registry.snapshot().unwrap().revision,
        unchanged_revision
    );
    assert_eq!(binding(&f).generation, 3);
    assert_eq!(
        f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id]
            .managed
            .first_prompt_submitted_at_ms,
        first
    );
    // The old receiver is not silently rebound by returning to the same root ID.
    let who = own_call(&f, AgentRequest::Whoami { detail: true }).unwrap();
    assert!(who["receiver_mode"].is_null());
}

#[test]
fn native_child_contexts_can_be_renewed_after_eviction_but_not_after_rebinding() {
    let f = ControlFixture::new();
    bind(&f, "root-A");
    let current = binding(&f);
    let expected = serde_json::to_value(&current).unwrap();
    let padding = "x".repeat(900);
    for index in 0..1024 {
        let context_id = format!("child-{index}-{padding}");
        event(
            &f,
            CaptureEvent::NativeContext {
                conversation: current.clone(),
                context_id: context_id.clone(),
            },
        )
        .unwrap();
        if index % 64 == 0 || index == 1023 {
            let reply = call(
                &f.fixture.registry,
                &f.grant,
                &f.peer,
                Some(AgentBindingGuard {
                    conversation: current.clone(),
                    context_id,
                }),
                AgentRequest::Whoami { detail: false },
            )
            .unwrap();
            assert_eq!(reply["conversation"], expected);
        }
    }
    let evicted = AgentBindingGuard {
        conversation: current.clone(),
        context_id: format!("child-0-{padding}"),
    };
    assert!(
        call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(evicted.clone()),
            AgentRequest::Whoami { detail: false },
        )
        .is_err()
    );
    for context_id in ["root-A".to_owned(), evicted.context_id.clone()] {
        for _ in 0..2 {
            event(
                &f,
                CaptureEvent::NativeContext {
                    conversation: current.clone(),
                    context_id: context_id.clone(),
                },
            )
            .unwrap();
        }
        let reply = call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(AgentBindingGuard {
                conversation: current.clone(),
                context_id,
            }),
            AgentRequest::Whoami { detail: false },
        )
        .unwrap();
        assert_eq!(reply["conversation"], expected);
    }
    for context_id in ["", "bad\ncontext", f.grant.token.as_str()] {
        assert!(
            event(
                &f,
                CaptureEvent::NativeContext {
                    conversation: current.clone(),
                    context_id: context_id.into(),
                },
            )
            .is_err()
        );
    }
    bind(&f, "root-B");
    assert!(
        call(
            &f.fixture.registry,
            &f.grant,
            &f.peer,
            Some(evicted),
            AgentRequest::Whoami { detail: false },
        )
        .is_err()
    );
    let next = binding(&f);
    event(
        &f,
        CaptureEvent::NativeContext {
            conversation: next.clone(),
            context_id: "fresh-child".into(),
        },
    )
    .unwrap();
    let reply = call(
        &f.fixture.registry,
        &f.grant,
        &f.peer,
        Some(AgentBindingGuard {
            conversation: next.clone(),
            context_id: "fresh-child".into(),
        }),
        AgentRequest::Whoami { detail: false },
    )
    .unwrap();
    assert_eq!(reply["conversation"], serde_json::to_value(next).unwrap());
}

#[test]
fn conditional_binding_storage_failure_is_fail_closed_without_erasing_recovery() {
    for replace in [false, true] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let (other, peer) = other(&f);
        let registered = own_call(
            &f,
            AgentRequest::ReceiverRegister {
                session_id: "root-A".into(),
                mode: AgentReceiverMode::ActiveTurn,
                pid: f.peer.pid as u32,
            },
        )
        .unwrap();
        call(
            &f.fixture.registry,
            &other,
            &peer,
            None,
            send(&f.grant, "pending-unbind", AgentDelivery::Notify),
        )
        .unwrap();
        let claimed = own_call(
            &f,
            AgentRequest::ReceiverSubscribe {
                receiver_id: registered["receiver_id"].as_str().unwrap().into(),
                wait_ms: 0,
            },
        )
        .unwrap();
        assert!(claimed["notification"].is_object());
        let old =
            f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
        let recovery = f
            .fixture
            .registry
            .inner
            .state
            .lock()
            .unwrap()
            .record
            .recoveries[&f.pane]
            .clone();
        let revision = snapshot(&f).revision;
        // A known stale precondition is not a mutation and must not consume the
        // injected commit failure or masquerade as a storage failure.
        f.fixture.registry.inner.agents.fail_next_commit();
        assert_cas_rejected(
            &f,
            CaptureEvent::Bind {
                conversation_id: "stale-root".into(),
                source: ConversationSource::ClaudeHook,
                expected_revision: Some(revision - 1),
            },
        );
        let failed = event(
            &f,
            if replace {
                CaptureEvent::Bind {
                    conversation_id: "root-A".into(),
                    source: ConversationSource::ClaudeHook,
                    expected_revision: Some(revision),
                }
            } else {
                CaptureEvent::Unbind {
                    expected_revision: revision,
                    conversation: Some(binding(&f)),
                }
            },
        )
        .unwrap_err();
        assert!(failed.to_string().contains("outcome unknown"));
        assert!(!old.control.is_valid());
        assert!(
            f.fixture
                .registry
                .inner
                .captures
                .lock()
                .unwrap()
                .lookup(&f.grant.launch_id, &f.grant.token)
                .is_err()
        );
        assert!(f.fixture.registry.inner.agents.check_health().is_err());
        assert_eq!(
            f.fixture
                .registry
                .inner
                .state
                .lock()
                .unwrap()
                .record
                .recoveries[&f.pane],
            recovery
        );
        assert!(proc::process(f.peer.pid).unwrap().is_some());
    }
}

#[test]
fn guarded_subscription_is_cancelled_by_binding_revocation_without_holding_writers() {
    for transition in ["bind", "unbind", "aba"] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let registered = own_call(
            &f,
            AgentRequest::ReceiverRegister {
                session_id: "root-A".into(),
                mode: AgentReceiverMode::ActiveTurn,
                pid: f.peer.pid as u32,
            },
        )
        .unwrap();
        let registry = f.fixture.registry.clone();
        let grant = f.grant.clone();
        let old_guard = guard(&f);
        let peer = Peer {
            pid: f.peer.pid,
            uid: f.peer.uid,
            birth: f.peer.birth.clone(),
        };
        let (entered, ready) = std::sync::mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let first = std::cell::Cell::new(true);
            registry.request_authenticated_cancellable(
                Request::new(Operation::Agent {
                    launch_id: grant.launch_id,
                    token: grant.token,
                    guard: Some(old_guard),
                    request: AgentRequest::ReceiverSubscribe {
                        receiver_id: registered["receiver_id"].as_str().unwrap().into(),
                        wait_ms: 25000,
                    },
                }),
                &peer,
                &|| {
                    if first.replace(false) {
                        entered.send(()).unwrap();
                    }
                    true
                },
            )
        });
        ready.recv_timeout(Duration::from_secs(8)).unwrap();
        assert!(f.fixture.registry.inner.transaction.try_lock().is_ok());
        change(&f, transition);
        assert!(worker.join().unwrap().is_err());
    }
}

#[test]
fn guarded_headless_read_cannot_return_old_root_content_after_cleanup() {
    for transition in ["bind", "unbind", "aba"] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let (pause, entered, resume) = barrier();
        *f.fixture
            .registry
            .inner
            .control_cleanup_barrier
            .lock()
            .unwrap() = Some(pause);
        let registry = f.fixture.registry.clone();
        let grant = f.grant.clone();
        let old_guard = guard(&f);
        let peer = Peer {
            pid: f.peer.pid,
            uid: f.peer.uid,
            birth: f.peer.birth.clone(),
        };
        let ControlAction::Native(operation) =
            resolve("pane.read", json!({"pane_id":f.pane})).unwrap()
        else {
            panic!("test expected native operation");
        };
        let worker = thread::spawn(move || {
            call(
                &registry,
                &grant,
                &peer,
                Some(old_guard),
                AgentRequest::Control {
                    request: Box::new(operation),
                    scope: AgentScope::Workspace,
                    reply: ControlReply::Receipt,
                },
            )
        });
        let blocked = entered.recv_timeout(Duration::from_secs(8));
        if blocked.is_ok() {
            change(&f, transition);
        }
        let _ = resume.send(());
        let result = worker.join().unwrap();
        blocked.unwrap();
        assert!(result.is_err());
        assert!(f.fixture.snapshot(&f.pane).views.is_empty());
    }
}

#[test]
fn admitted_child_requests_survive_cache_eviction_but_not_binding_revocation() {
    for control in [false, true] {
        for transition in ["evict", "bind", "unbind", "aba"] {
            let f = ControlFixture::new();
            bind(&f, "root-A");
            let conversation = binding(&f);
            let child = AgentBindingGuard {
                conversation: conversation.clone(),
                context_id: "admitted-child".into(),
            };
            event(
                &f,
                CaptureEvent::NativeContext {
                    conversation: conversation.clone(),
                    context_id: child.context_id.clone(),
                },
            )
            .unwrap();
            own_call(
                &f,
                AgentRequest::ReceiverRegister {
                    session_id: conversation.id.clone(),
                    mode: AgentReceiverMode::ActiveTurn,
                    pid: f.peer.pid as u32,
                },
            )
            .unwrap();
            let (pause, entered, resume) = barrier();
            if control {
                *f.fixture
                    .registry
                    .inner
                    .control_observe_barrier
                    .lock()
                    .unwrap() = Some(pause);
            } else {
                f.fixture.registry.inner.agents.pause_request(true, pause);
            }
            let registry = f.fixture.registry.clone();
            let grant = f.grant.clone();
            let peer = Peer {
                pid: f.peer.pid,
                uid: f.peer.uid,
                birth: f.peer.birth.clone(),
            };
            let admitted = child.clone();
            let request = if control {
                AgentRequest::Control {
                    request: Box::new(Operation::PaneRename {
                        pane_id: f.pane.clone(),
                        title: "admitted-before-eviction".into(),
                    }),
                    scope: AgentScope::Workspace,
                    reply: ControlReply::Receipt,
                }
            } else {
                AgentRequest::SetTitle {
                    title: "admitted-before-eviction".into(),
                }
            };
            let worker =
                thread::spawn(move || call(&registry, &grant, &peer, Some(admitted), request));
            let blocked = entered.recv_timeout(Duration::from_secs(8));
            if blocked.is_ok() {
                let padding = "x".repeat(900);
                // Below the entry bound, above the byte bound: old attestations
                // must turn over without retiring a root, receiver, or request.
                for index in 0..96 {
                    event(
                        &f,
                        CaptureEvent::NativeContext {
                            conversation: conversation.clone(),
                            context_id: format!("evict-{index}-{padding}"),
                        },
                    )
                    .unwrap();
                }
                let missing = call(
                    &f.fixture.registry,
                    &f.grant,
                    &f.peer,
                    Some(child.clone()),
                    AgentRequest::SetTitle {
                        title: "cache-miss-must-not-run".into(),
                    },
                )
                .unwrap_err();
                assert!(
                    missing
                        .to_string()
                        .contains("native context is not authorized")
                );
                assert_eq!(binding(&f), conversation);
                let who = own_call(&f, AgentRequest::Whoami { detail: true }).unwrap();
                assert_eq!(who["receiver_mode"], "active_turn");
                if transition != "evict" {
                    change(&f, transition);
                }
            }
            let _ = resume.send(());
            let result = worker.join().unwrap();
            blocked.unwrap();
            if transition == "evict" {
                result.unwrap();
                let who = own_call(&f, AgentRequest::Whoami { detail: true }).unwrap();
                let title = if control {
                    &who["pane_name"]
                } else {
                    &who["title"]
                };
                assert_eq!(title, "admitted-before-eviction");
                // A fresh trusted attestation admits a new request. No mutation
                // was automatically retried following the known cache miss.
                event(
                    &f,
                    CaptureEvent::NativeContext {
                        conversation,
                        context_id: child.context_id.clone(),
                    },
                )
                .unwrap();
                call(
                    &f.fixture.registry,
                    &f.grant,
                    &f.peer,
                    Some(child),
                    AgentRequest::SetTitle {
                        title: "explicitly-reattested".into(),
                    },
                )
                .unwrap();
            } else {
                assert!(result.is_err());
            }
        }
    }
}

#[test]
fn conditional_binding_receipts_fence_home_and_same_root_replacement_producers() {
    let f = ControlFixture::new();
    assert_eq!(
        snapshot(&f),
        NativeBindingSnapshot {
            revision: 0,
            conversation: None
        }
    );
    let reserved = event(
        &f,
        CaptureEvent::Unbind {
            conversation: None,
            expected_revision: 0,
        },
    )
    .unwrap();
    assert_eq!(
        reserved,
        json!({"accepted":true,"binding":{"revision":1,"conversation":null}})
    );
    assert_cas_rejected(
        &f,
        CaptureEvent::Bind {
            conversation_id: "late-old-root".into(),
            source: ConversationSource::ClaudeHook,
            expected_revision: Some(0),
        },
    );
    let first = conditional_bind(&f, "root-A", 1);
    assert_eq!(first.revision, 2);
    assert_eq!(first.conversation.as_ref().unwrap().generation, 2);
    let old =
        f.fixture.registry.inner.captures.lock().unwrap().launches[&f.grant.launch_id].clone();
    let replacement = conditional_bind(&f, "root-A", 2);
    assert_eq!(replacement.revision, 3);
    assert_eq!(replacement.conversation.as_ref().unwrap().generation, 3);
    assert!(
        !old.control.is_valid(),
        "same-root CAS must rotate ownership"
    );
    assert_cas_rejected(
        &f,
        CaptureEvent::Unbind {
            conversation: first.conversation.clone(),
            expected_revision: first.revision,
        },
    );
    let reserved = event(
        &f,
        CaptureEvent::Unbind {
            conversation: replacement.conversation.clone(),
            expected_revision: replacement.revision,
        },
    )
    .unwrap();
    assert_eq!(
        reserved,
        json!({"accepted":true,"binding":{"revision":4,"conversation":null}})
    );
    // Either side of an old unknown Bind is safe cleanup evidence, never
    // permission to look up and revoke a replacement producer's newer epoch.
    for stale in [
        CaptureEvent::Bind {
            conversation_id: "root-A".into(),
            source: ConversationSource::ClaudeHook,
            expected_revision: Some(3),
        },
        CaptureEvent::Unbind {
            conversation: None,
            expected_revision: 1,
        },
        CaptureEvent::Unbind {
            conversation: first.conversation,
            expected_revision: first.revision,
        },
    ] {
        assert_cas_rejected(&f, stale);
    }
    let current = conditional_bind(&f, "root-A", 4);
    assert_eq!(current.revision, 5);
    // A delayed old ACK cannot make an old close remove this same-ID owner.
    assert_cas_rejected(
        &f,
        CaptureEvent::Unbind {
            conversation: replacement.conversation,
            expected_revision: replacement.revision,
        },
    );
    let mut wrong = current.conversation.clone().unwrap();
    wrong.id = "not-the-current-root".into();
    assert_cas_rejected(
        &f,
        CaptureEvent::Unbind {
            conversation: Some(wrong),
            expected_revision: current.revision,
        },
    );
    assert_cas_rejected(
        &f,
        CaptureEvent::Unbind {
            conversation: None,
            expected_revision: current.revision,
        },
    );
}

#[test]
fn whoami_revision_and_conversation_share_the_fresh_context_snapshot() {
    for transition in ["bind", "unbind"] {
        let f = ControlFixture::new();
        bind(&f, "root-A");
        let (pause, entered, resume) = barrier();
        f.fixture.registry.inner.agents.pause_request(false, pause);
        let registry = f.fixture.registry.clone();
        let grant = f.grant.clone();
        let peer = Peer {
            pid: f.peer.pid,
            uid: f.peer.uid,
            birth: f.peer.birth.clone(),
        };
        let worker = thread::spawn(move || {
            call(
                &registry,
                &grant,
                &peer,
                None,
                AgentRequest::Whoami { detail: false },
            )
        });
        let blocked = entered.recv_timeout(Duration::from_secs(8));
        if blocked.is_ok() {
            change(&f, transition);
        }
        let _ = resume.send(());
        let result = worker.join().unwrap();
        blocked.expect("whoami did not reach its pre-context consumer barrier");
        let identity = result.unwrap();
        let current = snapshot(&f);
        assert_eq!(current.revision, 2);
        assert_eq!(identity["binding_revision"], current.revision);
        assert_eq!(identity["conversation"], json!(current.conversation));
    }
}
