use super::*;
use messages::SendInput;

fn launch(id: &str) -> Launch {
    let process = proc::process(std::process::id() as i32).unwrap().unwrap();
    Launch {
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
            title: id.into(),
            status: PaneStatus::Running,
            exit_code: None,
            cwd: PathBuf::from("/"),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            home: PathBuf::from("/"),
            isolated_environment: false,
        },
        workspace_name: workspace.map(str::to_owned),
        session_id: Some(format!("native-{id}")),
        cwd: Some(PathBuf::from("/")),
        repository: Some(PathBuf::from("/repository.git")),
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
            },
            &"a".repeat(64),
        )
    });
    assert!(changed_to.is_err());
    assert_eq!(service.state.lock().unwrap().store.messages.len(), 1);
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
        .transaction_maybe(|store| messages::read(store, "b", None, Some(1), 200))
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
        .transaction_maybe(|store| messages::read(store, "b", Some(cursor), Some(1), 201))
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
    assert_eq!(retry["status"]["acknowledged_at"], 300);
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
    };
    assert!(
        messages::unchanged(&service.state.lock().unwrap().store, "b", &request)
            .unwrap()
            .is_some()
    );
    let state_path = service.root.join("state.json");
    let initial_inode = fs::metadata(&state_path).unwrap().ino();
    service
        .transaction_maybe(|store| messages::read(store, "b", None, Some(1), 100))
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
            .unwrap()
            .values()
            .copied()
            .sum::<usize>(),
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
            .unwrap()
            .values()
            .copied()
            .sum::<usize>(),
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
                },
                &"a".repeat(64),
            )
        })
        .unwrap();
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
fn repository_identity_follows_live_directory_and_unifies_linked_worktrees() {
    fn git(cwd: &Path, args: &[&std::ffi::OsStr]) {
        let mut command = std::process::Command::new("git");
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        let status = command
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "-c",
                "init.templateDir=",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    let linked = directory.path().join("linked");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    git(&first, &["init".as_ref()]);
    git(
        &first,
        &[
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-m".as_ref(),
            "fixture".as_ref(),
        ],
    );
    git(
        &first,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "--detach".as_ref(),
            linked.as_os_str(),
        ],
    );
    git(&second, &["init".as_ref()]);
    assert_eq!(repository_identity(&first), repository_identity(&linked));
    assert_ne!(repository_identity(&first), repository_identity(&second));
    assert!(repository_identity(directory.path()).is_none());
}

#[test]
fn historical_pending_receipt_settles_after_read_without_touching_new_generation() {
    let (_dir, service, contexts) = fixture();
    let first = send(&service, &contexts, "one", "first", AgentScope::Workspace).unwrap();
    let n1 = claim(&service, 100);
    service
        .transaction_maybe(|store| messages::read(store, "b", None, None, 200))
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
        );
        assert_eq!(identity["notification_ready"], false);
        assert_eq!(identity["readiness"]["can_notify"], false);
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
