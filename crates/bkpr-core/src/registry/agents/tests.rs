use super::*;
use messages::SendInput;

fn launch(id: &str) -> Launch {
    let process = proc::process(std::process::id() as i32).unwrap().unwrap();
    Launch {
        control: Default::default(),
        binding_generation: 1,
        binding_guard: None,
        binding_context_admitted: false,
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
        binding_revision: 1,
        cwd: Some(PathBuf::from("/")),
        repository: Some(PathBuf::from("/repository.git")),
        project_summary: None,
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
    send_to(agents, contexts, "b", key, text, scope)
}

fn send_to(
    agents: &Agents,
    contexts: &BTreeMap<String, ContextFact>,
    to: &str,
    key: &str,
    text: &str,
    scope: AgentScope,
) -> Result<Value> {
    send_at(agents, contexts, to, key, text, scope, now())
}

fn send_at(
    agents: &Agents,
    contexts: &BTreeMap<String, ContextFact>,
    to: &str,
    key: &str,
    text: &str,
    scope: AgentScope,
    timestamp: u64,
) -> Result<Value> {
    agents.transaction(|store| {
        messages::send(
            store,
            &contexts["a"],
            contexts,
            SendInput {
                to: to.into(),
                text: text.into(),
                idempotency_key: key.into(),
                scope,
                timestamp,
            },
            &"a".repeat(64),
            |_, _| Ok(()),
            |_| false,
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
            &contexts["a"],
            &contexts,
            SendInput {
                to: "a".into(),
                text: "hello".into(),
                idempotency_key: "same".into(),
                scope: AgentScope::Workspace,
                timestamp: now(),
            },
            &"a".repeat(64),
            |_, _| Ok(()),
            |_| false,
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
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: "private peer body".into(),
                    idempotency_key: "push".into(),
                    scope: AgentScope::Workspace,
                    timestamp: now(),
                },
                &"a".repeat(64),
                |_, _| Ok(()),
                |_| true,
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
                expected_revision: None,
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
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: key.into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    timestamp: now(),
                },
                &"a".repeat(64),
                |_, _| Ok(()),
                |_| true,
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
                expected_revision: None,
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
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: key.into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    timestamp: now(),
                },
                &"a".repeat(64),
                |_, _| Ok(()),
                |_| true,
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
fn obsolete_stored_sender_choice_is_discarded_without_replaying_old_inboxes() {
    let (dir, service, contexts) = fixture();
    let sent = send(
        &service,
        &contexts,
        "historical",
        "body",
        AgentScope::Workspace,
    )
    .unwrap();
    let id = sent["message_id"].as_str().unwrap();
    let path = dir.path().join("agents/state.json");
    let mut old: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old["messages"][id]["delivery"] = json!("quiet");
    old["messages"][id]["wake_state"] = json!("quiet");
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    drop(service);
    let restarted = Agents::open(dir.path()).unwrap();
    let state = restarted.state.lock().unwrap();
    assert!(state.receivers.is_empty());
    assert!(
        state
            .store
            .agents
            .values()
            .all(|agent| agent.exited_at.is_some() && agent.notification.is_none())
    );
    assert_eq!(state.store.messages[id].wake_state, "quiet");
    assert!(state.store.messages[id].notification_id.is_none());
    assert!(Agents::require_live(&state.store, "b").is_err());
    let rewritten: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(rewritten["messages"][id].get("delivery").is_none());
    assert!(
        summary(&state.store.messages[id])
            .get("delivery_mode")
            .is_none()
    );
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
fn every_direct_message_requests_a_wake_and_joins_the_current_claim() {
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
    let send_to_a = |key: &str| {
        registry.inner.agents.transaction(|store| {
            messages::send(
                store,
                &contexts["b"],
                &contexts,
                SendInput {
                    to: "a".into(),
                    text: "body-free wake test body".into(),
                    idempotency_key: key.into(),
                    scope: AgentScope::Workspace,
                    timestamp: now(),
                },
                &"a".repeat(64),
                |_, _| Ok(()),
                |_| false,
            )
        })
    };
    let first = send_to_a("first").unwrap();
    assert_eq!(first["wake"]["state"], "blocked");
    let duplicate = send_to_a("first").unwrap();
    assert_eq!(duplicate["message_id"], first["message_id"]);
    assert_eq!(duplicate["duplicate"], true);
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let pointer = registry
        .agent_subscribe(&launch, &peer, receiver_id.clone(), 0, &|| true)
        .unwrap();
    assert_eq!(pointer["notification"]["message_count"], 1);
    assert_eq!(pointer["notification"]["unread_count"], 1);
    assert!(!pointer.to_string().contains("body-free wake test body"));
    let later = send_to_a("later").unwrap();
    assert_eq!(later["wake"]["state"], "pending");
    let saved = registry
        .inner
        .agents
        .state
        .lock()
        .unwrap()
        .store
        .messages
        .clone();
    for receipt in [first, later] {
        let message = &saved[receipt["message_id"].as_str().unwrap()];
        assert_eq!(
            message.notification_id.as_deref(),
            pointer["notification"]["notification_id"].as_str()
        );
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
    // Leave maintenance behind; distinct admission itself must time out the old claim.
    let new = send_at(
        &service,
        &contexts,
        "b",
        "new",
        "distinct wake",
        AgentScope::Workspace,
        30_100,
    )
    .unwrap();
    assert_eq!(new["wake"]["state"], "blocked");
    assert_eq!(service.state.lock().unwrap().maintenance_at, None);
    let old_id = old["message_id"].as_str().unwrap();
    let old_outcome = {
        let state = service.state.lock().unwrap();
        summary(&state.store.messages[old_id])
    };
    assert_eq!(old_outcome["delivery"]["state"], "unknown");
    assert_eq!(
        old_outcome["delivery"]["reason"],
        "native notification acceptance timed out"
    );
    let new_claim = claim(&service, 30300);
    assert_ne!(new_claim.id, old_claim.id);
    assert!(new_claim.generation > old_claim.generation);
    assert_eq!(new_claim.id, new["message_id"]);
    assert_eq!(new_claim.message_count, 1);
    let unchanged = {
        let state = service.state.lock().unwrap();
        summary(&state.store.messages[old_id])
    };
    assert_eq!(unchanged, old_outcome);
}

#[test]
fn terminal_batch_slots_expire_without_rewriting_or_reoffering_receipts() {
    for (outcome, label, run_maintenance) in [
        (
            AgentWakeOutcome::Unknown {
                reason: "native result ambiguous".into(),
            },
            "unknown",
            true,
        ),
        (
            AgentWakeOutcome::Unknown {
                reason: "native result ambiguous".into(),
            },
            "unknown",
            false,
        ),
        (AgentWakeOutcome::Accepted, "accepted", false),
        (
            AgentWakeOutcome::Failed {
                reason: "native request rejected".into(),
            },
            "failed",
            false,
        ),
    ] {
        let (dir, service, contexts) = fixture();
        let send = |key, timestamp| {
            send_at(
                &service,
                &contexts,
                "b",
                key,
                key,
                AgentScope::Workspace,
                timestamp,
            )
            .unwrap()
        };
        let old = send("old", 50);
        let old_id = old["message_id"].as_str().unwrap();
        let batch = claim(&service, 100);
        service
            .transaction(|store| {
                messages::outcome(
                    store,
                    "b",
                    "receiver",
                    &batch.id,
                    batch.generation,
                    outcome,
                    &"a".repeat(64),
                )
            })
            .unwrap();
        let joined = send("joined", 30_099);
        assert_eq!(joined["wake"]["state"], label);
        let historical = {
            let state = service.state.lock().unwrap();
            assert_eq!(state.maintenance_at, Some(30_100));
            summary(&state.store.messages[old_id])
        };
        assert_eq!(historical["delivery"]["state"], label);
        service.maintain(30_099).unwrap();
        let retry = send("old", 30_100);
        assert_eq!(retry["duplicate"], true);
        assert_eq!(retry["message_id"], old_id);
        assert_eq!(retry["wake"]["state"], label);
        assert_eq!(service.state.lock().unwrap().maintenance_at, Some(30_100));

        if run_maintenance {
            // The timer can release a reported unknown without touching its history.
            service.maintain(30_100).unwrap();
            assert_eq!(service.state.lock().unwrap().maintenance_at, None);
            assert!(
                service
                    .transaction(|store| messages::claim(store, "b", "receiver", 30_100))
                    .unwrap()
                    .is_none()
            );
        }
        // Otherwise, deliberately leave maintenance behind the admission deadline.
        let fresh = send("fresh", 30_100);
        assert_eq!(fresh["duplicate"], false);
        assert_eq!(fresh["wake"]["state"], "blocked");
        assert_eq!(service.state.lock().unwrap().maintenance_at, None);
        let next = claim(&service, 30_101);
        assert_eq!(next.id, fresh["message_id"]);
        assert_eq!(next.generation, batch.generation + 1);
        assert_eq!(next.message_count, 1);
        let persisted = Agents::load(&dir.path().join("agents")).unwrap();
        assert_eq!(summary(&persisted.messages[old_id]), historical);
        let joined = &persisted.messages[joined["message_id"].as_str().unwrap()];
        assert_eq!(joined.notification_id.as_deref(), Some(batch.id.as_str()));
        assert_eq!(joined.generation, Some(batch.generation));
        assert_eq!(joined.wake_state, label);
        if label == "unknown" {
            service
                .transaction(|store| {
                    messages::outcome(
                        store,
                        "b",
                        "receiver",
                        &batch.id,
                        batch.generation,
                        AgentWakeOutcome::Accepted,
                        &"a".repeat(64),
                    )
                })
                .unwrap();
            let state = service.state.lock().unwrap();
            assert_eq!(state.store.messages[old_id].wake_state, "accepted");
            assert_eq!(state.store.messages[&next.id].wake_state, "pending");
            assert_eq!(
                state.store.agents["b"]
                    .notification
                    .as_ref()
                    .unwrap()
                    .generation,
                next.generation
            );
            assert_eq!(state.maintenance_at, Some(60_101));
        }
    }
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

fn discovery_test_project() -> ProjectSnapshot {
    ProjectSnapshot {
        cwd: PathBuf::from("/repo"),
        state: ProjectState::Ready,
        repository: Some(RepositoryInfo {
            name: "bkpr".into(),
            root: PathBuf::from("/repo"),
            common_dir: PathBuf::from("/repo/.git"),
            git_dir: PathBuf::from("/repo/.git"),
            branch: Some("feature/concise-discovery".into()),
            head_sha: Some("a".repeat(40)),
            staged: ChangeSummary::default(),
            unstaged: ChangeSummary {
                files: 42,
                insertions: 1000,
                ..Default::default()
            },
            untracked_files: 12,
            base_ref: Some("origin/main".into()),
            base_sha: Some("b".repeat(40)),
            committed: None,
        }),
        ci: None,
        error: None,
    }
}

#[test]
fn discovery_keeps_actionable_context_without_repository_or_status_internals() {
    let (_directory, service, mut contexts) = fixture();
    let project = discovery_test_project();
    let status = ProgramStatus {
        state: "working".into(),
        kind: None,
        app: Some("omp".into()),
        id: "nested/task".into(),
        message: Some("status detail".into()),
        title: "terminal detail".into(),
        progress: Some(42),
    };
    let peer = contexts.get_mut("b").unwrap();
    peer.cwd = Some(project.cwd.clone());
    peer.project_summary = discovery_project(Some(&project), peer.cwd.as_deref(), true);
    peer.pane.project = Some(project);
    peer.status = status_summary(Some(&status));
    peer.pane.harness_status = Some(status);
    let mut state = service.state.lock().unwrap();
    state.store.agents.get_mut("b").unwrap().title = Some("Frontend".into());
    let page = |detail| {
        discover(
            &state,
            &contexts,
            &contexts["a"],
            DiscoveryQuery {
                scope: AgentScope::Workspace,
                detail,
                launch_id: Some("b"),
                pane_id: None,
                after: None,
                limit: None,
            },
        )
        .unwrap()
    };
    let compact = page(false);
    let expanded = page(true);
    let peer = &compact["agents"][0];
    assert_eq!(peer["launch_id"], "b");
    assert_eq!(peer["title"], "Frontend");
    assert_eq!(peer["cwd"], "/repo");
    assert_eq!(peer["status"], json!({"state":"working"}));
    assert_eq!(peer["project_name"], "bkpr");
    assert_eq!(peer["branch"], "feature/concise-discovery");
    assert!(peer.get("project").is_none());
    for field in [
        "workspace_id",
        "window_id",
        "status_changed_at_ms",
        "first_prompt_submitted_at_ms",
        "conversation",
        "registry_revision",
    ] {
        assert!(
            peer.get(field).is_none(),
            "{field} belongs to explicit detail"
        );
    }
    assert_eq!(
        expanded["agents"][0]["project"]["repository"]["unstaged"]["files"],
        42
    );
    assert_eq!(expanded["agents"][0]["status"]["message"], "status detail");
    assert_eq!(expanded["agents"][0]["workspace_id"], "one");
}

#[test]
fn discovery_project_context_requires_matching_live_foreground_cache() {
    let mut project = discovery_test_project();
    assert!(discovery_project(Some(&project), Some(Path::new("/elsewhere")), true).is_none());
    assert!(discovery_project(Some(&project), Some(Path::new("/repo")), false).is_none());
    project.state = ProjectState::Loading;
    assert!(discovery_project(Some(&project), Some(Path::new("/repo")), true).is_none());
}

#[test]
fn conversation_selector_persists_exact_launch_and_original_fingerprint() {
    let (dir, service, contexts) = fixture();
    let sent = service
        .transaction(|store| {
            messages::send(
                store,
                &contexts["a"],
                &contexts,
                SendInput {
                    to: "native-b".into(),
                    text: "conversation body".into(),
                    idempotency_key: "conversation".into(),
                    scope: AgentScope::Workspace,
                    timestamp: now(),
                },
                &"a".repeat(64),
                |_, selector| {
                    assert_eq!(selector, "native-b");
                    Ok(())
                },
                |recipient| {
                    assert_eq!(recipient, "b");
                    true
                },
            )
        })
        .unwrap();
    assert_eq!(sent["wake"]["state"], "pending");
    let saved = Agents::load(&dir.path().join("agents")).unwrap();
    let message = &saved.messages[sent["message_id"].as_str().unwrap()];
    assert_eq!(message.to, "b");
    assert_eq!(
        message.digest,
        payload("native-b", "conversation body", AgentScope::Workspace)
    );
    let claimed = claim(&service, now());
    assert_eq!(claimed.message_count, 1);
    let inbox = service
        .transaction_maybe(|store| messages::read(store, "b", None, None, &[], false, now()))
        .unwrap();
    assert_eq!(inbox["messages"][0]["id"], sent["message_id"]);
    assert_eq!(inbox["messages"][0]["text"], "conversation body");
}

#[test]
fn conversation_selector_requires_foreground_bound_live_recipient_and_rejects_self() {
    let (_dir, service, mut contexts) = fixture();
    for selector in ["missing", "native-a"] {
        assert!(
            send_to(
                &service,
                &contexts,
                selector,
                selector,
                "body",
                AgentScope::All,
            )
            .is_err()
        );
    }
    contexts.get_mut("b").unwrap().active = false;
    assert!(
        send_to(
            &service,
            &contexts,
            "native-b",
            "background-alias",
            "body",
            AgentScope::All,
        )
        .is_err()
    );
    assert!(
        send(
            &service,
            &contexts,
            "background-exact",
            "body",
            AgentScope::All
        )
        .is_ok()
    );
    contexts.get_mut("b").unwrap().active = true;
    contexts.get_mut("b").unwrap().conversation = None;
    assert!(
        send_to(
            &service,
            &contexts,
            "native-b",
            "unbound",
            "body",
            AgentScope::All,
        )
        .is_err()
    );
    contexts.get_mut("b").unwrap().conversation = Some(NativeConversation {
        id: "native-b".into(),
        source: ConversationSource::OmpExtension,
        generation: 0,
    });
    assert!(
        send_to(
            &service,
            &contexts,
            "native-b",
            "unverified",
            "body",
            AgentScope::All,
        )
        .is_err()
    );
}

fn ambiguity(error: anyhow::Error) -> Value {
    let error = error.to_string();
    let encoded = error
        .strip_prefix("ambiguous recipient; ")
        .unwrap()
        .split_once("; address an exact launch_id;")
        .unwrap()
        .0;
    serde_json::from_str(encoded).unwrap()
}

#[test]
fn duplicate_conversations_are_scoped_across_harnesses_before_ambiguity() {
    let (_dir, service, mut contexts) = fixture();
    let mut third = launch("c");
    third.grant.kind = HarnessKind::Claude;
    service.register(&third).unwrap();
    let mut duplicate = context("c", Some("other"));
    duplicate.conversation = Some(NativeConversation {
        id: "native-b".into(),
        source: ConversationSource::ClaudeHook,
        generation: 3,
    });
    contexts.insert("c".into(), duplicate);
    let sent = send_to(
        &service,
        &contexts,
        "native-b",
        "scoped",
        "body",
        AgentScope::Workspace,
    )
    .unwrap();
    assert_eq!(
        service.state.lock().unwrap().store.messages[sent["message_id"].as_str().unwrap()].to,
        "b"
    );
    let error = send_to(
        &service,
        &contexts,
        "native-b",
        "session-ambiguous",
        "body",
        AgentScope::Session,
    )
    .unwrap_err();
    assert_eq!(
        ambiguity(error),
        json!({"candidates":[{"launch_id":"b","pane_id":"pane-b"},{"launch_id":"c","pane_id":"pane-c"}],"has_more":false,"truncated":false})
    );
    contexts.get_mut("c").unwrap().pane.workspace_id = Some("one".into());
    assert_eq!(
        ambiguity(
            send_to(
                &service,
                &contexts,
                "native-b",
                "workspace-ambiguous",
                "body",
                AgentScope::Workspace,
            )
            .unwrap_err()
        )["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn ambiguity_is_bounded_without_foreign_scope_metadata_or_counts() {
    let (_dir, service, mut contexts) = fixture();
    for index in 0..15 {
        let id = format!("candidate-{index:02}");
        service.register(&launch(&id)).unwrap();
        let mut candidate = context(&id, Some("one"));
        candidate.conversation.as_mut().unwrap().id = "native-b".into();
        candidate.pane.title = "private-rich-title".into();
        contexts.insert(id, candidate);
    }
    service.register(&launch("private-foreign")).unwrap();
    let mut foreign = context("private-foreign", Some("other"));
    foreign.conversation.as_mut().unwrap().id = "native-b".into();
    contexts.insert("private-foreign".into(), foreign);
    let encoded = ambiguity(
        send_to(
            &service,
            &contexts,
            "native-b",
            "bounded",
            "body",
            AgentScope::Workspace,
        )
        .unwrap_err(),
    );
    assert_eq!(encoded["candidates"].as_array().unwrap().len(), 10);
    assert_eq!(encoded["has_more"], true);
    assert_eq!(encoded["truncated"], true);
    for candidate in encoded["candidates"].as_array().unwrap() {
        assert_eq!(candidate.as_object().unwrap().len(), 2);
    }
    assert!(!encoded.to_string().contains("private"));
    assert!(service.state.lock().unwrap().store.messages.is_empty());
}

#[test]
fn launch_conversation_namespace_collision_fails_closed_and_retirement_never_forwards() {
    let (_dir, service, mut contexts) = fixture();
    contexts
        .get_mut("b")
        .unwrap()
        .conversation
        .as_mut()
        .unwrap()
        .id = "b".into();
    assert!(
        send(
            &service,
            &contexts,
            "same-launch",
            "body",
            AgentScope::Workspace
        )
        .is_ok()
    );
    service.register(&launch("c")).unwrap();
    let mut collision = context("c", Some("one"));
    collision.conversation.as_mut().unwrap().id = "b".into();
    contexts.insert("c".into(), collision);
    assert_eq!(
        ambiguity(
            send(
                &service,
                &contexts,
                "collision",
                "body",
                AgentScope::Workspace
            )
            .unwrap_err()
        )["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    contexts.get_mut("c").unwrap().pane.workspace_id = Some("other".into());
    assert!(
        send(
            &service,
            &contexts,
            "scoped-exact",
            "body",
            AgentScope::Workspace
        )
        .is_ok()
    );
    contexts.get_mut("b").unwrap().pane.workspace_id = Some("other".into());
    contexts.get_mut("c").unwrap().pane.workspace_id = Some("one".into());
    let outside = send(
        &service,
        &contexts,
        "outside",
        "body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(outside.to_string().contains("outside selected live scope"));
    service.retire(&["b".into()], now()).unwrap();
    contexts.remove("b");
    let retired = send(
        &service,
        &contexts,
        "retired",
        "body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(retired.to_string().contains("recipient launch is retired"));
    assert_eq!(service.state.lock().unwrap().store.messages.len(), 2);
}

#[test]
fn conversation_retry_survives_rebinding_replacement_ambiguity_and_body_expiry() {
    let (dir, service, mut contexts) = fixture();
    let sent = send_to(
        &service,
        &contexts,
        "native-b",
        "stable",
        "original body",
        AgentScope::Workspace,
    )
    .unwrap();
    assert!(
        send_to(
            &service,
            &contexts,
            "b",
            "stable",
            "original body",
            AgentScope::Workspace,
        )
        .unwrap_err()
        .to_string()
        .contains("idempotency key conflicts")
    );
    contexts
        .get_mut("b")
        .unwrap()
        .conversation
        .as_mut()
        .unwrap()
        .id = "replacement".into();
    for id in ["replacement", "competitor"] {
        service.register(&launch(id)).unwrap();
        let mut candidate = context(id, Some("one"));
        candidate.conversation.as_mut().unwrap().id = "native-b".into();
        contexts.insert(id.into(), candidate);
    }
    let exited_at = now();
    service.retire(&["b".into()], exited_at).unwrap();
    service.maintain(exited_at + RETENTION_MS).unwrap();
    let retried = send_to(
        &service,
        &contexts,
        "native-b",
        "stable",
        "original body",
        AgentScope::Workspace,
    )
    .unwrap();
    assert_eq!(retried["message_id"], sent["message_id"]);
    assert_eq!(retried["duplicate"], true);
    for (text, scope) in [
        ("changed body", AgentScope::Workspace),
        ("original body", AgentScope::All),
    ] {
        assert!(
            send_to(&service, &contexts, "native-b", "stable", text, scope)
                .unwrap_err()
                .to_string()
                .contains("idempotency key conflicts")
        );
    }
    let persisted = Agents::load(&dir.path().join("agents")).unwrap();
    assert_eq!(persisted.messages.len(), 1);
    let message = &persisted.messages[sent["message_id"].as_str().unwrap()];
    assert_eq!(message.to, "b");
    assert!(message.text.is_none());
    assert_eq!(message.expired_at, Some(exited_at + RETENTION_MS));
    assert_eq!(
        message.digest,
        payload("native-b", "original body", AgentScope::Workspace)
    );
}

#[test]
fn conversation_commit_uncertainty_never_returns_durable_success_or_redirects() {
    let (dir, service, contexts) = fixture();
    service.fail_next_commit();
    let failure = send_to(
        &service,
        &contexts,
        "native-b",
        "uncertain-alias",
        "body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert!(failure.to_string().contains("outcome unknown"));
    let persisted = Agents::load(&dir.path().join("agents")).unwrap();
    let message = persisted.messages.values().next().unwrap();
    assert_eq!(message.to, "b");
    assert_eq!(
        message.digest,
        payload("native-b", "body", AgentScope::Workspace)
    );
    assert!(
        send_to(
            &service,
            &contexts,
            "native-b",
            "uncertain-alias",
            "body",
            AgentScope::Workspace,
        )
        .is_err()
    );
    assert!(service.state.lock().unwrap().store.messages.is_empty());
}

#[test]
fn self_conversation_and_launch_namespace_collisions_never_choose_a_peer() {
    let (_dir, service, mut contexts) = fixture();
    contexts
        .get_mut("b")
        .unwrap()
        .conversation
        .as_mut()
        .unwrap()
        .id = "a".into();
    let error = send_to(
        &service,
        &contexts,
        "a",
        "self-collision",
        "body",
        AgentScope::Workspace,
    )
    .unwrap_err();
    assert_eq!(ambiguity(error)["candidates"].as_array().unwrap().len(), 2);
    contexts.get_mut("b").unwrap().active = false;
    let error = send_to(
        &service,
        &contexts,
        "a",
        "self-only",
        "body",
        AgentScope::All,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "cannot message self");
    assert!(service.state.lock().unwrap().store.messages.is_empty());
}
