use super::*;
use bkpr_protocol::channels::{
    ChannelId, ChannelPost, ChannelRequest, ChannelResponse, ChannelSource, MembershipMode,
    MessageId, ParticipantId, PostIntent,
};
use bkpr_protocol::{AgentReceiverMode, AgentWakeOutcome, LayoutNode, Window, Workspace};

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
    fs::write(home.join("native-pid"), _native.0.id().to_string()).unwrap();
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
    let _ = fs::remove_file(home.join("native-pid"));
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
            source: None,
        },
    }
}

#[test]
fn committed_channel_attention_reaches_host_once_without_post_body() {
    use bkpr_protocol::{
        ChannelAttentionKind, NotificationKind, NotificationPage, NotificationQuery,
        NotificationRequest, NotificationSource,
    };
    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let service =
        crate::notifications::NotificationService::open(&root.join("notifications")).unwrap();
    registry.configure_notifications(service.clone()).unwrap();
    let spec = launch_spec(root, "reporter", NATIVE_ID);
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec.clone()),
        },
    );
    let author = live(&registry, &spec.home);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let ChannelResponse::Listed { channels, .. } = channel(
        &registry,
        None,
        ChannelRequest::List {
            workspace_id: workspace.id.clone(),
            after: None,
            limit: 8,
        },
    )
    .unwrap() else {
        panic!("expected channels");
    };
    let desk = channels.into_iter().find(|c| c.name == "#desk").unwrap();
    let body = "unique private report body";
    let request = || post(&desk.id, body, PostIntent::Report);
    let first = agent_channel(&registry, &author, request()).unwrap();
    assert_eq!(agent_channel(&registry, &author, request()).unwrap(), first);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let value = service
            .request(
                NotificationRequest::Inspect {
                    query: NotificationQuery::default(),
                    after: None,
                },
                None,
                &|| true,
            )
            .unwrap()
            .into_json()
            .unwrap();
        assert!(!value.to_string().contains(body));
        let page: NotificationPage = serde_json::from_value(value).unwrap();
        if let [record] = page.notifications.as_slice() {
            assert_eq!(record.kind, NotificationKind::Attention);
            let NotificationSource::Channel(source) = &record.source else {
                panic!("expected channel source");
            };
            assert_eq!(source.workspace_id, workspace.id);
            assert_eq!(source.channel_id, desk.id);
            assert_eq!(source.message_id.0, first["message"]["id"]);
            assert_eq!(source.intent, ChannelAttentionKind::Report);
            break;
        }
        assert!(page.notifications.is_empty());
        assert!(
            std::time::Instant::now() < deadline,
            "attention was not routed"
        );
        thread::sleep(std::time::Duration::from_millis(10));
    }
    registry.shutdown().unwrap();
    service.shutdown().unwrap();
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
    let body = "東京\n".repeat(90);
    let send_direct = || direct_post(&old.grant.launch_id, &body, "dm");
    let direct = agent(&registry, &other, send_direct());
    assert_eq!(direct["wake_count"], 1);
    let ChannelResponse::Listed { channels, .. } = channel(&registry, None, list()).unwrap() else {
        panic!("expected workspace channels");
    };
    let direct_channel = channels.into_iter().find(|entry| entry.direct).unwrap().id;
    let message_id = direct["message"]["id"].as_str().unwrap().to_owned();
    let ack = || AgentRequest::ChannelAck {
        channel_id: direct_channel.clone(),
        message_id: channels::MessageId(message_id.clone()),
    };
    assert!(agent_result(&registry, &old, ack()).is_err());
    let page = agent(
        &registry,
        &old,
        AgentRequest::ChannelRead {
            channel_id: direct_channel.clone(),
            message_id: channels::MessageId(message_id.clone()),
            offset: None,
            max_bytes: Some(1024),
        },
    );
    assert_eq!(page["text"], body);
    assert_eq!(agent(&registry, &old, ack())["acknowledged"], true);
    assert_eq!(agent(&registry, &old, ack())["acknowledged"], false);
    assert_eq!(agent(&registry, &other, send_direct()), direct);
    let pane = old.grant.pane_id.clone();
    let window = &workspace.windows[0];
    drop(server);
    drop(registry);
    // A v9 store has the history but used a separate launch delivery ledger.
    let old_disk = rusqlite::Connection::open(root.join("state/channels.sqlite3")).unwrap();
    old_disk
        .execute_batch(
            "DELETE FROM notification_work WHERE channel_id IN (SELECT channel_id FROM direct_channels);
             DROP INDEX pending_launch_enqueue;
             DROP INDEX pending_channel_sequence;
             DROP INDEX wake_batch_membership;
             DROP INDEX active_batch_launch;
             DROP INDEX unsettled_batch_deadline;
             ALTER TABLE channel_batches DROP COLUMN revoked;
             ALTER TABLE notification_work DROP COLUMN enqueue_ordinal;
             ALTER TABLE notification_work DROP COLUMN channel_sequence;
             ALTER TABLE notification_work DROP COLUMN read_at;
             ALTER TABLE notification_work DROP COLUMN acknowledged_at;
             CREATE TABLE agent_state(singleton INTEGER PRIMARY KEY, snapshot BLOB);
             INSERT INTO agent_state VALUES(1,x'00');
             PRAGMA user_version=9;",
        )
        .unwrap();
    drop(old_disk);

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
    for caller in [None, Some(&recovered)] {
        let ChannelResponse::History { messages, .. } =
            channel(&registry, caller, history(&direct_channel)).unwrap()
        else {
            panic!("expected retained direct history");
        };
        assert_eq!(messages[0].message.id.0, message_id);
        assert_eq!(messages[0].text, body);
    }
    assert!(agent_result(&registry, &recovered, ack()).is_err());
    assert!(
        agent_result(
            &registry,
            &recovered,
            direct_post(&other.grant.launch_id, "retired", "new-key")
        )
        .is_err()
    );
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
                include_source: false,
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
    let wake = subscribe(&registry, &recovered, &recovered_receiver);
    assert_eq!(wake["channel_wake"]["channel_id"], kept.0);
    assert_eq!(wake["channel_wake"]["pending_count"], 1);
    assert_eq!(
        settle_wake(&registry, &recovered, &recovered_receiver, &wake)["recorded"],
        true
    );
    assert!(
        channel(
            &registry,
            None,
            post(
                &kept,
                "pending when recovered launch ends",
                PostIntent::Note
            ),
        )
        .is_ok()
    );
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
    assert!(
        agent_result(
            &registry,
            &recovered,
            AgentRequest::ReceiverSubscribe {
                receiver_id: recovered_receiver,
                wait_ms: 0,
            },
        )
        .is_err()
    );
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
    assert!(channel(&registry, Some(&replacement), history(&direct_channel)).is_err());
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
    let saved_desk = channels.iter().find(|channel| &channel.id == desk).unwrap();
    assert_eq!(saved_desk.settings, custom);
    assert_eq!(saved_desk.membership, MembershipMode::Explicit);

    let team = reopened["#team"].clone();
    let legacy = ChannelSettings {
        user_wake: WakePolicy::All,
        agent_wake: WakePolicy::All,
        attention_reports: false,
        attention_questions: false,
    };
    channel(
        &registry,
        None,
        ChannelRequest::SetSettings {
            channel_id: team.clone(),
            settings: legacy,
        },
    )
    .unwrap();
    drop(registry);

    let registry = Registry::open(root.path()).unwrap();
    let ChannelResponse::Listed { channels, .. } = channel(&registry, None, list()).unwrap() else {
        panic!("expected both presets");
    };
    let saved = channels
        .iter()
        .find(|channel| channel.name == "#team")
        .unwrap();
    assert_eq!(saved.id, team);
    assert_eq!(saved.settings, legacy);
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
fn authenticated_compact_channel_reads_resolve_only_workspace_builtins() {
    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let spec = launch_spec(root, "reader", "native-reader");
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec.clone()),
        },
    );
    let reader = live(&registry, &spec.home);
    let foreign = send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: None,
        },
    );
    let ChannelResponse::Listed {
        channels: foreign_channels,
        ..
    } = channel(
        &registry,
        None,
        ChannelRequest::List {
            workspace_id: foreign["workspace_id"].as_str().unwrap().into(),
            after: None,
            limit: 10,
        },
    )
    .unwrap()
    else {
        panic!("expected foreign workspace presets");
    };

    let compact = |request| agent_channel(&registry, &reader, request);
    let members = |id: &ChannelId| ChannelRequest::ListMembers {
        channel_id: id.clone(),
        after: None,
        limit: 10,
    };
    for name in ["#desk", "#team"] {
        let selector = ChannelId(name.into());
        let empty = compact(history(&selector)).unwrap();
        assert_eq!(empty["messages"], json!([]));
        let exact_id = ChannelId(empty["cursor"]["channel_id"].as_str().unwrap().into());
        assert_ne!(exact_id, selector);
        assert_eq!(compact(history(&exact_id)).unwrap(), empty);
        let roster = compact(members(&selector)).unwrap();
        assert_eq!(compact(members(&exact_id)).unwrap(), roster);
        assert!(
            roster["participants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["launch_id"] == reader.grant.launch_id)
        );

        let body = format!("東京 {name} actual body");
        let receipt = compact(post(&selector, &body, PostIntent::Note)).unwrap();
        let summary = &receipt["message"];
        let listed = compact(history(&selector)).unwrap();
        assert_eq!(listed["messages"], json!([summary]));
        assert_eq!(listed, compact(history(&exact_id)).unwrap());
        assert!(!listed.to_string().contains(body.as_str()));
        let ChannelResponse::History { messages, .. } =
            channel(&registry, None, history(&exact_id)).unwrap()
        else {
            panic!("expected native history");
        };
        assert_eq!(messages[0].text, body);

        let read = |id: &ChannelId, message_id, offset, max_bytes| {
            agent_result(
                &registry,
                &reader,
                AgentRequest::ChannelRead {
                    channel_id: id.clone(),
                    message_id,
                    offset,
                    max_bytes,
                },
            )
        };
        let message_id = bkpr_protocol::channels::MessageId(summary["id"].as_str().unwrap().into());
        let first = read(&selector, message_id.clone(), None, Some(4)).unwrap();
        assert_eq!(first["message"], *summary);
        assert_eq!(
            first["reply_target"],
            json!({"channel_id": exact_id, "reply_to": message_id})
        );
        assert_eq!(first["text"], "東");
        assert_eq!(first["body_bytes"], body.len());
        assert_eq!(first["next_offset"], 3);
        assert_eq!(
            first,
            read(&exact_id, message_id.clone(), None, Some(4)).unwrap()
        );
        let rest = read(&selector, message_id.clone(), Some(3), Some(4096)).unwrap();
        assert_eq!(rest["text"], &body[3..]);
        assert!(rest["next_offset"].is_null());
        let ChannelResponse::Message { message: native } = channel(
            &registry,
            None,
            ChannelRequest::Read {
                channel_id: exact_id,
                message_id: message_id.clone(),
            },
        )
        .unwrap() else {
            panic!("expected native message");
        };
        assert_eq!(native.text, body);

        assert!(channel(&registry, None, history(&selector)).is_err());
        assert!(channel(&registry, None, members(&selector)).is_err());
        assert!(
            channel(
                &registry,
                None,
                ChannelRequest::Read {
                    channel_id: selector,
                    message_id,
                },
            )
            .is_err()
        );

        let foreign_id = &foreign_channels.iter().find(|c| c.name == name).unwrap().id;
        let ChannelResponse::Posted { message, .. } = channel(
            &registry,
            None,
            post(
                foreign_id,
                &format!("foreign {name} body"),
                PostIntent::Note,
            ),
        )
        .unwrap() else {
            panic!("expected a real foreign post");
        };
        assert!(compact(history(foreign_id)).is_err());
        assert!(compact(members(foreign_id)).is_err());
        assert!(read(foreign_id, message.id, None, Some(4)).is_err());
    }
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
    let desk_id: ChannelId = serde_json::from_value(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "#desk")
            .unwrap()["id"]
            .clone(),
    )
    .unwrap();
    let (a_mention, a_selector) = mention_and_launch(&registry, &sender, &team_id, &a_actor);
    assert_eq!(a_selector, a.grant.launch_id);
    let ChannelRequest::Post {
        channel_id,
        post: mut passive_post,
    } = post(&desk, "passive desk", PostIntent::Note)
    else {
        unreachable!()
    };
    let source_body = "東京".repeat(400);
    passive_post.text.clone_from(&source_body);
    passive_post.source = Some(Box::new(ChannelSource {
        channel_id: desk_id.clone(),
        message_id: MessageId("nested-dangling".into()),
    }));
    let passive = agent_channel(
        &registry,
        &sender,
        ChannelRequest::Post {
            channel_id,
            post: passive_post,
        },
    )
    .unwrap();
    assert_eq!(passive["wake_count"], 0);
    for source in [
        ChannelSource {
            channel_id: ChannelId(sender.grant.token.clone()),
            message_id: MessageId("ordinary-dangling".into()),
        },
        ChannelSource {
            channel_id: desk_id.clone(),
            message_id: MessageId(sender.grant.token.clone()),
        },
    ] {
        let ChannelRequest::Post {
            channel_id,
            mut post,
        } = post(
            &desk,
            "must not share a capture credential",
            PostIntent::Note,
        )
        else {
            unreachable!()
        };
        post.source = Some(Box::new(source));
        let error = agent_channel(
            &registry,
            &sender,
            ChannelRequest::Post { channel_id, post },
        )
        .unwrap_err();
        assert!(error.to_string().contains("private capability"));
        assert!(!error.to_string().contains(&sender.grant.token));
    }
    assert_eq!(
        agent_channel(&registry, &sender, history(&desk)).unwrap()["messages"],
        json!([passive["message"]])
    );
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
    let ChannelRequest::Post {
        channel_id,
        post: mut newest_post,
    } = addressed_post("newest callout")
    else {
        unreachable!()
    };
    let source = Box::new(ChannelSource {
        channel_id: desk_id.clone(),
        message_id: serde_json::from_value(passive["message"]["id"].clone()).unwrap(),
    });
    newest_post.source = Some(source.clone());
    let newest = agent_channel(
        &registry,
        &sender,
        ChannelRequest::Post {
            channel_id,
            post: newest_post,
        },
    )
    .unwrap();
    assert_eq!(newest["message"]["mentions"], json!([a_actor]));
    assert_eq!(older["wake_count"], 1);
    assert_eq!(newest["wake_count"], 1);

    let (self_mention, _) = mention_and_launch(
        &registry,
        &sender,
        &team_id,
        sender.channel_participant.as_ref().unwrap(),
    );
    let ChannelRequest::Post {
        channel_id,
        post: mut ineligible,
    } = post(&team, "addressed to self", PostIntent::Note)
    else {
        unreachable!()
    };
    ineligible.mentions.push(self_mention);
    let self_callout = agent_channel(
        &registry,
        &sender,
        ChannelRequest::Post {
            channel_id,
            post: ineligible,
        },
    )
    .unwrap();
    assert_eq!(self_callout["wake_count"], 0);

    let broadcast = post(&team, "newer broadcast", PostIntent::Note);
    let receipt = agent_channel(&registry, &sender, broadcast.clone()).unwrap();
    assert_eq!(receipt["wake_count"], 2);
    assert_eq!(
        agent_channel(&registry, &sender, broadcast).unwrap(),
        receipt
    );
    let next_broadcast = agent_channel(
        &registry,
        &sender,
        post(&team, "last broadcast", PostIntent::Note),
    )
    .unwrap();
    assert_eq!(next_broadcast["wake_count"], 2);
    let a_receiver = receiver(&registry, &a);
    let b_receiver = receiver(&registry, &b);
    registry
        .inner
        .channels
        .with_delivery_store(&|| false, &|_| true, |_| {
            // Claim admission cannot reach SQLite while this operation owns the writer.
            assert!(subscribe(&registry, &a, &a_receiver).is_null());
            Ok(())
        })
        .unwrap();
    let a_wake = subscribe(&registry, &a, &a_receiver);
    let b_wake = subscribe(&registry, &b, &b_receiver);
    let a_pointer = &a_wake["channel_wake"];
    let b_pointer = &b_wake["channel_wake"];
    assert_eq!(a_pointer["pending_count"], 4);
    assert_eq!(b_pointer["pending_count"], 2);
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
        include_source: false,
    };
    let lookup_source = |pointer: &Value, max_body_bytes| AgentRequest::ChannelWake {
        batch_id: pointer["batch_id"].as_str().unwrap().to_owned(),
        max_body_bytes,
        include_source: true,
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
    let other = agent(&registry, &b, lookup(b_pointer));
    assert_eq!(other["messages"][0]["id"], receipt["message"]["id"]);
    assert_eq!(other["messages"][1]["id"], next_broadcast["message"]["id"]);
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
    assert_eq!(callout["post"]["message"]["source"], json!(source));
    assert!(callout["post"].get("source").is_none());
    assert!(a_pointer.get("source").is_none());
    let expanded = agent(&registry, &a, lookup_source(a_pointer, Some(4)));
    assert_eq!(expanded["post"]["text"], "newe");
    let source_page = &expanded["post"]["source"];
    let mut source_message = passive["message"].clone();
    source_message
        .as_object_mut()
        .unwrap()
        .remove("source")
        .unwrap();
    assert_eq!(source_page["message"], source_message);
    assert_eq!(source_page["text"], "東");
    assert_eq!(source_page["body_bytes"], source_body.len());
    assert_eq!(source_page["next_offset"], 3);
    assert_eq!(
        source_page["reply_target"],
        json!({ "channel_id": desk_id, "reply_to": source.message_id }),
    );
    assert!(source_page.get("source").is_none());
    let next_page = agent(
        &registry,
        &a,
        AgentRequest::ChannelRead {
            channel_id: source.channel_id.clone(),
            message_id: source.message_id,
            offset: Some(3),
            max_bytes: Some(4),
        },
    );
    assert_eq!(next_page["text"], "京");
    assert_eq!(next_page["next_offset"], 6);
    let default = agent(&registry, &a, lookup_source(a_pointer, None));
    assert_eq!(default["post"]["text"], "newest callout");
    assert_eq!(default["post"]["source"]["next_offset"], 2046);
    assert_eq!(default["post"]["source"]["text"], &source_body[..2046]);
    assert!(
        agent(&registry, &b, lookup_source(b_pointer, None))
            .get("post")
            .is_none()
    );
    assert!(
        agent(&registry, &b, lookup_body(b_pointer, 4096))
            .get("post")
            .is_none()
    );

    let dm = agent(
        &registry,
        &sender,
        direct_post(&a.grant.launch_id, "direct message", "direct"),
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
    assert_eq!(direct["channel_wake"]["channel_id"], dm["channel_id"]);
    assert_eq!(
        direct["channel_wake"]["addressed_message_id"],
        dm["message"]["id"]
    );
    assert!(direct["generation"].as_u64().unwrap() > a_wake["generation"].as_u64().unwrap());
    settle_wake(&registry, &a, &a_receiver, &direct);
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

    let desk = desk_id;
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
    assert_eq!(
        agent(
            &registry,
            &b,
            lookup_source(&singleton["channel_wake"], None)
        ),
        user
    );
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
                source: None,
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

    let user_post = |request| {
        let ChannelResponse::Posted { wake_count, .. } = channel(&registry, None, request).unwrap()
        else {
            panic!("expected user post")
        };
        wake_count
    };
    assert_eq!(
        user_post(post(&team_id, "request for all peers", PostIntent::Note)),
        3
    );
    let ChannelRequest::Post {
        channel_id,
        post: mut addressed,
    } = post(&team_id, "request for coordinator", PostIntent::Note)
    else {
        unreachable!()
    };
    addressed.mentions.push(a_actor);
    assert_eq!(
        user_post(ChannelRequest::Post {
            channel_id,
            post: addressed,
        }),
        1
    );
}

#[test]
fn channel_subscription_sees_empty_to_pending_and_rechecks_an_exiting_receiver() {
    use crate::registry::{SpawnBarrier, agents::ChannelTestPause};
    use std::sync::mpsc::sync_channel;

    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let spec = launch_spec(root, "receiver", NATIVE_ID);
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(spec.clone()),
        },
    );
    let launch = live(&registry, &spec.home);
    let inbox = agent_channel(&registry, &launch, history(&ChannelId("#desk".into()))).unwrap();
    let desk = ChannelId(inbox["cursor"]["channel_id"].as_str().unwrap().into());
    let native_pid: i32 = fs::read_to_string(spec.home.join("native-pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(native_pid, launch.root.pid);
    let native = crate::harness_process::process(native_pid)
        .unwrap()
        .unwrap();
    let lifetime = crate::control_lease::ProcessLifetime::observe(&native).unwrap();
    let registered = agent(
        &registry,
        &launch,
        AgentRequest::ReceiverRegister {
            session_id: NATIVE_ID.into(),
            mode: AgentReceiverMode::ActiveTurn,
            pid: native_pid.try_into().unwrap(),
        },
    );
    let receiver = registered["receiver_id"].as_str().unwrap();
    channel(&registry, None, post(&desk, "first", PostIntent::Note)).unwrap();
    let first = subscribe(&registry, &launch, receiver);
    assert_eq!(
        settle_wake(&registry, &launch, receiver, &first)["recorded"],
        true
    );

    let pause = |phase| {
        let (entered, notice) = sync_channel(0);
        let (resume, resume_rx) = sync_channel(0);
        registry.inner.agents.pause_channel(
            phase,
            Arc::new(SpawnBarrier {
                entered,
                resume: Mutex::new(resume_rx),
            }),
        );
        (notice, resume)
    };
    let second = thread::scope(|scope| {
        let (notice, resume) = pause(ChannelTestPause::EmptyClaim);
        let registry_ref = &registry;
        let launch_ref = &launch;
        let waiting = scope.spawn(move || {
            agent(
                registry_ref,
                launch_ref,
                AgentRequest::ReceiverSubscribe {
                    receiver_id: receiver.into(),
                    wait_ms: 5000,
                },
            )["notification"]
                .take()
        });
        notice.recv_timeout(Duration::from_secs(5)).unwrap();
        channel(
            &registry,
            None,
            post(&desk, "committed during empty probe", PostIntent::Note),
        )
        .unwrap();
        resume.send(()).unwrap();
        waiting.join().unwrap()
    });
    assert_eq!(second["channel_wake"]["channel_id"], desk.0);
    assert_eq!(second["channel_wake"]["pending_count"], 1);

    thread::scope(|scope| {
        let (notice, resume) = pause(ChannelTestPause::Validate);
        let req = AgentRequest::WakeValidate {
            receiver_id: receiver.into(),
            notification_id: second["notification_id"].as_str().unwrap().into(),
            generation: second["generation"].as_u64().unwrap(),
        };
        let registry_ref = &registry;
        let launch_ref = &launch;
        let validating = scope.spawn(move || agent_result(registry_ref, launch_ref, req));
        notice.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(unsafe { libc::kill(native.pid, libc::SIGKILL) }, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while lifetime.is_alive() {
            assert!(
                Instant::now() < deadline,
                "fixture native receiver did not exit"
            );
            thread::sleep(Duration::from_millis(5));
        }
        resume.send(()).unwrap();
        assert!(validating.join().unwrap().is_err());
    });
    assert!(launch.channel_launch_alive());
    let replacement = self::receiver(&registry, &launch);
    assert_ne!(replacement, receiver);
    assert!(subscribe(&registry, &launch, &replacement).is_null());
    channel(
        &registry,
        None,
        post(&desk, "after receiver replacement", PostIntent::Note),
    )
    .unwrap();
    let next = subscribe(&registry, &launch, &replacement);
    assert_eq!(next["channel_wake"]["pending_count"], 1);
    assert!(next["generation"].as_u64().unwrap() > second["generation"].as_u64().unwrap());
}

#[test]
fn channel_wake_author_mention_tracks_live_binding_and_membership() {
    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let author_spec = launch_spec(root, "author", "author-native");
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(author_spec.clone()),
        },
    );
    let author = live(&registry, &author_spec.home);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let reader_spec = launch_spec(root, "reader", "reader-native");
    send(
        &registry,
        Operation::WindowCreate {
            workspace_id: workspace.id.clone(),
            expected_revision: workspace.revision,
            name: None,
            spec: reader_spec.clone(),
        },
    );
    let reader = live(&registry, &reader_spec.home);
    let author_id = author.channel_participant.as_ref().unwrap();
    let reader_id = reader.channel_participant.as_ref().unwrap();
    let ChannelResponse::Channel { channel: group } = channel(
        &registry,
        None,
        ChannelRequest::Create {
            workspace_id: workspace.id,
            name: "author mention".into(),
            membership: MembershipMode::Explicit,
            members: vec![author_id.clone(), reader_id.clone()],
            settings: Default::default(),
        },
    )
    .unwrap() else {
        panic!("expected an explicit channel");
    };
    let receipt = agent_channel(
        &registry,
        &author,
        post(&group.id, "proposal from a live author", PostIntent::Note),
    )
    .unwrap();
    assert_eq!(receipt["wake_count"], 1);
    let reader_receiver = receiver(&registry, &reader);
    let notification = subscribe(&registry, &reader, &reader_receiver);
    let pointer = &notification["channel_wake"];
    assert!(pointer.get("author_mention_id").is_none());
    let lookup = |max_body_bytes| {
        agent(
            &registry,
            &reader,
            AgentRequest::ChannelWake {
                batch_id: pointer["batch_id"].as_str().unwrap().to_owned(),
                max_body_bytes,
                include_source: false,
            },
        )
    };
    assert!(lookup(None).get("post").is_none());
    let (old, _) = mention_and_launch(&registry, &reader, &group.id, author_id);
    let selected = lookup(Some(2048));
    assert_eq!(selected["post"]["author_mention_id"], old.as_ref());
    assert_eq!(selected["post"]["message"], receipt["message"]);
    assert_ne!(old, *author_id);
    let event = |event| {
        launch_request(
            &registry,
            &author,
            Operation::HarnessEvent {
                launch_id: author.grant.launch_id.clone(),
                token: author.grant.token.clone(),
                event,
            },
        )
        .unwrap()
        .into_json()
        .unwrap()
    };
    let conversation = author.managed.conversation.as_ref().unwrap();
    assert_eq!(
        event(CaptureEvent::Bind {
            conversation_id: conversation.id.clone(),
            source: conversation.source,
            expected_revision: Some(conversation.generation),
        })["accepted"],
        true
    );
    let (current, _) = mention_and_launch(&registry, &reader, &group.id, author_id);
    assert_ne!(current, old);
    assert_eq!(
        lookup(Some(2048))["post"]["author_mention_id"],
        current.as_ref()
    );
    let ChannelRequest::Post {
        channel_id,
        mut post,
    } = post(&group.id, "reject replaced handle", PostIntent::Note)
    else {
        unreachable!()
    };
    post.mentions.push(old);
    assert!(
        agent_channel(
            &registry,
            &reader,
            ChannelRequest::Post { channel_id, post }
        )
        .unwrap_err()
        .to_string()
        .contains("refresh the roster")
    );

    let membership = |members| {
        channel(
            &registry,
            None,
            ChannelRequest::SetMembership {
                channel_id: group.id.clone(),
                membership: MembershipMode::Explicit,
                members,
            },
        )
        .unwrap()
    };
    membership(vec![reader_id.clone()]);
    let without_author = lookup(Some(2048));
    assert!(without_author["post"].get("author_mention_id").is_none());
    assert_eq!(without_author["post"]["message"], receipt["message"]);
    membership(vec![author_id.clone(), reader_id.clone()]);
    assert_eq!(
        lookup(Some(2048))["post"]["author_mention_id"],
        current.as_ref()
    );
    event(CaptureEvent::End);
    assert_eq!(lookup(Some(2048)), without_author);
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

fn add_channel_peer(registry: &Registry, spec: PaneSpawnSpec) -> Launch {
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    send(
        registry,
        Operation::WindowCreate {
            workspace_id: workspace.id,
            expected_revision: workspace.revision,
            name: None,
            spec: spec.clone(),
        },
    );
    live(registry, &spec.home)
}

fn open_channel_pair(root: &Path) -> (Registry, ServerGuard, Launch, Launch) {
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, server) = open(root);
    let first = launch_spec(root, "first", "first");
    send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(first.clone()),
        },
    );
    let first = live(&registry, &first.home);
    let second = add_channel_peer(&registry, launch_spec(root, "second", "second"));
    (registry, server, first, second)
}

fn channel_peer_window<'a>(workspace: &'a Workspace, launch: &Launch) -> &'a Window {
    workspace
        .windows
        .iter()
        .find(|window| {
            matches!(&window.layout, Some(LayoutNode::Leaf { pane_id }) if pane_id == &launch.grant.pane_id)
        })
        .unwrap()
}

#[test]
fn resumed_participant_reads_retained_direct_history_but_cannot_ack_old_delivery() {
    let root = private_root();
    let root = root.path();
    let (registry, server, sender, old) = open_channel_pair(root);
    let body = "read before restart, never acknowledged";
    let receipt = agent(
        &registry,
        &sender,
        direct_post(&old.grant.launch_id, body, "before-restart"),
    );
    assert_eq!(receipt["wake_count"], 1);
    let channel_id = ChannelId(receipt["channel_id"].as_str().unwrap().into());
    let message_id = MessageId(receipt["message"]["id"].as_str().unwrap().into());
    let read = || AgentRequest::ChannelRead {
        channel_id: channel_id.clone(),
        message_id: message_id.clone(),
        offset: None,
        max_bytes: None,
    };
    let original_receiver = receiver(&registry, &old);
    let wake = subscribe(&registry, &old, &original_receiver);
    assert_eq!(wake["channel_wake"]["addressed_message_id"], message_id.0);
    assert_eq!(
        settle_wake(&registry, &old, &original_receiver, &wake)["recorded"],
        true
    );
    assert_eq!(agent(&registry, &old, read())["text"], body);

    let delivery = || {
        let db = rusqlite::Connection::open_with_flags(
            root.join("state/channels.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT launch_id,binding_revision,read_at,acknowledged_at FROM notification_work
             WHERE channel_id=?1 AND message_id=?2",
            [channel_id.as_ref(), message_id.as_ref()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .unwrap()
    };
    let before = delivery();
    assert_eq!(before.0, old.grant.launch_id);
    assert_eq!(before.1, old.binding_generation);
    assert!(before.2.is_some());
    assert_eq!(before.3, None);

    let pane = &old.grant.pane_id;
    drop(server);
    drop(registry);

    let (registry, _server) = open(root);
    let expected = registry
        .recovery_candidates()
        .unwrap()
        .into_iter()
        .find(|candidate| &candidate.pane_id == pane)
        .unwrap()
        .harness;
    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let window = channel_peer_window(&workspace, &old);
    let window_id = window.id.clone();
    let revision = window.revision;
    let spec = launch_spec(root, "second", "second");
    registry
        .spawn_inner(
            SpawnTarget::Reopen {
                pane_id: pane.clone(),
                workspace_id: workspace.id,
                window_id,
                expected_revision: revision,
            },
            spec.clone(),
            Some(recovery::ResumeAttempt {
                source: pane.clone(),
                expected,
                workspace: None,
            }),
            None,
        )
        .unwrap();
    let recovered = live(&registry, &spec.home);
    assert_ne!(recovered.grant.launch_id, old.grant.launch_id);
    assert_eq!(recovered.channel_participant, old.channel_participant);
    let retained = agent_channel(&registry, &recovered, history(&channel_id)).unwrap();
    assert_eq!(retained["messages"], json!([receipt["message"]]));
    assert_eq!(agent(&registry, &recovered, read())["text"], body);
    assert!(
        agent_result(
            &registry,
            &recovered,
            AgentRequest::ChannelAck {
                channel_id: channel_id.clone(),
                message_id: message_id.clone(),
            }
        )
        .is_err()
    );
    assert_eq!(delivery(), before);
    let recovered_receiver = receiver(&registry, &recovered);
    assert!(subscribe(&registry, &recovered, &recovered_receiver).is_null());
}

#[test]
fn exact_direct_retry_survives_target_replacement_and_early_ack_keeps_wake() {
    let root = private_root();
    let root = root.path();
    let (registry, _server, sender, target) = open_channel_pair(root);
    let body = "dispatch even if already acknowledged";
    let original = || direct_post(&target.grant.launch_id, body, "exact");
    let receipt = agent(&registry, &sender, original());
    assert_eq!(receipt["wake_count"], 1);
    let channel_id = ChannelId(receipt["channel_id"].as_str().unwrap().into());
    let message_id = MessageId(receipt["message"]["id"].as_str().unwrap().into());
    let initial_history = agent_channel(&registry, &sender, history(&channel_id)).unwrap();
    assert_eq!(initial_history["messages"], json!([receipt["message"]]));
    assert_eq!(
        agent(
            &registry,
            &target,
            AgentRequest::ChannelRead {
                channel_id: channel_id.clone(),
                message_id: message_id.clone(),
                offset: None,
                max_bytes: None,
            }
        )["text"],
        body
    );
    assert_eq!(
        agent(
            &registry,
            &target,
            AgentRequest::ChannelAck {
                channel_id: channel_id.clone(),
                message_id: message_id.clone(),
            }
        )["acknowledged"],
        true
    );
    let target_receiver = receiver(&registry, &target);
    let wake = subscribe(&registry, &target, &target_receiver);
    assert_eq!(wake["channel_wake"]["addressed_message_id"], message_id.0);
    assert_eq!(
        validate_wake(&registry, &target, &target_receiver, &wake)["current"],
        true
    );
    assert_eq!(
        settle_wake(&registry, &target, &target_receiver, &wake)["recorded"],
        true
    );

    let workspace = registry.snapshot().unwrap().workspaces.remove(0);
    let window = channel_peer_window(&workspace, &target);
    send(
        &registry,
        Operation::WindowClose {
            workspace_id: workspace.id.clone(),
            window_id: window.id.clone(),
            expected_revision: window.revision,
        },
    );
    assert_eq!(agent(&registry, &sender, original()), receipt);
    let native = &target.managed.conversation.as_ref().unwrap().id;
    let replacement = add_channel_peer(&registry, launch_spec(root, "replacement", native));
    assert_ne!(replacement.grant.launch_id, target.grant.launch_id);
    let replacement_receiver = receiver(&registry, &replacement);
    assert!(subscribe(&registry, &replacement, &replacement_receiver).is_null());
    assert_eq!(agent(&registry, &sender, original()), receipt);
    assert!(
        agent_result(
            &registry,
            &sender,
            direct_post(&target.grant.launch_id, body, "fresh")
        )
        .is_err()
    );
    assert_eq!(
        agent_channel(&registry, &sender, history(&channel_id)).unwrap(),
        initial_history
    );
    assert!(subscribe(&registry, &replacement, &replacement_receiver).is_null());

    let fresh = agent(
        &registry,
        &sender,
        direct_post(&replacement.grant.launch_id, "new target", "fresh"),
    );
    let wake = subscribe(&registry, &replacement, &replacement_receiver);
    assert_eq!(
        wake["channel_wake"]["addressed_message_id"],
        fresh["message"]["id"]
    );
    assert_eq!(wake["message_count"], 1);
    assert_eq!(
        settle_wake(&registry, &replacement, &replacement_receiver, &wake)["recorded"],
        true
    );
    assert!(subscribe(&registry, &replacement, &replacement_receiver).is_null());
}

#[test]
fn concurrent_direct_sends_share_one_private_thread_and_keep_exact_receipts() {
    let root = private_root();
    let root = root.path();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), root.join("omp")).unwrap();
    let (registry, _server) = open(root);
    let add = |name| {
        let spec = launch_spec(root, name, name);
        let workspace = send(
            &registry,
            Operation::WorkspaceCreate {
                name: None,
                initial: Some(spec.clone()),
            },
        )["workspace_id"]
            .as_str()
            .unwrap()
            .to_owned();
        (live(&registry, &spec.home), workspace)
    };
    let (a, a_workspace) = add("first");
    let snapshot = ChannelRequest::CatalogSnapshot {
        after: None,
        limit: 16,
    };
    let ChannelResponse::CatalogSnapshot {
        cursor: catalog_before,
        ..
    } = channel(&registry, None, snapshot).unwrap()
    else {
        panic!("expected catalog baseline");
    };
    let add_peer = |name| {
        let spec = launch_spec(root, name, name);
        let revision = registry
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .find(|workspace| workspace.id == a_workspace)
            .unwrap()
            .revision;
        send(
            &registry,
            Operation::WindowCreate {
                workspace_id: a_workspace.clone(),
                expected_revision: revision,
                name: None,
                spec: spec.clone(),
            },
        );
        live(&registry, &spec.home)
    };
    let b = add_peer("second");
    let outsider = add_peer("unrelated");
    let alias = &b.managed.conversation.as_ref().unwrap().id;
    let original = || direct_post(&b.grant.launch_id, "from A", "first-send");
    assert!(agent_result(&registry, &a, direct_post(alias, "from A", "first-send")).is_err());
    let start = std::sync::Barrier::new(2);
    let (a_post, b_post) = thread::scope(|scope| {
        let to_b = scope.spawn(|| {
            start.wait();
            agent(&registry, &a, original())
        });
        let to_a = scope.spawn(|| {
            start.wait();
            agent(
                &registry,
                &b,
                direct_post(&a.grant.launch_id, "from B", "first-send"),
            )
        });
        (to_b.join().unwrap(), to_a.join().unwrap())
    });
    assert_ne!(a_post["message"]["id"], b_post["message"]["id"]);
    assert_eq!(a_post["channel_id"], b_post["channel_id"]);
    assert_eq!(agent(&registry, &a, original()), a_post);
    let ChannelResponse::CatalogChanges {
        changes,
        more: false,
        ..
    } = channel(
        &registry,
        None,
        ChannelRequest::CatalogSubscribe {
            after: catalog_before,
            limit: 8,
            wait_ms: 0,
        },
    )
    .unwrap()
    else {
        panic!("expected direct send catalog changes");
    };
    assert!(!changes.is_empty());
    for change in changes {
        assert!(matches!(change,
            bkpr_protocol::channels::ChannelCatalogChange::Upsert { channel }
                if channel.direct && channel.last_sequence == 2 && !channel.unread_attention
                    && channel.workspace_id == a_workspace
        ));
    }
    assert!(
        agent_result(
            &registry,
            &a,
            direct_post(&outsider.grant.launch_id, "from A", "first-send")
        )
        .is_err()
    );

    let list = |workspace_id: &str| ChannelRequest::List {
        workspace_id: workspace_id.into(),
        after: None,
        limit: 10,
    };
    let ChannelResponse::Listed { channels, .. } =
        channel(&registry, None, list(&a_workspace)).unwrap()
    else {
        panic!("expected workspace channels")
    };
    let human_threads: Vec<_> = channels.into_iter().filter(|entry| entry.direct).collect();
    assert_eq!(human_threads.len(), 1);
    let id = &human_threads[0].id;
    assert_ne!(id.as_ref(), alias);
    let ChannelResponse::History { messages, .. } = channel(&registry, None, history(id)).unwrap()
    else {
        panic!("expected direct history");
    };
    assert_eq!(messages.len(), 2);
    for (receipt, body) in [(&a_post, "from A"), (&b_post, "from B")] {
        assert!(
            messages
                .iter()
                .any(|m| m.message.id.0 == receipt["message"]["id"] && m.text == body)
        );
    }
    for caller in [&a, &b] {
        let listed = agent_channel(&registry, caller, list(&a_workspace)).unwrap();
        assert!(
            listed["channels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == id.0)
        );
        assert!(agent_channel(&registry, caller, history(id)).is_ok());
    }
    let listed = agent_channel(&registry, &outsider, list(&a_workspace)).unwrap();
    assert!(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != id.0)
    );
    assert!(agent_channel(&registry, &outsider, history(id)).is_err());
    assert!(
        agent_channel(
            &registry,
            &outsider,
            post(id, "foreign post", PostIntent::Note)
        )
        .is_err()
    );
    assert!(
        channel(
            &registry,
            None,
            post(id, "not a human channel", PostIntent::Note)
        )
        .is_err()
    );

    let receiving = receiver(&registry, &b);
    let wake = subscribe(&registry, &b, &receiving);
    assert_eq!(
        wake["channel_wake"]["addressed_message_id"],
        a_post["message"]["id"]
    );
    assert_eq!(wake["message_count"], 1);
    assert_eq!(
        validate_wake(&registry, &b, &receiving, &wake)["current"],
        true
    );
    assert!(wake.get("text").is_none());
    assert_eq!(
        settle_wake(&registry, &b, &receiving, &wake)["recorded"],
        true
    );
    assert_eq!(agent(&registry, &a, original()), a_post);
    assert!(subscribe(&registry, &b, &receiving).is_null());

    // Failure inserting mandatory wake work rolls back both the thread and post.
    let disk = rusqlite::Connection::open(root.join("state/channels.sqlite3")).unwrap();
    disk.execute_batch(
        "CREATE TRIGGER reject_agent_update BEFORE INSERT ON notification_work
         BEGIN SELECT RAISE(ABORT,'fixture write rejected'); END;",
    )
    .unwrap();
    assert!(
        agent_result(
            &registry,
            &a,
            direct_post(
                &outsider.grant.launch_id,
                "must not enter history",
                "storage-failure"
            ),
        )
        .is_err()
    );
    disk.execute_batch("DROP TRIGGER reject_agent_update")
        .unwrap();
    let count: usize = disk
        .query_row("SELECT count(*) FROM direct_channels", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let count: usize = disk
        .query_row("SELECT count(*) FROM posts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let ChannelRequest::Post {
        channel_id,
        post: mut reply,
    } = post(id, "reply on thread", PostIntent::Note)
    else {
        unreachable!()
    };
    reply.reply_to = Some(channels::MessageId(
        a_post["message"]["id"].as_str().unwrap().into(),
    ));
    let receipt = agent_channel(
        &registry,
        &b,
        ChannelRequest::Post {
            channel_id,
            post: reply,
        },
    )
    .unwrap();
    assert_eq!(receipt["channel_id"], id.0);
    assert_eq!(receipt["wake_count"], 1);
}

fn direct_post(launch_id: &str, text: &str, key: &str) -> AgentRequest {
    AgentRequest::ChannelDirectPost {
        launch_id: launch_id.into(),
        post: Box::new(ChannelPost {
            idempotency_key: key.into(),
            text: text.into(),
            intent: PostIntent::Note,
            mentions: Vec::new(),
            reply_to: None,
            attachments: Vec::new(),
            source: None,
        }),
    }
}
