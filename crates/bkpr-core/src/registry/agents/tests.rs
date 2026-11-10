use super::*;
use messages::SendInput;

fn launch(id: &str) -> Launch {
    let process = proc::process(std::process::id() as i32).unwrap().unwrap();
    Launch {
        control: Default::default(),
        root: process.clone(),
        pane_root: process,
        grant: CaptureGrant {
            launch_id: id.into(),
            token: "a".repeat(64),
            pane_id: format!("pane-{id}"),
            kind: HarnessKind::Omp,
        },
        managed: ManagedHarness {
            launch_id: id.into(),
            conversation: Some(NativeConversation {
                id: format!("native-{id}"),
                source: ConversationSource::OmpExtension,
                generation: 1,
            }),
            error: None,
            first_prompt_submitted_at_ms: None,
        },
    }
}
fn context(id: &str, workspace: Option<&str>) -> ContextFact {
    ContextFact {
        launch_id: id.into(),
        active: true,
        pane: PaneInfo {
            id: format!("pane-{id}"),
            workspace_id: workspace.map(str::to_owned),
            window_id: workspace.map(|id| format!("window-{id}")),
            title: id.into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: PathBuf::from("/"),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            harness_status_changed_at_ms: None,
            project: None,
            home: PathBuf::from("/"),
            isolated_environment: false,
        },
        workspace_name: workspace.map(str::to_owned),
        conversation: Some(NativeConversation {
            id: format!("native-{id}"),
            source: ConversationSource::OmpExtension,
            generation: 1,
        }),
        cwd: Some(PathBuf::from("/")),
        repository: Some(PathBuf::from("/repository.git")),
        registry_revision: 7,
        workspace_revision: Some(3),
        window_revision: Some(2),
        pane_revision: Some(5),
        first_prompt_submitted_at_ms: None,
        status: Value::Null,
    }
}
fn fixture() -> (tempfile::TempDir, Agents, BTreeMap<String, ContextFact>) {
    let dir = tempfile::tempdir().unwrap();
    let agents = Agents::open(dir.path()).unwrap();
    agents.register(&launch("a")).unwrap();
    agents.register(&launch("b")).unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect();
    (dir, agents, contexts)
}
fn send(
    agents: &Agents,
    contexts: &BTreeMap<String, ContextFact>,
    key: &str,
    text: &str,
    scope: AgentScope,
) -> Result<Value> {
    agents.transaction(|store| {
        messages::send(
            store,
            "a",
            &contexts["a"],
            contexts,
            SendInput {
                to: "b".into(),
                text: text.into(),
                idempotency_key: key.into(),
                scope,
                delivery: AgentDelivery::Notify,
                receiver_ready: false,
            },
            &"a".repeat(64),
        )
    })
}
fn claim(agents: &Agents, timestamp: u64) -> Notification {
    agents
        .transaction(|store| messages::claim(store, "b", "receiver", timestamp))
        .unwrap()
        .unwrap()
}

#[test]
fn crash_after_durable_acceptance_is_reconciled_without_adopting_inbox() {
    const ENV: &str = "BKPR_TEST_AGENT_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ENV) {
        let root = PathBuf::from(root);
        let service = Agents::open(&root).unwrap();
        service.register(&launch("a")).unwrap();
        service.register(&launch("b")).unwrap();
        let contexts = [
            ("a".into(), context("a", Some("one"))),
            ("b".into(), context("b", Some("one"))),
        ]
        .into_iter()
        .collect();
        let receipt = send(
            &service,
            &contexts,
            "crash-key",
            "durable body",
            AgentScope::Workspace,
        )
        .unwrap();
        assert_eq!(receipt["durable"], true);
        // Kill only this isolated test subprocess after the real fsync acceptance.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
        unreachable!();
    }
    let directory = tempfile::tempdir().unwrap();
    let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","registry::agents::tests::crash_after_durable_acceptance_is_reconciled_without_adopting_inbox","--nocapture"]).env(ENV,directory.path()).status().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    let saved = Agents::load(&directory.path().join("agents")).unwrap();
    assert_eq!(saved.messages.len(), 1);
    assert_eq!(
        saved
            .messages
            .values()
            .next()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .as_str(),
        "durable body"
    );
    let reopened = Agents::open(directory.path()).unwrap();
    let state = reopened.state.lock().unwrap();
    assert!(Agents::require_live(&state.store, "a").is_err());
    assert!(Agents::require_live(&state.store, "b").is_err());
    assert!(state.receivers.is_empty());
    assert_eq!(state.store.messages.len(), 1);
    let bytes = fs::read(directory.path().join("agents/state.json")).unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains(&"a".repeat(64)));
}

#[test]
fn exact_retry_survives_recipient_exit_and_membership_change_but_conflicts_fail() {
    let (_dir, service, mut contexts) = fixture();
    let sent = send(&service, &contexts, "same", "hello", AgentScope::Workspace).unwrap();
    contexts.get_mut("a").unwrap().active = false;
    contexts.get_mut("b").unwrap().pane.workspace_id = Some("moved".into());
    service.retire(&["b".into()], 123).unwrap();
    let retry = send(&service, &contexts, "same", "hello", AgentScope::Workspace).unwrap();
    assert_eq!(sent["message_id"], retry["message_id"]);
    assert_eq!(retry["duplicate"], true);
    assert!(
        send(
            &service,
            &contexts,
            "same",
            "changed",
            AgentScope::Workspace
        )
        .is_err()
    );
    assert!(send(&service, &contexts, "same", "hello", AgentScope::All).is_err());
    let changed_to = service.transaction(|store| {
        messages::send(
            store,
            "a",
            &contexts["a"],
            &contexts,
            SendInput {
                to: "a".into(),
                text: "hello".into(),
                idempotency_key: "same".into(),
                scope: AgentScope::Workspace,
                delivery: AgentDelivery::Notify,
                receiver_ready: false,
            },
            &"a".repeat(64),
        )
    });
    assert!(changed_to.is_err());
    assert_eq!(service.state.lock().unwrap().store.messages.len(), 1);
}

#[test]
fn window_membership_is_reported_without_narrowing_workspace_agent_scope() {
    let (_dir, service, mut contexts) = fixture();
    contexts.get_mut("b").unwrap().pane.window_id = Some("independent-window".into());
    assert!(
        send(
            &service,
            &contexts,
            "across-windows",
            "body",
            AgentScope::Workspace
        )
        .is_ok()
    );
    let state = service.state.lock().unwrap();
    let payload = public(&contexts["b"], &state.store.agents["b"], None, 0, false);
    assert_eq!(payload["window_id"], "independent-window");
    assert_eq!(payload["workspace_id"], "one");
}

#[test]
fn scopes_use_live_execution_membership_and_unknown_repository_fails_closed() {
    let (_dir, service, mut contexts) = fixture();
    contexts.get_mut("b").unwrap().pane.workspace_id = Some("other".into());
    assert!(
        send(
            &service,
            &contexts,
            "workspace",
            "body",
            AgentScope::Workspace
        )
        .is_err()
    );
    assert!(send(&service, &contexts, "session", "body", AgentScope::Session).is_ok());
    contexts.get_mut("b").unwrap().active = false;
    assert!(send(&service, &contexts, "inactive", "body", AgentScope::Session).is_err());
    assert!(send(&service, &contexts, "all", "body", AgentScope::All).is_ok());
    contexts.get_mut("a").unwrap().repository = None;
    contexts.get_mut("b").unwrap().repository = None;
    assert!(
        send(
            &service,
            &contexts,
            "unknown-repo",
            "body",
            AgentScope::Repository
        )
        .is_err()
    );
    contexts.get_mut("a").unwrap().repository = Some(PathBuf::from("/same-common.git"));
    contexts.get_mut("b").unwrap().repository = Some(PathBuf::from("/same-common.git"));
    assert!(
        send(
            &service,
            &contexts,
            "same-repo",
            "body",
            AgentScope::Repository
        )
        .is_ok()
    );
    contexts.get_mut("a").unwrap().pane.workspace_id = None;
    contexts.get_mut("b").unwrap().pane.workspace_id = None;
    contexts.get_mut("b").unwrap().active = true;
    assert!(
        send(
            &service,
            &contexts,
            "unknown-workspace",
            "body",
            AgentScope::Workspace
        )
        .is_err()
    );
}

#[test]
fn read_pages_do_not_ack_or_mark_hidden_bodies_and_ack_keeps_exact_receipt() {
    let (_dir, service, contexts) = fixture();
    let first = send(&service, &contexts, "one", "first", AgentScope::Workspace).unwrap();
    let second = send(&service, &contexts, "two", "second", AgentScope::Workspace).unwrap();
    let n = claim(&service, 100);
    let page = service
        .transaction_maybe(|store| messages::read(store, "b", None, Some(1), &[], false, 200))
        .unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["has_more"], true);
    {
        let state = service.state.lock().unwrap();
        assert!(
            state.store.messages[second["message_id"].as_str().unwrap()]
                .read_at
                .is_none()
        );
        assert!(state.store.agents["b"].notification.is_some());
        assert!(
            state.store.messages[first["message_id"].as_str().unwrap()]
                .acknowledged_at
                .is_none()
        );
    }
    let cursor = page["next_cursor"].as_str().unwrap();
    service
        .transaction_maybe(|store| {
            messages::read(store, "b", Some(cursor), Some(1), &[], false, 201)
        })
        .unwrap();
    assert!(
        service.state.lock().unwrap().store.agents["b"]
            .notification
            .is_none()
    );
    let id = first["message_id"].as_str().unwrap().to_owned();
    assert!(
        service
            .transaction_maybe(|store| messages::ack(store, "a", std::slice::from_ref(&id), 300))
            .is_err()
    );
    service
        .transaction_maybe(|store| messages::ack(store, "b", std::slice::from_ref(&id), 300))
        .unwrap();
    assert!(
        service.state.lock().unwrap().store.messages[&id]
            .text
            .is_none()
    );
    let retry = send(&service, &contexts, "one", "first", AgentScope::Workspace).unwrap();
    assert_eq!(retry["message_id"], id);
    assert_eq!(retry["duplicate"], true);
    assert!(retry.get("status").is_none());
    assert_eq!(
        service.state.lock().unwrap().store.messages[&id].acknowledged_at,
        Some(300)
    );
    // Read consumed the notification, but its exact generation receipt can settle late.
    service
        .transaction(|store| {
            messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64),
            )
        })
        .unwrap();
    assert_eq!(
        service.state.lock().unwrap().store.messages[&id].wake_state,
        "accepted"
    );
}

#[test]
fn ambiguous_dispatch_is_not_replayed_and_late_failure_cannot_undo_acceptance() {
    let (_dir, service, contexts) = fixture();
    let sent = send(&service, &contexts, "one", "body", AgentScope::Workspace).unwrap();
    let n = claim(&service, 100);
    service.maintain(30100).unwrap();
    {
        let state = service.state.lock().unwrap();
        let m = &state.store.messages[sent["message_id"].as_str().unwrap()];
        assert_eq!(m.wake_state, "unknown");
        assert!(m.wake_failure.is_some());
    }
    assert!(
        service
            .transaction(|store| messages::claim(store, "b", "replacement", 30200))
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .transaction(|store| messages::outcome(
                store,
                "b",
                "replacement",
                &n.id,
                n.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64)
            ))
            .is_err()
    );
    assert!(
        service
            .transaction(|store| messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation + 1,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64)
            ))
            .is_err()
    );
    service
        .transaction(|store| {
            messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64),
            )
        })
        .unwrap();
    service
        .transaction(|store| {
            messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation,
                AgentWakeOutcome::Failed {
                    reason: "late failure".into(),
                },
                &"a".repeat(64),
            )
        })
        .unwrap();
    let state = service.state.lock().unwrap();
    let m = &state.store.messages[sent["message_id"].as_str().unwrap()];
    assert_eq!(m.wake_state, "accepted");
    assert_eq!(m.wake_failure.as_ref().unwrap().state, "unknown");
}

#[test]
fn exit_retention_uses_first_persisted_observation_and_preserves_wake_history() {
    let (dir, service, contexts) = fixture();
    let sent = send(
        &service,
        &contexts,
        "one",
        "retained",
        AgentScope::Workspace,
    )
    .unwrap();
    let n = claim(&service, 100);
    service
        .transaction(|store| {
            messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64),
            )
        })
        .unwrap();
    service.retire(&["b".into()], 1000).unwrap();
    service.retire(&["b".into()], 9000).unwrap();
    let path = dir.path().join("agents/state.json");
    let before = fs::read(&path).unwrap();
    let diagnostic = summary(
        &service.state.lock().unwrap().store.messages[sent["message_id"].as_str().unwrap()],
    );
    assert_eq!(diagnostic["delivery"]["state"], "accepted");
    assert_eq!(before, fs::read(&path).unwrap());
    service.maintain(1000 + RETENTION_MS - 1).unwrap();
    assert!(
        service.state.lock().unwrap().store.messages[sent["message_id"].as_str().unwrap()]
            .text
            .is_some()
    );
    service.maintain(1000 + RETENTION_MS).unwrap();
    let state = service.state.lock().unwrap();
    let m = &state.store.messages[sent["message_id"].as_str().unwrap()];
    assert!(m.text.is_none());
    assert_eq!(m.expired_at, Some(1000 + RETENTION_MS));
    assert_eq!(m.wake_state, "accepted");
    drop(state);
    assert_eq!(
        send(
            &service,
            &contexts,
            "one",
            "retained",
            AgentScope::Workspace
        )
        .unwrap()["message_id"],
        sent["message_id"]
    );
}

#[test]
fn store_failure_suspends_mutations_and_callbacks_even_after_path_is_repaired() {
    let (dir, service, contexts) = fixture();
    let sent = send(
        &service,
        &contexts,
        "accepted",
        "body",
        AgentScope::Workspace,
    )
    .unwrap();
    let n = claim(&service, 100);
    let state_path = dir.path().join("agents/state.json");
    let saved = fs::read(&state_path).unwrap();
    fs::remove_file(&state_path).unwrap();
    let external = dir.path().join("must-not-follow");
    fs::write(&external, b"untouched").unwrap();
    std::os::unix::fs::symlink(&external, &state_path).unwrap();
    assert!(
        send(
            &service,
            &contexts,
            "not-accepted",
            "new",
            AgentScope::Workspace
        )
        .is_err()
    );
    assert_eq!(fs::read(&external).unwrap(), b"untouched");
    fs::remove_file(&state_path).unwrap();
    fs::write(&state_path, &saved).unwrap();
    fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(service.register(&launch("new")).is_err());
    assert!(service.retire(&["b".into()], 500).is_err());
    assert!(
        service
            .transaction(|store| messages::outcome(
                store,
                "b",
                "receiver",
                &n.id,
                n.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64)
            ))
            .is_err()
    );
    let reconciled = Agents::open(dir.path()).unwrap();
    let state = reconciled.state.lock().unwrap();
    assert_eq!(state.store.messages.len(), 1);
    assert!(
        state
            .store
            .messages
            .contains_key(sent["message_id"].as_str().unwrap())
    );
}

#[test]
fn replaced_store_directory_is_not_recreated_or_followed() {
    let (dir, service, contexts) = fixture();
    fs::rename(
        dir.path().join("agents"),
        dir.path().join("original-agents"),
    )
    .unwrap();
    fs::create_dir(dir.path().join("agents")).unwrap();
    fs::set_permissions(dir.path().join("agents"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(send(&service, &contexts, "new", "body", AgentScope::Workspace).is_err());
    assert!(!dir.path().join("agents/state.json").exists());
}

#[test]
fn stale_receiver_birth_and_conversation_generation_reject_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(dir.path()).unwrap();
    let launch = launch("a");
    registry.inner.agents.register(&launch).unwrap();
    registry
        .inner
        .captures
        .lock()
        .unwrap()
        .launches
        .insert("a".into(), launch.clone());
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let registered = registry
        .agent_receiver_register(
            &launch,
            &peer,
            "native-a".into(),
            AgentReceiverMode::ActiveTurn,
            std::process::id(),
        )
        .unwrap();
    let receiver_id = registered["receiver_id"].as_str().unwrap();
    registry
        .agent_receiver_peer(&launch, &peer, receiver_id)
        .unwrap();
    registry
        .inner
        .captures
        .lock()
        .unwrap()
        .launches
        .get_mut("a")
        .unwrap()
        .managed
        .conversation
        .as_mut()
        .unwrap()
        .generation = 2;
    assert!(
        registry
            .agent_receiver_peer(&launch, &peer, receiver_id)
            .is_err()
    );
    registry
        .inner
        .captures
        .lock()
        .unwrap()
        .launches
        .get_mut("a")
        .unwrap()
        .managed
        .conversation
        .as_mut()
        .unwrap()
        .generation = 1;
    registry
        .inner
        .agents
        .state
        .lock()
        .unwrap()
        .receivers
        .get_mut("a")
        .unwrap()
        .process
        .identity = "recycled-birth".into();
    assert!(
        registry
            .agent_receiver_peer(&launch, &peer, receiver_id)
            .is_err()
    );
    registry.shutdown().unwrap();
}

#[test]
fn empty_reads_and_duplicate_acks_do_not_commit_storage() {
    let (_dir, service, contexts) = fixture();
    let request = AgentRequest::ReadInbox {
        after: None,
        limit: Some(1),
        ack_ids: Vec::new(),
        include_read: false,
    };
    assert!(
        messages::unchanged(&service.state.lock().unwrap().store, "b", &request)
            .unwrap()
            .is_some()
    );
    let state_path = service.root.join("state.json");
    let initial_inode = fs::metadata(&state_path).unwrap().ino();
    service
        .transaction_maybe(|store| messages::read(store, "b", None, Some(1), &[], false, 100))
        .unwrap();
    assert_eq!(initial_inode, fs::metadata(&state_path).unwrap().ino());
    let sent = send(
        &service,
        &contexts,
        "one",
        "large immutable body",
        AgentScope::Workspace,
    )
    .unwrap();
    let id = sent["message_id"].as_str().unwrap().to_owned();
    service
        .transaction_maybe(|store| messages::ack(store, "b", std::slice::from_ref(&id), 100))
        .unwrap();
    let before = fs::metadata(&state_path).unwrap().ino();
    let duplicate = AgentRequest::AckMessage {
        ids: vec![id.clone()],
    };
    assert_eq!(
        messages::unchanged(&service.state.lock().unwrap().store, "b", &duplicate)
            .unwrap()
            .unwrap()["acknowledged"],
        0
    );
    service
        .transaction_maybe(|store| messages::ack(store, "b", std::slice::from_ref(&id), 101))
        .unwrap();
    assert_eq!(before, fs::metadata(&state_path).unwrap().ino());
}

fn receiver_fixture() -> (tempfile::TempDir, Registry, Launch, String) {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(dir.path()).unwrap();
    let launch = launch("a");
    registry.inner.agents.register(&launch).unwrap();
    registry
        .inner
        .captures
        .lock()
        .unwrap()
        .launches
        .insert("a".into(), launch.clone());
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let result = registry
        .agent_receiver_register(
            &launch,
            &peer,
            "native-a".into(),
            AgentReceiverMode::ActiveTurn,
            std::process::id(),
        )
        .unwrap();
    assert_eq!(result["notification_ready"], false);
    let receiver_id = result["receiver_id"].as_str().unwrap().to_owned();
    (dir, registry, launch, receiver_id)
}

#[test]
fn subscriptions_are_bounded_and_shutdown_wakes_real_waiters() {
    let (_dir, registry, launch, receiver_id) = receiver_fixture();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut workers = Vec::new();
    for _ in 0..2 {
        let registry = registry.clone();
        let launch = launch.clone();
        let receiver_id = receiver_id.clone();
        let ready = ready_tx.clone();
        workers.push(std::thread::spawn(move || {
            let peer = Peer {
                pid: std::process::id() as i32,
                uid: unsafe { libc::geteuid() },
                birth: launch.root.identity.clone(),
            };
            let first = std::cell::Cell::new(true);
            registry.agent_subscribe(&launch, &peer, receiver_id, 25000, &|| {
                if first.replace(false) {
                    ready.send(()).unwrap();
                }
                true
            })
        }));
    }
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let excess = registry
        .agent_subscribe(&launch, &peer, receiver_id, 0, &|| true)
        .unwrap_err();
    assert!(excess.to_string().contains("capacity"));
    registry.shutdown().unwrap();
    for worker in workers {
        assert!(worker.join().unwrap().is_err());
    }
    assert_eq!(
        registry
            .inner
            .agents
            .state
            .lock()
            .unwrap()
            .waiters
            .get("a")
            .map_or(0, |waiters| waiters.values().copied().sum::<usize>()),
        0
    );
}

#[test]
fn committed_send_pushes_pointer_without_reading_and_disconnect_retires_readiness() {
    let (_dir, registry, launch, receiver_id) = receiver_fixture();
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    assert!(
        registry
            .agent_subscribe(&launch, &peer, receiver_id.clone(), 25000, &|| false)
            .is_err()
    );
    assert_eq!(
        registry
            .inner
            .agents
            .state
            .lock()
            .unwrap()
            .waiters
            .get("a")
            .map_or(0, |waiters| waiters.values().copied().sum::<usize>()),
        0
    );
    registry
        .inner
        .agents
        .register(&super::tests::launch("b"))
        .unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let worker_registry = registry.clone();
    let worker_launch = launch.clone();
    let worker_receiver = receiver_id.clone();
    let worker = std::thread::spawn(move || {
        let peer = Peer {
            pid: std::process::id() as i32,
            uid: unsafe { libc::geteuid() },
            birth: worker_launch.root.identity.clone(),
        };
        let first = std::cell::Cell::new(true);
        worker_registry.agent_subscribe(&worker_launch, &peer, worker_receiver, 25000, &|| {
            if first.replace(false) {
                ready_tx.send(()).unwrap();
            }
            true
        })
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let sent = registry
        .inner
        .agents
        .transaction(|store| {
            messages::send(
                store,
                "b",
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: "private peer body".into(),
                    idempotency_key: "push".into(),
                    scope: AgentScope::Workspace,
                    delivery: AgentDelivery::Notify,
                    receiver_ready: true,
                },
                &"a".repeat(64),
            )
        })
        .unwrap();
    assert_eq!(sent["wake"]["state"], "pending");
    let notification = worker.join().unwrap().unwrap();
    assert_eq!(
        notification["notification"]["notification_id"],
        sent["message_id"]
    );
    assert!(!notification.to_string().contains("private peer body"));
    let state = registry.inner.agents.state.lock().unwrap();
    assert!(
        state.store.messages[sent["message_id"].as_str().unwrap()]
            .read_at
            .is_none()
    );
    drop(state);
    let id = notification["notification"]["notification_id"]
        .as_str()
        .unwrap();
    let generation = notification["notification"]["generation"].as_u64().unwrap();
    registry
        .agent_subscription_disconnected("a", &receiver_id, id, generation + 1)
        .unwrap();
    assert_eq!(
        registry.inner.agents.state.lock().unwrap().store.messages[id].wake_state,
        "pending"
    );
    registry
        .agent_subscription_disconnected("a", &receiver_id, id, generation)
        .unwrap();
    assert_eq!(
        registry.inner.agents.state.lock().unwrap().store.messages[id].wake_state,
        "unknown"
    );
    registry.shutdown().unwrap();
}

#[test]
fn conversation_rebind_wins_over_receiver_registration_waiting_on_writer() {
    let (_dir, registry, launch, _receiver) = receiver_fixture();
    let writer = registry.inner.agents.callback_guard().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let worker_registry = registry.clone();
    let old_launch = launch.clone();
    let worker = std::thread::spawn(move || {
        let peer = Peer {
            pid: std::process::id() as i32,
            uid: unsafe { libc::geteuid() },
            birth: old_launch.root.identity.clone(),
        };
        entered_tx.send(()).unwrap();
        worker_registry.agent_receiver_register(
            &old_launch,
            &peer,
            "native-a".into(),
            AgentReceiverMode::ActiveTurn,
            std::process::id(),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    registry
        .inner
        .captures
        .lock()
        .unwrap()
        .event(
            "a",
            &launch.grant.token,
            CaptureEvent::Bind {
                conversation_id: "replacement-native".into(),
                source: ConversationSource::OmpExtension,
            },
        )
        .unwrap();
    drop(writer);
    assert!(worker.join().unwrap().is_err());
    registry.shutdown().unwrap();
}

#[test]
fn historical_pending_receipt_settles_after_read_without_touching_new_generation() {
    let (_dir, service, contexts) = fixture();
    let first = send(&service, &contexts, "one", "first", AgentScope::Workspace).unwrap();
    let n1 = claim(&service, 100);
    service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 200))
        .unwrap();
    let second = send(&service, &contexts, "two", "second", AgentScope::Workspace).unwrap();
    let n2 = claim(&service, 100000);
    service.maintain(30100).unwrap();
    let state = service.state.lock().unwrap();
    assert_eq!(
        state.store.messages[first["message_id"].as_str().unwrap()].wake_state,
        "unknown"
    );
    assert_eq!(
        state.store.messages[second["message_id"].as_str().unwrap()].wake_state,
        "pending"
    );
    assert_eq!(
        state.store.agents["b"]
            .notification
            .as_ref()
            .unwrap()
            .generation,
        n2.generation
    );
    drop(state);
    service
        .transaction(|store| {
            messages::outcome(
                store,
                "b",
                "receiver",
                &n1.id,
                n1.generation,
                AgentWakeOutcome::Accepted,
                &"a".repeat(64),
            )
        })
        .unwrap();
    let state = service.state.lock().unwrap();
    assert_eq!(
        state.store.messages[first["message_id"].as_str().unwrap()].wake_state,
        "accepted"
    );
    assert_eq!(
        state.store.agents["b"]
            .notification
            .as_ref()
            .unwrap()
            .generation,
        n2.generation
    );
}

#[test]
fn native_rebind_does_not_replay_old_ambiguity_but_new_messages_get_new_generation() {
    let (_dir, registry, launch, receiver) = receiver_fixture();
    registry
        .inner
        .agents
        .register(&super::tests::launch("b"))
        .unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let send_to_a = |key: &str| {
        registry.inner.agents.transaction(|store| {
            messages::send(
                store,
                "b",
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: key.into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    delivery: AgentDelivery::Notify,
                    receiver_ready: true,
                },
                &"a".repeat(64),
            )
        })
    };
    let old = send_to_a("old").unwrap();
    let n1 = registry
        .inner
        .agents
        .transaction(|store| messages::claim(store, "a", &receiver, 100))
        .unwrap()
        .unwrap();
    registry
        .agent_capture_event(
            "a",
            &launch.grant.token,
            CaptureEvent::Bind {
                conversation_id: "replacement-native".into(),
                source: ConversationSource::OmpExtension,
            },
        )
        .unwrap();
    assert!(registry.agent_receiver(&launch, &receiver).is_err());
    assert!(
        registry
            .inner
            .agents
            .transaction(|store| messages::claim(store, "a", "new-receiver", 200))
            .unwrap()
            .is_none()
    );
    let new = send_to_a("new").unwrap();
    let n2 = registry
        .inner
        .agents
        .transaction(|store| messages::claim(store, "a", "new-receiver", 300))
        .unwrap()
        .unwrap();
    let state = registry.inner.agents.state.lock().unwrap();
    let old_message = &state.store.messages[old["message_id"].as_str().unwrap()];
    assert_eq!(old_message.generation, Some(n1.generation));
    assert_eq!(old_message.wake_state, "unknown");
    assert_eq!(
        state.store.messages[new["message_id"].as_str().unwrap()].generation,
        Some(n2.generation)
    );
    assert!(n2.generation > n1.generation);
    drop(state);
    registry.shutdown().unwrap();
}

#[test]
fn receiver_mode_replacement_retires_old_claim_without_replaying_it() {
    let (_dir, registry, launch, receiver) = receiver_fixture();
    registry
        .inner
        .agents
        .register(&super::tests::launch("b"))
        .unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let send_to_a = |key: &str| {
        registry.inner.agents.transaction(|store| {
            messages::send(
                store,
                "b",
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: key.into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    delivery: AgentDelivery::Notify,
                    receiver_ready: true,
                },
                &"a".repeat(64),
            )
        })
    };
    let first = send_to_a("old").unwrap();
    let old = registry
        .inner
        .agents
        .transaction(|store| messages::claim(store, "a", &receiver, 100))
        .unwrap()
        .unwrap();
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let replacement = registry
        .agent_receiver_register(
            &launch,
            &peer,
            "native-a".into(),
            AgentReceiverMode::FutureTurn,
            std::process::id(),
        )
        .unwrap();
    let replacement = replacement["receiver_id"].as_str().unwrap();
    assert_ne!(replacement, receiver);
    assert!(registry.agent_receiver(&launch, &receiver).is_err());
    assert!(
        registry
            .inner
            .agents
            .transaction(|store| messages::claim(store, "a", replacement, 200))
            .unwrap()
            .is_none()
    );
    let second = send_to_a("new").unwrap();
    let new = registry
        .inner
        .agents
        .transaction(|store| messages::claim(store, "a", replacement, 300))
        .unwrap()
        .unwrap();
    let state = registry.inner.agents.state.lock().unwrap();
    assert_eq!(
        state.store.messages[first["message_id"].as_str().unwrap()].wake_state,
        "unknown"
    );
    assert_eq!(
        state.store.messages[first["message_id"].as_str().unwrap()].generation,
        Some(old.generation)
    );
    assert_eq!(
        state.store.messages[second["message_id"].as_str().unwrap()].generation,
        Some(new.generation)
    );
    assert!(new.generation > old.generation);
    drop(state);
    registry.shutdown().unwrap();
}

#[test]
fn replacement_readiness_excludes_old_live_subscription() {
    let (_dir, registry, launch, receiver) = receiver_fixture();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let worker_registry = registry.clone();
    let worker_launch = launch.clone();
    let worker = std::thread::spawn(move || {
        let peer = Peer {
            pid: std::process::id() as i32,
            uid: unsafe { libc::geteuid() },
            birth: worker_launch.root.identity.clone(),
        };
        worker_registry.agent_subscribe(&worker_launch, &peer, receiver, 25000, &|| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            true
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    {
        let state = registry.inner.agents.state.lock().unwrap();
        assert_eq!(current_subscribers(&state, "a"), 1);
    }
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let replacement = registry
        .agent_receiver_register(
            &launch,
            &peer,
            "native-a".into(),
            AgentReceiverMode::FutureTurn,
            std::process::id(),
        )
        .unwrap();
    {
        let state = registry.inner.agents.state.lock().unwrap();
        let identity = public(
            &context("a", Some("one")),
            &state.store.agents["a"],
            state.receivers.get("a"),
            current_subscribers(&state, "a"),
            true,
        );
        assert_eq!(identity["notification_ready"], false);
    }
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let worker_registry = registry.clone();
    let worker_launch = launch.clone();
    let receiver = replacement["receiver_id"].as_str().unwrap().to_owned();
    let worker = std::thread::spawn(move || {
        let peer = Peer {
            pid: std::process::id() as i32,
            uid: unsafe { libc::geteuid() },
            birth: worker_launch.root.identity.clone(),
        };
        worker_registry.agent_subscribe(&worker_launch, &peer, receiver, 0, &|| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            true
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    {
        let state = registry.inner.agents.state.lock().unwrap();
        let identity = public(
            &context("a", Some("one")),
            &state.store.agents["a"],
            state.receivers.get("a"),
            current_subscribers(&state, "a"),
            true,
        );
        assert_eq!(identity["notification_ready"], true);
    }
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
    registry.shutdown().unwrap();
}

#[test]
fn rename_then_sync_failure_reports_unknown_and_restart_reconciles_committed_send() {
    let (dir, service, contexts) = fixture();
    service
        .fail_directory_sync
        .store(true, std::sync::atomic::Ordering::Release);
    let failure = send(
        &service,
        &contexts,
        "uncertain",
        "committed before sync error",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(failure.to_string().contains("outcome unknown"));
    let persisted = Agents::load(&dir.path().join("agents")).unwrap();
    assert_eq!(persisted.messages.len(), 1);
    let saved = persisted.messages.values().next().unwrap();
    assert_eq!(
        saved.text.as_ref().unwrap().as_str(),
        "committed before sync error"
    );
    assert!(
        send(
            &service,
            &contexts,
            "another",
            "must fail closed",
            AgentScope::Workspace
        )
        .is_err()
    );
    assert!(service.register(&launch("new")).is_err());
    let restarted = Agents::open(dir.path()).unwrap();
    let state = restarted.state.lock().unwrap();
    assert_eq!(state.store.messages.len(), 1);
    assert!(state.store.messages.contains_key(&saved.id));
    assert!(Agents::require_live(&state.store, "a").is_err());
}

#[test]
fn inbox_defaults_to_unread_and_combines_only_explicit_previously_read_acks_atomically() {
    let (_dir, service, contexts) = fixture();
    let first = send(
        &service,
        &contexts,
        "first",
        "processed later",
        AgentScope::Workspace,
    )
    .unwrap();
    let first_id = first["message_id"].as_str().unwrap().to_owned();
    let read = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 10))
        .unwrap();
    assert_eq!(read["messages"][0]["id"], first_id);
    assert_eq!(read["acknowledged"], 0);
    let second = send(
        &service,
        &contexts,
        "second",
        "new task",
        AgentScope::Workspace,
    )
    .unwrap();
    let second_id = second["message_id"].as_str().unwrap().to_owned();
    let invalid = service.transaction_maybe(|store| {
        messages::read(
            store,
            "b",
            None,
            None,
            &[first_id.clone(), second_id.clone()],
            false,
            20,
        )
    });
    assert!(invalid.is_err());
    let before = service.state.lock().unwrap().store.messages.clone();
    assert!(before[&first_id].acknowledged_at.is_none());
    assert!(before[&second_id].read_at.is_none());
    let page = service
        .transaction_maybe(|store| {
            messages::read(
                store,
                "b",
                None,
                None,
                std::slice::from_ref(&first_id),
                false,
                30,
            )
        })
        .unwrap();
    assert_eq!(page["acknowledged"], 1);
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["id"], second_id);
    let empty = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 40))
        .unwrap();
    assert!(empty["messages"].as_array().unwrap().is_empty());
    let recovery = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], true, 50))
        .unwrap();
    assert_eq!(recovery["messages"][0]["id"], second_id);
    assert!(
        service
            .transaction_maybe(|store| messages::read(
                store,
                "a",
                None,
                None,
                std::slice::from_ref(&second_id),
                false,
                60
            ))
            .is_err()
    );
    let saved = service.state.lock().unwrap().store.messages.clone();
    assert_eq!(saved[&first_id].acknowledged_at, Some(30));
    assert!(saved[&first_id].text.is_none());
    assert!(saved[&second_id].acknowledged_at.is_none());
    assert!(saved[&second_id].text.is_some());
}

#[test]
fn large_inbox_snippets_and_recipient_only_utf8_pages_preserve_every_body_byte() {
    let (_dir, service, contexts) = fixture();
    let body = "🦀".repeat(2048);
    for index in 0..12 {
        send(
            &service,
            &contexts,
            &format!("large-{index}"),
            &body,
            AgentScope::Workspace,
        )
        .unwrap();
    }
    let page = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 10))
        .unwrap();
    let rows = page["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(page["has_more"], true);
    assert!(serde_json::to_vec(&page).unwrap().len() < 8192);
    for row in rows {
        assert_eq!(row["body_complete"], false);
        assert_eq!(row["body_bytes"], body.len());
        assert!(row["text"].as_str().unwrap().len() <= 192);
    }
    let message_id = rows[0]["id"].as_str().unwrap();
    assert!(
        service
            .transaction_maybe(|store| messages::read_message(
                store, "a", message_id, None, None, 20
            ))
            .is_err()
    );
    assert!(
        service
            .transaction_maybe(|store| messages::read_message(
                store,
                "b",
                message_id,
                Some(1),
                None,
                20
            ))
            .is_err()
    );
    let mut restored = rows[0]["text"].as_str().unwrap().to_owned();
    let mut offset = rows[0]["next_offset"].as_u64().unwrap() as usize;
    loop {
        let part = service
            .transaction_maybe(|store| {
                messages::read_message(store, "b", message_id, Some(offset), Some(4095), 20)
            })
            .unwrap();
        assert_eq!(part["offset"], offset);
        let text = part["text"].as_str().unwrap();
        assert!(text.len() <= 4095);
        restored.push_str(text);
        if part["has_more"] == false {
            assert!(part.get("next_offset").is_none());
            break;
        }
        let next = part["next_offset"].as_u64().unwrap() as usize;
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(restored, body);
    let remaining = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 30))
        .unwrap();
    assert_eq!(remaining["messages"].as_array().unwrap().len(), 2);
    assert_eq!(remaining["has_more"], false);
    let retained = service.state.lock().unwrap().store.messages[message_id].clone();
    assert!(retained.acknowledged_at.is_none());
    assert_eq!(retained.text.as_ref().unwrap().as_str(), body);
}

#[test]
fn quiet_delivery_never_wakes_or_joins_a_claim_and_changed_delivery_conflicts() {
    let (_dir, registry, launch, receiver_id) = receiver_fixture();
    registry
        .inner
        .agents
        .register(&super::tests::launch("b"))
        .unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let send_to_a = |key: &str, delivery| {
        registry.inner.agents.transaction(|store| {
            messages::send(
                store,
                "b",
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: "body-free wake test body".into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    delivery,
                    receiver_ready: false,
                },
                &"a".repeat(64),
            )
        })
    };
    let quiet = send_to_a("quiet", AgentDelivery::Quiet).unwrap();
    assert_eq!(quiet["wake"]["state"], "quiet");
    assert!(send_to_a("quiet", AgentDelivery::Notify).is_err());
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let empty = registry
        .agent_subscribe(&launch, &peer, receiver_id.clone(), 0, &|| true)
        .unwrap();
    assert!(empty["notification"].is_null());
    let notifying = send_to_a("notify", AgentDelivery::Notify).unwrap();
    assert_eq!(notifying["wake"]["state"], "blocked");
    let pointer = registry
        .agent_subscribe(&launch, &peer, receiver_id.clone(), 0, &|| true)
        .unwrap();
    assert_eq!(pointer["notification"]["message_count"], 1);
    assert_eq!(pointer["notification"]["unread_count"], 2);
    assert!(!pointer.to_string().contains("body-free wake test body"));
    let later_quiet = send_to_a("later-quiet", AgentDelivery::Quiet).unwrap();
    let saved = registry
        .inner
        .agents
        .state
        .lock()
        .unwrap()
        .store
        .messages
        .clone();
    for receipt in [quiet, later_quiet] {
        let message = &saved[receipt["message_id"].as_str().unwrap()];
        assert!(message.notification_id.is_none());
        assert!(message.read_at.is_none());
        assert!(message.acknowledged_at.is_none());
    }
    registry.shutdown().unwrap();
}

#[test]
fn timed_out_claim_releases_only_its_slot_and_later_distinct_messages_wake() {
    let (_dir, service, contexts) = fixture();
    let old = send(
        &service,
        &contexts,
        "old",
        "ambiguous old wake",
        AgentScope::Workspace,
    )
    .unwrap();
    let old_claim = claim(&service, 100);
    service.maintain(30100).unwrap();
    let old_id = old["message_id"].as_str().unwrap();
    let old_outcome = {
        let state = service.state.lock().unwrap();
        summary(&state.store.messages[old_id])
    };
    assert_eq!(old_outcome["delivery"]["state"], "unknown");
    assert!(
        service
            .transaction(|store| messages::claim(store, "b", "receiver", 30200))
            .unwrap()
            .is_none()
    );
    let new = send(
        &service,
        &contexts,
        "new",
        "distinct wake",
        AgentScope::Workspace,
    )
    .unwrap();
    let new_claim = claim(&service, 30300);
    assert_ne!(new_claim.id, old_claim.id);
    assert!(new_claim.generation > old_claim.generation);
    assert_eq!(new_claim.id, new["message_id"]);
    let unchanged = {
        let state = service.state.lock().unwrap();
        summary(&state.store.messages[old_id])
    };
    assert_eq!(unchanged, old_outcome);
}

#[test]
fn compact_identity_and_scoped_exact_discovery_are_bounded_and_pageable() {
    let (_dir, service, mut contexts) = fixture();
    for index in 0..25 {
        let id = format!("peer-{index:02}");
        service.register(&launch(&id)).unwrap();
        contexts.insert(
            id.clone(),
            context(&id, Some(if index < 13 { "other" } else { "one" })),
        );
    }
    let state = service.state.lock().unwrap();
    let own = &contexts["a"];
    let identity = public(own, &state.store.agents["a"], None, 0, false);
    let detailed = public(own, &state.store.agents["a"], None, 0, true);
    let first = discover(
        &state,
        &contexts,
        own,
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: false,
            launch_id: None,
            pane_id: None,
            after: None,
            limit: None,
        },
    )
    .unwrap();
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = discover(
        &state,
        &contexts,
        own,
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: false,
            launch_id: None,
            pane_id: None,
            after: Some(cursor),
            limit: None,
        },
    )
    .unwrap();
    let exact = discover(
        &state,
        &contexts,
        own,
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: false,
            launch_id: Some("peer-20"),
            pane_id: Some("pane-peer-20"),
            after: None,
            limit: Some(1),
        },
    )
    .unwrap();
    let foreign = discover(
        &state,
        &contexts,
        own,
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: false,
            launch_id: Some("peer-00"),
            pane_id: None,
            after: None,
            limit: None,
        },
    )
    .unwrap();
    let mismatch = discover(
        &state,
        &contexts,
        own,
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: false,
            launch_id: Some("peer-20"),
            pane_id: Some("pane-b"),
            after: None,
            limit: None,
        },
    )
    .unwrap();
    drop(state);
    assert!(identity.get("project").is_none());
    assert!(identity.get("cwd").is_none());
    assert_eq!(identity["workspace_revision"], 3);
    assert_eq!(identity["window_revision"], 2);
    assert_eq!(identity["pane_revision"], 5);
    assert!(detailed.get("project").is_some());
    assert_eq!(first["agents"].as_array().unwrap().len(), 10);
    assert_eq!(first["has_more"], true);
    assert_eq!(second["agents"].as_array().unwrap().len(), 3);
    assert_eq!(second["has_more"], false);
    assert_eq!(exact["agents"].as_array().unwrap().len(), 1);
    assert_eq!(exact["agents"][0]["launch_id"], "peer-20");
    assert!(foreign["agents"].as_array().unwrap().is_empty());
    assert!(mismatch["agents"].as_array().unwrap().is_empty());
}

#[test]
fn retired_recipient_successor_hint_is_scoped_read_only_and_never_replays() {
    let (_dir, service, mut contexts) = fixture();
    let old = send(
        &service,
        &contexts,
        "old-send",
        "retained body",
        AgentScope::Workspace,
    )
    .unwrap();
    service.retire(&["b".into()], now()).unwrap();
    contexts.remove("b");
    let mut successor = launch("successor");
    successor.grant.pane_id = "pane-b".into();
    service.register(&successor).unwrap();
    let mut successor_context = context("successor", Some("other"));
    successor_context.pane.id = "pane-b".into();
    contexts.insert("successor".into(), successor_context);
    let private = send(
        &service,
        &contexts,
        "attempt",
        "new body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(!private.to_string().contains("launch_id=successor"));
    contexts.get_mut("successor").unwrap().pane.workspace_id = Some("one".into());
    let hint = send(
        &service,
        &contexts,
        "attempt",
        "new body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(hint.to_string().contains("launch_id=successor"));
    let messages = service.state.lock().unwrap().store.messages.clone();
    assert_eq!(messages.len(), 1);
    let retained = &messages[old["message_id"].as_str().unwrap()];
    assert_eq!(retained.to, "b");
    assert_eq!(retained.text.as_ref().unwrap().as_str(), "retained body");
}

#[test]
fn retired_bookkeeping_prunes_only_after_existing_retention_without_any_references() {
    let (_dir, service, contexts) = fixture();
    let sent = send(
        &service,
        &contexts,
        "keep",
        "retained message",
        AgentScope::Workspace,
    )
    .unwrap();
    service.register(&launch("unreferenced")).unwrap();
    service
        .transaction(|store| {
            let old = now().saturating_sub(RETENTION_MS + 1);
            store.agents.get_mut("unreferenced").unwrap().exited_at = Some(old);
            store.agents.get_mut("b").unwrap().exited_at = Some(old);
            Ok(())
        })
        .unwrap();
    let state = service.state.lock().unwrap();
    let unreferenced_exists = state.store.agents.contains_key("unreferenced");
    let retained_recipient_exists = state.store.agents.contains_key("b");
    let retained_message_exists = state
        .store
        .messages
        .contains_key(sent["message_id"].as_str().unwrap());
    drop(state);
    assert!(!unreferenced_exists);
    assert!(retained_recipient_exists);
    assert!(retained_message_exists);
}

#[test]
fn control_escaped_inbox_bodies_stay_bounded_with_explicit_complete_recovery() {
    let (_dir, service, contexts) = fixture();
    let body = (0..32u8).map(char::from).collect::<String>().repeat(2048);
    for index in 0..10 {
        send(
            &service,
            &contexts,
            &format!("escaped-{index}"),
            &body,
            AgentScope::Workspace,
        )
        .unwrap();
    }
    let page = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, 10))
        .unwrap();
    assert!(serde_json::to_vec(&page).unwrap().len() < 8192);
    let first = &page["messages"][0];
    assert_eq!(first["body_bytes"], 65536);
    assert_eq!(first["body_complete"], false);
    let snippet = first["text"].as_str().unwrap();
    let mut offset = first["next_offset"].as_u64().unwrap() as usize;
    assert_eq!(offset, snippet.len());
    let message_id = first["id"].as_str().unwrap();
    let mut reconstructed = snippet.to_owned();
    let mut explicit_limit = false;
    loop {
        let max_bytes = explicit_limit.then_some(4096);
        let part = service
            .transaction_maybe(|store| {
                messages::read_message(store, "b", message_id, Some(offset), max_bytes, 20)
            })
            .unwrap();
        assert_eq!(part["offset"], offset);
        let text = part["text"].as_str().unwrap();
        assert!(text.len() <= max_bytes.unwrap_or(2048));
        assert!(serde_json::to_vec(text).unwrap().len() <= 4096);
        assert!(serde_json::to_vec(&part).unwrap().len() < 8192);
        reconstructed.push_str(text);
        if part["has_more"] == false {
            assert!(part.get("next_offset").is_none());
            break;
        }
        let next_offset = part["next_offset"].as_u64().unwrap() as usize;
        assert_eq!(next_offset, offset + text.len());
        assert!(next_offset > offset);
        offset = next_offset;
        explicit_limit = !explicit_limit;
    }
    assert_eq!(reconstructed, body);
}

#[test]
fn default_status_is_elementary_bounded_and_detail_recovers_full_osc_fields() {
    let (_dir, service, mut contexts) = fixture();
    let status = bkpr_protocol::ProgramStatus {
        state: "\u{01}".repeat(4096),
        kind: Some("task".into()),
        app: Some("🦀".repeat(4096)),
        message: Some("private OSC message".repeat(4096)),
        id: "large-status-id".repeat(4096),
        title: "private OSC title".repeat(4096),
        progress: Some(42),
    };
    let own = contexts.get_mut("a").unwrap();
    own.status = status_summary(Some(&status));
    own.pane.harness_status = Some(status.clone());
    let state = service.state.lock().unwrap();
    let compact = public(own, &state.store.agents["a"], None, 0, false);
    let detailed = public(own, &state.store.agents["a"], None, 0, true);
    drop(state);
    assert!(serde_json::to_vec(&compact["status"]).unwrap().len() < 256);
    assert_eq!(compact["status"]["progress"], 42);
    assert_eq!(
        compact["status"]["truncated_fields"],
        json!(["state", "app", "id"])
    );
    assert!(compact["status"].get("message").is_none());
    assert!(compact["status"].get("title").is_none());
    assert_eq!(detailed["status"], serde_json::to_value(status).unwrap());
    assert_eq!(compact["conversation"]["id"], "native-a");
    assert_eq!(compact["conversation"]["generation"], 1);
    assert_eq!(compact["attached"], true);
}
