use super::*;
use bkpr_protocol::channels::{
    ChannelId, ChannelPost, ChannelRequest, ChannelResponse, MembershipMode, ParticipantId,
    PostIntent,
};
use bkpr_protocol::{AgentReceiverMode, AgentWakeOutcome};

const NATIVE_ID: &str = "owned-channel-conversation";

#[test]
#[ignore = "owned PTY fixture; requires its private HOME marker"]
fn channel_peer_fixture() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let native = fs::read_to_string(home.join("fixture-conversation")).unwrap();
    fs::write(home.join("channel-stage"), "fixture entered").unwrap();
    let socket = PathBuf::from(std::env::var_os("BKPR_SOCKET").unwrap());
    let response = wire(
        &socket,
        Operation::HarnessBegin {
            kind: HarnessKind::Omp,
        },
    );
    fs::write(
        home.join("channel-stage"),
        format!("begin: {}", response["error"]),
    )
    .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    let grant: CaptureGrant = serde_json::from_value(response["result"].clone()).unwrap();
    let _native = ChildGuard(
        Command::new(home.join("native/omp"))
            .args([
                "-c",
                "printf ready > \"$HOME/native-ready\"; IFS= read -r hold",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    await_file(&home.join("native-ready"));
    let response = wire(
        &socket,
        Operation::HarnessEvent {
            launch_id: grant.launch_id.clone(),
            token: grant.token,
            event: CaptureEvent::Bind {
                conversation_id: native,
                source: ConversationSource::OmpExtension,
                expected_revision: None,
            },
        },
    );
    fs::write(
        home.join("channel-stage"),
        format!("bind: {}", response["error"]),
    )
    .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    fs::write(home.join("bound.tmp"), &grant.launch_id).unwrap();
    fs::rename(home.join("bound.tmp"), home.join("bound")).unwrap();
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn open(root: &Path) -> (Registry, ServerGuard) {
    let registry = Registry::open(&root.join("state")).unwrap();
    let socket = root.join("c.sock");
    let bound = crate::transport::bind(&socket).unwrap();
    registry
        .configure_harnesses(&socket, &std::env::current_exe().unwrap())
        .unwrap();
    let server = registry.clone();
    let worker = thread::spawn(move || bound.serve(server));
    (
        registry,
        ServerGuard {
            socket,
            worker: Some(worker),
        },
    )
}

fn launch_spec(root: &Path, name: &str, native: &str) -> PaneSpawnSpec {
    let home = root.join(name);
    crate::prepare_home(&home).unwrap();
    fs::write(home.join("fixture-conversation"), native).unwrap();
    let _ = fs::remove_file(home.join("bound"));
    let _ = fs::remove_file(home.join("channel-stage"));
    let _ = fs::remove_file(home.join("native-ready"));
    let executable = home.join("native/omp");
    if !executable.exists() {
        fs::create_dir(home.join("native")).unwrap();
        fs::copy("/bin/sh", executable).unwrap();
    }
    PaneSpawnSpec {
        argv: [
            root.join("omp").to_str().unwrap(),
            "--ignored",
            "--exact",
            "registry::capture::tests::channel_tests::channel_peer_fixture",
        ]
        .map(str::to_owned)
        .into(),
        cwd: home.clone(),
        home,
        isolated_environment: true,
        title: Some("Same label".into()),
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
    }
}

fn live(registry: &Registry, home: &Path) -> Launch {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !home.join("bound").exists() {
        assert!(
            Instant::now() < deadline,
            "owned channel fixture did not bind in {home:?}: {:?}",
            fs::read_to_string(home.join("channel-stage"))
        );
        thread::sleep(Duration::from_millis(5));
    }
    let id = fs::read_to_string(home.join("bound")).unwrap();
    registry.inner.captures.lock().unwrap().launches[&id].clone()
}

fn send(registry: &Registry, op: Operation) -> Value {
    registry
        .request(Request::new(op))
        .unwrap()
        .into_json()
        .unwrap()
}

fn launch_request(registry: &Registry, launch: &Launch, operation: Operation) -> Result<Payload> {
    let peer = Peer {
        pid: launch.root.pid,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    registry.request_authenticated(Request::new(operation), &peer)
}

fn agent_payload(registry: &Registry, launch: &Launch, request: AgentRequest) -> Result<Payload> {
    launch_request(
        registry,
        launch,
        Operation::Agent {
            launch_id: launch.grant.launch_id.clone(),
            token: launch.grant.token.clone(),
            guard: None,
            request,
        },
    )
}

fn agent_result(registry: &Registry, launch: &Launch, request: AgentRequest) -> Result<Value> {
    agent_payload(registry, launch, request)?.into_json()
}

fn agent(registry: &Registry, launch: &Launch, request: AgentRequest) -> Value {
    agent_result(registry, launch, request).unwrap()
}

fn agent_channel(registry: &Registry, launch: &Launch, request: ChannelRequest) -> Result<Value> {
    agent_result(
        registry,
        launch,
        AgentRequest::Channels {
            request: Box::new(request),
        },
    )
}

fn mention_and_launch(
    registry: &Registry,
    launch: &Launch,
    channel: &ChannelId,
    member: &ParticipantId,
) -> (ParticipantId, String) {
    let roster = agent_channel(
        registry,
        launch,
        ChannelRequest::ListMembers {
            channel_id: channel.clone(),
            after: None,
            limit: 10,
        },
    )
    .unwrap();
    assert!(
        roster["participants"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "user")
            .all(|entry| entry.get("launch_id").is_none())
    );
    let entry = roster["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == member.0)
        .unwrap();
    (
        ParticipantId(entry["mention_id"].as_str().unwrap().to_owned()),
        entry["launch_id"].as_str().unwrap().to_owned(),
    )
}

fn receiver(registry: &Registry, launch: &Launch) -> String {
    agent(
        registry,
        launch,
        AgentRequest::ReceiverRegister {
            session_id: launch.managed.conversation.as_ref().unwrap().id.clone(),
            mode: AgentReceiverMode::ActiveTurn,
            pid: launch.root.pid.try_into().unwrap(),
        },
    )["receiver_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn subscribe(registry: &Registry, launch: &Launch, receiver: &str) -> Value {
    agent(
        registry,
        launch,
        AgentRequest::ReceiverSubscribe {
            receiver_id: receiver.to_owned(),
            wait_ms: 0,
        },
    )["notification"]
        .take()
}

fn validate_wake(registry: &Registry, launch: &Launch, receiver: &str, wake: &Value) -> Value {
    agent(
        registry,
        launch,
        AgentRequest::WakeValidate {
            receiver_id: receiver.to_owned(),
            notification_id: wake["notification_id"].as_str().unwrap().to_owned(),
            generation: wake["generation"].as_u64().unwrap(),
        },
    )
}

fn settle_wake(registry: &Registry, launch: &Launch, receiver: &str, wake: &Value) -> Value {
    agent(
        registry,
        launch,
        AgentRequest::WakeOutcome {
            receiver_id: receiver.to_owned(),
            notification_id: wake["notification_id"].as_str().unwrap().to_owned(),
            generation: wake["generation"].as_u64().unwrap(),
            outcome: AgentWakeOutcome::Unknown {
                reason: "native dispatch unobservable".into(),
            },
        },
    )
}

fn channel(
    registry: &Registry,
    caller: Option<&Launch>,
    request: ChannelRequest,
) -> Result<ChannelResponse> {
    let Payload::Channels(response) =
        registry.request_channels_for(request, caller, &|| true, ChannelProjection::Native)?
    else {
        panic!("expected typed channel response");
    };
    Ok(response)
}

fn history(id: &ChannelId) -> ChannelRequest {
    ChannelRequest::History {
        channel_id: id.clone(),
        before: None,
        after: None,
        limit: 8,
    }
}

fn post(id: &ChannelId, text: &str, intent: PostIntent) -> ChannelRequest {
    ChannelRequest::Post {
        channel_id: id.clone(),
        post: ChannelPost {
            idempotency_key: text.into(),
            text: text.into(),
            intent,
            mentions: Vec::new(),
            reply_to: None,
            attachments: Vec::new(),
        },
    }
}

#[test]
fn verified_resume_inherits_channel_history_but_not_live_delivery() {
    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, server) = open(root);
    let a = launch_spec(root, "a", NATIVE_ID);
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(a.clone()),
        },
    );
    let old = live(&registry, &a.home);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let list = || ChannelRequest::List {
        workspace_id: workspace.id.clone(),
        after: None,
        limit: 8,
    };
    let ChannelResponse::Listed { channels, .. } = channel(&registry, None, list()).unwrap() else {
        panic!("expected workspace channels");
    };
    let desk = channels
        .iter()
        .find(|c| c.name == "#desk")
        .unwrap()
        .id
        .clone();
    let team = channels.into_iter().find(|c| c.name == "#team").unwrap().id;
    let b = launch_spec(root, "b", "other-fixture-conversation");
    send(
        &registry,
        Operation::WindowCreate {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
            name: None,
            spec: b.clone(),
        },
    );
    let other = live(&registry, &b.home);
    agent(
        &registry,
        &other,
        AgentRequest::SetTitle { title: " ".into() },
    );
    let ChannelResponse::Members { participants, .. } = channel(
        &registry,
        None,
        ChannelRequest::ListMembers {
            channel_id: team.clone(),
            after: None,
            limit: 8,
        },
    )
    .unwrap() else {
        panic!("expected roster")
    };
    let old_id = old.channel_participant.clone().unwrap();
    assert!(participants.iter().any(|p| p.id == old_id));
    assert!(
        participants
            .iter()
            .any(|p| Some(&p.id) == other.channel_participant.as_ref())
    );
    assert!(matches!(
        channel(
            &registry,
            Some(&old),
            post(&desk, "question", PostIntent::Question)
        )
        .unwrap(),
        ChannelResponse::Posted {
            wake_count: 0,
            attention: true,
            ..
        }
    ));
    assert!(matches!(
        channel(
            &registry,
            Some(&other),
            post(&team, "team", PostIntent::Note)
        )
        .unwrap(),
        ChannelResponse::Posted {
            wake_count: 1,
            attention: false,
            ..
        }
    ));
    let create = |name: &str| {
        let ChannelResponse::Channel { channel } = channel(
            &registry,
            None,
            ChannelRequest::Create {
                workspace_id: workspace.id.clone(),
                name: name.into(),
                membership: MembershipMode::Explicit,
                members: vec![old_id.clone()],
                settings: Default::default(),
            },
        )
        .unwrap() else {
            panic!("expected created channel")
        };
        channel.id
    };
    let kept = create("kept");
    let removed = create("removed");
    channel(
        &registry,
        None,
        ChannelRequest::SetMembership {
            channel_id: removed.clone(),
            membership: MembershipMode::Explicit,
            members: Vec::new(),
        },
    )
    .unwrap();
    assert!(channel(&registry, Some(&old), history(&removed)).is_err());
    channel(
        &registry,
        None,
        post(&kept, "prior shared history", PostIntent::Note),
    )
    .unwrap();
    let (stale_mention, old_selector) = mention_and_launch(&registry, &other, &team, &old_id);
    assert_eq!(old_selector, old.grant.launch_id);
    let original_receiver = receiver(&registry, &old);
    let interrupted = subscribe(&registry, &old, &original_receiver);
    assert_eq!(interrupted["channel_wake"]["channel_id"], team.0);
    let direct = agent(
        &registry,
        &other,
        AgentRequest::SendMessage {
            to: old.grant.launch_id.clone(),
            text: "old launch only".into(),
            idempotency_key: "dm".into(),
            scope: AgentScope::Workspace,
        },
    );
    assert_eq!(direct["durable"], true);
    let read_inbox = || AgentRequest::ReadInbox {
        after: None,
        limit: None,
        ack_ids: Vec::new(),
        include_read: true,
    };
    assert_eq!(
        agent(&registry, &old, read_inbox())["messages"][0]["id"],
        direct["message_id"]
    );
    let pane = old.grant.pane_id.clone();
    let window = &workspace.windows[0];
    drop(server);
    drop(registry);

    let (registry, server) = open(root);
    let expected = registry
        .recovery_candidates()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.pane_id == pane)
        .unwrap()
        .harness;
    let resume = recovery::ResumeAttempt {
        source: pane.clone(),
        expected,
        workspace: None,
    };
    let revision = registry.snapshot().unwrap().workspaces[0].windows[0].revision;
    // Use the owned launcher fixture as the exact PTY root of the reserved execution.
    registry
        .spawn_inner(
            SpawnTarget::Reopen {
                pane_id: pane.clone(),
                workspace_id: workspace.id.clone(),
                window_id: window.id.clone(),
                expected_revision: revision,
            },
            launch_spec(root, "a", NATIVE_ID),
            Some(resume),
            None,
        )
        .unwrap();
    let recovered = live(&registry, &a.home);
    assert_ne!(recovered.grant.launch_id, old.grant.launch_id);
    assert_ne!(recovered.grant.pane_id, pane);
    let ChannelResponse::History { messages, .. } =
        channel(&registry, Some(&recovered), history(&kept)).unwrap()
    else {
        panic!("expected retained history")
    };
    assert_eq!(messages[0].text, "prior shared history");
    let (live_mention, recovered_selector) =
        mention_and_launch(&registry, &recovered, &team, &old_id);
    assert_eq!(recovered_selector, recovered.grant.launch_id);
    assert_ne!(stale_mention, live_mention);
    let ChannelRequest::Post {
        channel_id,
        post: mut stale_post,
    } = post(&team, "reject stale selection", PostIntent::Note)
    else {
        unreachable!()
    };
    stale_post.mentions.push(stale_mention);
    let error = agent_channel(
        &registry,
        &recovered,
        ChannelRequest::Post {
            channel_id,
            post: stale_post,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("refresh the roster"), "{error}");
    assert!(channel(&registry, Some(&recovered), history(&removed)).is_err());
    assert!(channel(&registry, Some(&old), history(&kept)).is_err());
    let inbox = agent(&registry, &recovered, read_inbox());
    assert_eq!(inbox["messages"], json!([]));
    let recovered_receiver = receiver(&registry, &recovered);
    assert!(subscribe(&registry, &recovered, &recovered_receiver).is_null());
    let old_batch = interrupted["channel_wake"]["batch_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        agent_result(
            &registry,
            &recovered,
            AgentRequest::ChannelWake {
                batch_id: old_batch,
                max_body_bytes: Some(4096),
            }
        )
        .is_err()
    );
    let ChannelResponse::Posted { wake_count: 1, .. } = channel(
        &registry,
        None,
        post(&kept, "new launch wake", PostIntent::Note),
    )
    .unwrap() else {
        panic!("expected one new wake")
    };
    let db = rusqlite::Connection::open_with_flags(
        root.join("state/channels.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut work = db
        .prepare(
            "SELECT launch_id,pending FROM notification_work WHERE channel_id=?1 ORDER BY pending",
        )
        .unwrap();
    assert_eq!(
        work.query_map([kept.as_ref()], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, bool>(1)?
        )))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap(),
        [
            (old.grant.launch_id.clone(), false),
            (recovered.grant.launch_id.clone(), true)
        ]
    );
    drop(work);
    registry
        .request_authenticated(
            Request::new(Operation::HarnessEvent {
                launch_id: recovered.grant.launch_id.clone(),
                token: recovered.grant.token.clone(),
                event: CaptureEvent::End,
            }),
            &Peer {
                pid: recovered.root.pid,
                uid: unsafe { libc::geteuid() },
                birth: recovered.root.identity.clone(),
            },
        )
        .unwrap();
    assert!(channel(&registry, None, history(&kept)).is_ok());
    let pending: usize = db
        .query_row(
            "SELECT count(*) FROM notification_work WHERE launch_id=?1 AND pending=1",
            [&recovered.grant.launch_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    drop(db);
    let wake = subscribe(&registry, &recovered, &recovered_receiver);
    assert_eq!(wake["channel_wake"]["channel_id"], kept.0);
    assert_eq!(wake["channel_wake"]["pending_count"], 1);
    drop(server);
    drop(registry);

    let (registry, _server) = open(root);
    registry
        .inner
        .observer_started
        .store(true, std::sync::atomic::Ordering::Release);
    let pane = recovered.grant.pane_id;
    let expected = registry
        .recovery_candidates()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.pane_id == pane)
        .unwrap()
        .harness;
    let mut spec = launch_spec(root, "a", NATIVE_ID);
    // Keeping the shell alive makes the authenticated launcher a descendant,
    // not the exact PTY root of this reserved resume.
    spec.argv.splice(
        0..0,
        [
            "/bin/sh".into(),
            "-c".into(),
            "\"$@\"; IFS= read -r hold".into(),
            "fixture".into(),
        ],
    );
    registry
        .spawn_inner(
            SpawnTarget::Restore {
                pane_id: pane.clone(),
                workspace_id: workspace.id.clone(),
                window_id: window.id.clone(),
            },
            spec,
            Some(recovery::ResumeAttempt {
                source: pane,
                expected,
                workspace: None,
            }),
            None,
        )
        .unwrap();
    let replacement = live(&registry, &a.home);
    assert_ne!(replacement.root.pid, replacement.pane_root.pid);
    assert!(channel(&registry, Some(&replacement), history(&kept)).is_err());
    assert!(channel(&registry, Some(&replacement), history(&desk)).is_ok());
    let replacement_id = replacement.channel_participant.as_ref().unwrap().clone();
    let addressed = |key: &str| {
        let mut request = post(&desk, key, PostIntent::Note);
        if let ChannelRequest::Post { post, .. } = &mut request {
            post.mentions.push(replacement_id.clone());
        }
        request
    };
    assert!(channel(&registry, None, addressed("before exit")).is_ok());
    let process = crate::harness_process::process(replacement.root.pid)
        .unwrap()
        .unwrap();
    assert_eq!(process.identity, replacement.root.identity);
    assert_eq!(unsafe { libc::kill(process.pid, libc::SIGKILL) }, 0);
    let lifetime = replacement.channel_lifetime.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while lifetime.is_alive() {
        assert!(Instant::now() < deadline, "fixture launcher did not exit");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(registry.capture_pane(&replacement.grant.pane_id).is_ok());
    let error = channel(&registry, None, addressed("after exit")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("mention no longer names a current member")
    );
}

fn private_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[test]
fn workspace_presets_repair_without_resetting_existing_channel() {
    use bkpr_protocol::channels::{ChannelSettings, WakePolicy};

    let root = private_root();
    let registry = Registry::open(root.path()).unwrap();
    let workspace = send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: None,
        },
    );
    let id = workspace["workspace_id"].as_str().unwrap();
    let db_path = root.path().join("channels.sqlite3");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let entries = |db: &rusqlite::Connection| {
        let mut query = db
            .prepare("SELECT name,id FROM channels WHERE workspace_id=?1 ORDER BY name")
            .unwrap();
        query
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, ChannelId(row.get(1)?)))
            })
            .unwrap()
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()
            .unwrap()
    };
    let created = entries(&db);
    assert_eq!(
        created.keys().map(String::as_str).collect::<Vec<_>>(),
        ["#desk", "#team"]
    );
    let desk = &created["#desk"];
    let custom = ChannelSettings {
        user_wake: WakePolicy::None,
        ..Default::default()
    };
    channel(
        &registry,
        None,
        ChannelRequest::SetSettings {
            channel_id: desk.clone(),
            settings: custom,
        },
    )
    .unwrap();
    channel(
        &registry,
        None,
        ChannelRequest::SetMembership {
            channel_id: desk.clone(),
            membership: MembershipMode::Explicit,
            members: Vec::new(),
        },
    )
    .unwrap();
    let remove_team = |db: &rusqlite::Connection| {
        db.execute(
            "DELETE FROM channels WHERE workspace_id=?1 AND name='#team'",
            [id],
        )
        .unwrap();
    };
    let list = || ChannelRequest::List {
        workspace_id: id.into(),
        after: None,
        limit: 8,
    };
    remove_team(&db);
    let first = channel(&registry, None, list()).unwrap();
    assert_eq!(channel(&registry, None, list()).unwrap(), first);
    assert_eq!(entries(&db).len(), 2);
    remove_team(&db);
    drop(registry);

    let registry = Registry::open(root.path()).unwrap();
    let reopened = entries(&db);
    assert_eq!(reopened.len(), 2);
    assert_eq!(&reopened["#desk"], desk);
    let ChannelResponse::Listed {
        channels,
        next_after: None,
    } = channel(&registry, None, list()).unwrap()
    else {
        panic!("expected both presets");
    };
    let channel = channels.iter().find(|channel| &channel.id == desk).unwrap();
    assert_eq!(channel.settings, custom);
    assert_eq!(channel.membership, MembershipMode::Explicit);
}

#[test]
fn create_repairs_presets_after_storage_recovers() {
    let root = private_root();
    let registry = Registry::open(root.path()).unwrap();
    let path = root.path().join("channels.sqlite3");
    fs::create_dir(&path).unwrap();
    let workspace = send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: None,
        },
    );
    let id = workspace["workspace_id"].as_str().unwrap();
    let create = || ChannelRequest::Create {
        workspace_id: id.into(),
        name: "custom".into(),
        membership: MembershipMode::WorkspaceWide,
        members: Vec::new(),
        settings: Default::default(),
    };
    assert!(channel(&registry, None, create()).is_err());
    fs::remove_dir(&path).unwrap();
    channel(&registry, None, create()).unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    let mut query = db
        .prepare("SELECT name FROM channels WHERE workspace_id=?1 ORDER BY name")
        .unwrap();
    let names = query
        .query_map([id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(names, ["#desk", "#team", "custom"]);
}

#[test]
fn authenticated_channel_tools_and_native_wakes_keep_recipient_and_membership_fences() {
    use bkpr_protocol::channels::{ChannelSettings, WakePolicy};

    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let spec = launch_spec(root, "sender", "native-sender");
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec.clone()),
        },
    );
    let sender = live(&registry, &spec.home);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0).id;
    let add = |name| {
        let spec = launch_spec(root, name, name);
        let revision = registry
            .snapshot()
            .unwrap()
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .unwrap()
            .revision;
        send(
            &registry,
            Operation::WindowCreate {
                workspace_id: workspace.clone(),
                expected_revision: revision,
                name: None,
                spec: spec.clone(),
            },
        );
        live(&registry, &spec.home)
    };
    let a = add("a");
    let b = add("b");
    let a_actor = a.channel_participant.clone().unwrap();
    let b_actor = b.channel_participant.clone().unwrap();
    let foreign = send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: None,
        },
    )["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let list = |workspace_id| ChannelRequest::List {
        workspace_id,
        after: None,
        limit: 10,
    };
    let listed = agent_channel(&registry, &sender, list(foreign.clone())).unwrap();
    assert_eq!(listed["channels"].as_array().unwrap().len(), 2);
    assert!(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["workspace_id"] == workspace)
    );
    let ChannelResponse::Listed {
        channels: foreign_channels,
        ..
    } = channel(&registry, None, list(foreign)).unwrap()
    else {
        panic!("expected presets")
    };
    let missing = history(&ChannelId(uuid::Uuid::new_v4().to_string()));
    let denial = |request| {
        agent_channel(&registry, &sender, request)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(denial(history(&foreign_channels[0].id)), denial(missing));

    let desk = ChannelId("#desk".into());
    let team = ChannelId("#team".into());
    let team_id = ChannelId(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "#team")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .into(),
    );
    let (a_mention, a_selector) = mention_and_launch(&registry, &sender, &team_id, &a_actor);
    assert_eq!(a_selector, a.grant.launch_id);
    let passive = agent_channel(
        &registry,
        &sender,
        post(&desk, "passive desk", PostIntent::Note),
    )
    .unwrap();
    assert_eq!(passive["wake_count"], 0);
    let addressed_post = |text| {
        let ChannelRequest::Post {
            channel_id,
            mut post,
        } = post(&team, text, PostIntent::Note)
        else {
            unreachable!()
        };
        post.mentions.push(a_mention.clone());
        ChannelRequest::Post { channel_id, post }
    };
    let older = agent_channel(&registry, &sender, addressed_post("older callout")).unwrap();
    let newest = agent_channel(&registry, &sender, addressed_post("newest callout")).unwrap();
    assert_eq!(newest["message"]["mentions"], json!([a_actor]));
    let broadcast = post(&team, "newer broadcast", PostIntent::Note);
    let receipt = agent_channel(&registry, &sender, broadcast.clone()).unwrap();
    assert_eq!(receipt["wake_count"], 2);
    assert_eq!(
        agent_channel(&registry, &sender, broadcast).unwrap(),
        receipt
    );
    let a_receiver = receiver(&registry, &a);
    let b_receiver = receiver(&registry, &b);
    let a_wake = subscribe(&registry, &a, &a_receiver);
    let b_wake = subscribe(&registry, &b, &b_receiver);
    let a_pointer = &a_wake["channel_wake"];
    let b_pointer = &b_wake["channel_wake"];
    assert_eq!(a_pointer["pending_count"], 3);
    assert_eq!(b_pointer["pending_count"], 3);
    assert_eq!(a_pointer["addressed_message_id"], newest["message"]["id"]);
    assert!(b_pointer.get("addressed_message_id").is_none());
    assert!(!a_wake.to_string().contains("newer broadcast"));
    assert_eq!(
        validate_wake(&registry, &a, &a_receiver, &a_wake)["current"],
        true
    );
    let lookup_with = |pointer: &Value, max_body_bytes| AgentRequest::ChannelWake {
        batch_id: pointer["batch_id"].as_str().unwrap().to_owned(),
        max_body_bytes,
    };
    let lookup = |pointer: &Value| lookup_with(pointer, None);
    let lookup_body = |pointer: &Value, maximum| lookup_with(pointer, Some(maximum));
    assert!(agent_result(&registry, &b, lookup(a_pointer)).is_err());
    assert!(agent_result(&registry, &sender, lookup(a_pointer)).is_err());
    assert!(agent_result(&registry, &b, lookup_body(a_pointer, 4096)).is_err());
    assert!(agent_result(&registry, &a, lookup_body(a_pointer, 4097)).is_err());
    let detail = agent(&registry, &a, lookup(a_pointer));
    assert_eq!(
        detail["addressed_message_id"],
        a_pointer["addressed_message_id"]
    );
    assert_eq!(detail["messages"][0]["id"], older["message"]["id"]);
    assert_eq!(detail["messages"][0]["addressed"], true);
    assert_eq!(detail["messages"][2]["addressed"], false);
    assert!(detail.get("post").is_none());
    let callout = agent(&registry, &a, lookup_body(a_pointer, 6));
    assert_eq!(callout["post"]["message"]["id"], newest["message"]["id"]);
    assert_eq!(callout["post"]["message"]["author"]["kind"], "agent");
    assert_eq!(callout["post"]["text"], "newest");
    assert_eq!(callout["post"]["next_offset"], 6);
    assert_eq!(
        callout["post"]["reply_target"],
        json!({
            "channel_id": a_pointer["channel_id"], "reply_to": newest["message"]["id"],
        })
    );
    assert!(
        agent(&registry, &b, lookup_body(b_pointer, 4096))
            .get("post")
            .is_none()
    );

    agent(
        &registry,
        &sender,
        AgentRequest::SendMessage {
            to: a.grant.launch_id.clone(),
            text: "separate direct message".into(),
            idempotency_key: "direct".into(),
            scope: AgentScope::Workspace,
        },
    );
    let mut wrong = a_wake.clone();
    wrong["notification_id"] = b_wake["notification_id"].clone();
    assert_eq!(
        validate_wake(&registry, &a, &a_receiver, &wrong)["current"],
        false
    );
    assert!(subscribe(&registry, &a, &a_receiver).is_null());
    assert_eq!(
        settle_wake(&registry, &a, &a_receiver, &a_wake)["recorded"],
        true
    );
    assert_eq!(
        settle_wake(&registry, &b, &b_receiver, &b_wake)["recorded"],
        true
    );
    let direct = subscribe(&registry, &a, &a_receiver);
    assert!(direct.get("channel_wake").is_none());
    assert!(direct["generation"].as_u64().unwrap() > a_wake["generation"].as_u64().unwrap());
    assert_eq!(agent(&registry, &a, lookup(a_pointer)), detail);

    let created = agent_channel(
        &registry,
        &sender,
        ChannelRequest::Create {
            workspace_id: "@workspace".into(),
            name: "named".into(),
            membership: MembershipMode::Explicit,
            members: vec![b_actor.clone()],
            settings: ChannelSettings {
                agent_wake: WakePolicy::All,
                ..Default::default()
            },
        },
    )
    .unwrap();
    let named = ChannelId(created["channel"]["id"].as_str().unwrap().into());
    assert!(agent_channel(&registry, &a, history(&named)).is_err());
    agent_channel(
        &registry,
        &sender,
        post(&named, "claimed then revoked", PostIntent::Note),
    )
    .unwrap();
    let revoked = subscribe(&registry, &b, &b_receiver);
    agent_channel(
        &registry,
        &sender,
        post(&named, "queued then revoked", PostIntent::Note),
    )
    .unwrap();
    let creator = sender.channel_participant.clone().unwrap();
    for members in [vec![creator.clone()], vec![creator, b_actor.clone()]] {
        channel(
            &registry,
            None,
            ChannelRequest::SetMembership {
                channel_id: named.clone(),
                membership: MembershipMode::Explicit,
                members,
            },
        )
        .unwrap();
    }
    assert_eq!(
        validate_wake(&registry, &b, &b_receiver, &revoked)["current"],
        false
    );
    assert!(agent_result(&registry, &b, lookup(&revoked["channel_wake"])).is_err());
    assert!(agent_result(&registry, &b, lookup_body(&revoked["channel_wake"], 4096)).is_err());
    assert!(subscribe(&registry, &b, &b_receiver).is_null());
    assert!(agent_channel(&registry, &b, history(&named)).is_ok());

    for index in 0..35 {
        agent_channel(
            &registry,
            &sender,
            post(&named, &format!("queued-{index}"), PostIntent::Note),
        )
        .unwrap();
    }
    let first = subscribe(&registry, &b, &b_receiver);
    assert_eq!(first["channel_wake"]["pending_count"], 32);
    assert_eq!(
        agent(&registry, &b, lookup(&first["channel_wake"]))["messages"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        settle_wake(&registry, &b, &b_receiver, &first)["recorded"],
        true
    );
    let rest = subscribe(&registry, &b, &b_receiver);
    assert_eq!(rest["channel_wake"]["pending_count"], 3);
    assert_eq!(
        settle_wake(&registry, &b, &b_receiver, &rest)["recorded"],
        true
    );

    let desk = ChannelId(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "#desk")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let ChannelRequest::Post {
        channel_id,
        post: mut user_post,
    } = post(&desk, "東京 user direction", PostIntent::Note)
    else {
        unreachable!()
    };
    user_post.mentions.push(b_actor);
    let ChannelResponse::Posted {
        message,
        wake_count: 1,
        ..
    } = channel(
        &registry,
        None,
        ChannelRequest::Post {
            channel_id,
            post: user_post,
        },
    )
    .unwrap()
    else {
        panic!("expected only the addressed agent")
    };
    let sender_receiver = receiver(&registry, &sender);
    assert!(subscribe(&registry, &sender, &sender_receiver).is_null());
    let targeted = subscribe(&registry, &b, &b_receiver);
    assert_eq!(
        targeted["channel_wake"]["addressed_message_id"],
        message.id.0
    );
    let user = agent(&registry, &b, lookup_body(&targeted["channel_wake"], 4));
    assert_eq!(user["post"]["message"]["author"]["kind"], "user");
    assert_eq!(user["post"]["text"], "東");
    assert_eq!(user["post"]["next_offset"], 3);
    assert_eq!(
        user["post"]["reply_target"],
        json!({ "channel_id":desk.0, "reply_to":message.id.0 })
    );
    assert_eq!(
        settle_wake(&registry, &b, &b_receiver, &targeted)["recorded"],
        true
    );
    let ChannelResponse::Posted { message, .. } = channel(
        &registry,
        None,
        post(&desk, "broadcast singleton", PostIntent::Note),
    )
    .unwrap() else {
        panic!("expected user post");
    };
    let singleton = subscribe(&registry, &b, &b_receiver);
    assert!(
        singleton["channel_wake"]
            .get("addressed_message_id")
            .is_none()
    );
    let user = agent(&registry, &b, lookup_body(&singleton["channel_wake"], 2048));
    assert_eq!(user["post"]["message"]["id"], message.id.0);
    assert_eq!(user["post"]["text"], "broadcast singleton");
    assert!(user["post"]["next_offset"].is_null());
    let response = agent_channel(
        &registry,
        &b,
        ChannelRequest::Post {
            channel_id: serde_json::from_value(user["post"]["reply_target"]["channel_id"].clone())
                .unwrap(),
            post: ChannelPost {
                text: "reply sent in one call".into(),
                idempotency_key: "singleton-reply".into(),
                intent: PostIntent::Report,
                mentions: Vec::new(),
                attachments: Vec::new(),
                reply_to: Some(
                    serde_json::from_value(user["post"]["reply_target"]["reply_to"].clone())
                        .unwrap(),
                ),
            },
        },
    )
    .unwrap();
    assert_eq!(response["wake_count"], 0);
    assert_eq!(response["message"]["reply_to"], message.id.0);

    let body = "東京".repeat(2600);
    for index in 0..11 {
        let ChannelRequest::Post {
            channel_id,
            mut post,
        } = post(&desk, &format!("busy-{index}"), PostIntent::Note)
        else {
            unreachable!()
        };
        post.text.clone_from(&body);
        agent_channel(
            &registry,
            &sender,
            ChannelRequest::Post { channel_id, post },
        )
        .unwrap();
    }
    let page = agent_channel(
        &registry,
        &b,
        ChannelRequest::History {
            channel_id: desk.clone(),
            before: None,
            after: None,
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 10);
    assert!(!page.to_string().contains('東'));
    let message_id =
        bkpr_protocol::channels::MessageId(page["messages"][0]["id"].as_str().unwrap().into());
    let page = agent(
        &registry,
        &b,
        AgentRequest::ChannelRead {
            channel_id: desk,
            message_id,
            offset: None,
            max_bytes: Some(4096),
        },
    );
    assert_eq!(page["text"].as_str().unwrap(), &body[..4095]);
    assert_eq!(page["next_offset"], 4095);
}

#[test]
fn authenticated_channel_attachments_require_live_member() {
    use bkpr_protocol::channels::{AttachmentMediaType, AttachmentUpload};

    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let member_spec = launch_spec(root, "member", "native-member");
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(member_spec.clone()),
        },
    );
    let member = live(&registry, &member_spec.home);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let nonmember_spec = launch_spec(root, "nonmember", "native-nonmember");
    send(
        &registry,
        Operation::WindowCreate {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
            name: None,
            spec: nonmember_spec.clone(),
        },
    );
    let nonmember = live(&registry, &nonmember_spec.home);
    let actor = member.channel_participant.clone().unwrap();
    assert_ne!(nonmember.channel_participant.as_ref().unwrap(), &actor);

    let ChannelResponse::Channel {
        channel: restricted,
    } = channel(
        &registry,
        None,
        ChannelRequest::Create {
            workspace_id: workspace.id,
            name: "attachments".into(),
            membership: MembershipMode::Explicit,
            members: vec![actor.clone()],
            settings: Default::default(),
        },
    )
    .unwrap()
    else {
        panic!("expected created channel");
    };

    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, 1, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[24, 42, 71])
        .unwrap();
    let upload = || ChannelRequest::UploadAttachment {
        channel_id: restricted.id.clone(),
        upload: AttachmentUpload {
            media_type: AttachmentMediaType::Png,
            alt_text: Some("Example".into()),
            bytes: png.clone(),
        },
    };
    let call = |launch, request| {
        agent_payload(
            &registry,
            launch,
            AgentRequest::Channels {
                request: Box::new(request),
            },
        )
    };
    let Payload::Channels(ChannelResponse::AttachmentUploaded { attachment }) =
        call(&member, upload()).unwrap()
    else {
        panic!("expected upload metadata");
    };
    assert_eq!(attachment.media_type, AttachmentMediaType::Png);
    assert_eq!(attachment.byte_len as usize, png.len());
    assert_eq!(attachment.alt_text.as_deref(), Some("Example"));

    let ChannelRequest::Post {
        channel_id,
        mut post,
    } = post(&restricted.id, "attached image", PostIntent::Note)
    else {
        unreachable!()
    };
    post.attachments.push(attachment.id.clone());
    let posted = agent_channel(
        &registry,
        &member,
        ChannelRequest::Post { channel_id, post },
    )
    .unwrap();
    assert_eq!(posted["message"]["author"]["id"], json!(actor));
    assert_eq!(posted["message"]["attachments"], json!([attachment.id]));

    let fetch = || ChannelRequest::GetAttachment {
        channel_id: restricted.id.clone(),
        attachment_id: attachment.id.clone(),
    };
    let Payload::Channels(ChannelResponse::Attachment {
        attachment: fetched,
        bytes,
    }) = call(&member, fetch()).unwrap()
    else {
        panic!("expected fetched attachment");
    };
    assert_eq!(fetched, attachment);
    assert_eq!(bytes.as_ref(), png.as_slice());
    for request in [upload(), fetch()] {
        assert_eq!(
            call(&nonmember, request).unwrap_err().to_string(),
            "channel unavailable"
        );
    }

    // Revoke the launch while its process and explicit channel membership remain.
    launch_request(
        &registry,
        &member,
        Operation::HarnessEvent {
            launch_id: member.grant.launch_id.clone(),
            token: member.grant.token.clone(),
            event: CaptureEvent::End,
        },
    )
    .unwrap();
    for request in [upload(), fetch()] {
        assert_eq!(
            call(&member, request).unwrap_err().to_string(),
            "unknown or ended launch"
        );
    }
}
