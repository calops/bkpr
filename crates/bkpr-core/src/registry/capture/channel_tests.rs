use super::*;
use bkpr_protocol::channels::{
    ChannelId, ChannelPost, ChannelRequest, ChannelResponse, MembershipMode, PostIntent,
};

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

fn agent(registry: &Registry, launch: &Launch, request: AgentRequest) -> Value {
    let peer = Peer {
        pid: launch.root.pid,
        uid: unsafe { libc::geteuid() },
        birth: launch.root.identity.clone(),
    };
    registry
        .request_authenticated(
            Request::new(Operation::Agent {
                launch_id: launch.grant.launch_id.clone(),
                token: launch.grant.token.clone(),
                guard: None,
                request,
            }),
            &peer,
        )
        .unwrap()
        .into_json()
        .unwrap()
}

fn channel(
    registry: &Registry,
    caller: Option<&Launch>,
    request: ChannelRequest,
) -> Result<ChannelResponse> {
    let Payload::Channels(response) = registry.request_channels_for(request, caller, &|| true)?
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
    assert!(channel(&registry, Some(&recovered), history(&removed)).is_err());
    assert!(channel(&registry, Some(&old), history(&kept)).is_err());
    let inbox = agent(&registry, &recovered, read_inbox());
    assert_eq!(inbox["messages"], json!([]));
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
