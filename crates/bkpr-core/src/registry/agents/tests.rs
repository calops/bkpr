use super::*;

fn launch(id: &str) -> Launch {
    let process = proc::process(std::process::id() as i32).unwrap().unwrap();
    Launch {
        control: Default::default(),
        binding_generation: 1,
        channel_participant: None,
        channel_lifetime: None,
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

fn fixture() -> (Agents, BTreeMap<String, ContextFact>) {
    let agents = Agents::new();
    agents.register(&launch("a")).unwrap();
    agents.register(&launch("b")).unwrap();
    let contexts = [
        ("a".into(), context("a", Some("one"))),
        ("b".into(), context("b", Some("one"))),
    ]
    .into_iter()
    .collect();
    (agents, contexts)
}
#[test]
fn window_membership_is_reported_without_narrowing_workspace_agent_scope() {
    let (service, mut contexts) = fixture();
    contexts.get_mut("b").unwrap().pane.window_id = Some("independent-window".into());
    let state = service.state.lock().unwrap();
    let page = discover(
        &state,
        &contexts,
        &contexts["a"],
        DiscoveryQuery {
            scope: AgentScope::Workspace,
            detail: true,
            launch_id: None,
            pane_id: None,
            after: None,
            limit: None,
        },
    )
    .unwrap();
    let payload = &page["agents"][0];
    assert_eq!(payload["window_id"], "independent-window");
    assert_eq!(payload["workspace_id"], "one");
}

#[test]
fn discovery_scopes_use_live_execution_membership_and_fail_closed_without_context() {
    let (service, mut contexts) = fixture();
    let state = service.state.lock().unwrap();
    let size = |contexts: &BTreeMap<String, ContextFact>, scope| {
        discover(
            &state,
            contexts,
            &contexts["a"],
            DiscoveryQuery {
                scope,
                detail: false,
                launch_id: None,
                pane_id: None,
                after: None,
                limit: None,
            },
        )
        .map(|page| page["agents"].as_array().unwrap().len())
    };
    let recipient = contexts.get_mut("b").unwrap();
    recipient.pane.workspace_id = Some("other".into());
    assert_eq!(size(&contexts, AgentScope::Workspace).unwrap(), 0);
    assert_eq!(size(&contexts, AgentScope::Session).unwrap(), 1);
    assert_eq!(size(&contexts, AgentScope::Repository).unwrap(), 1);
    contexts.get_mut("b").unwrap().active = false;
    assert_eq!(size(&contexts, AgentScope::All).unwrap(), 0);
    let own = contexts.get_mut("a").unwrap();
    own.repository = None;
    own.pane.workspace_id = None;
    assert!(size(&contexts, AgentScope::Repository).is_err());
    assert!(size(&contexts, AgentScope::Workspace).is_err());
}
#[test]
fn stale_receiver_birth_and_conversation_generation_reject_callbacks() {
    let (_dir, registry, launch, receiver_id) = receiver_fixture();
    let peer = Peer {
        pid: std::process::id() as i32,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    let receiver_id = &receiver_id;
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
            &state.agents["a"],
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
            &state.agents["a"],
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
fn compact_identity_and_scoped_exact_discovery_are_bounded_and_pageable() {
    let (service, mut contexts) = fixture();
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
    let identity = public(own, &state.agents["a"], None, 0, false);
    let detailed = public(own, &state.agents["a"], None, 0, true);
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
fn default_status_is_elementary_bounded_and_detail_recovers_full_osc_fields() {
    let (service, mut contexts) = fixture();
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
    let compact = public(own, &state.agents["a"], None, 0, false);
    let detailed = public(own, &state.agents["a"], None, 0, true);
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
    let (service, mut contexts) = fixture();
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
    state.agents.get_mut("b").unwrap().title = Some("Frontend".into());
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
