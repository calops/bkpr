use super::*;
use crate::registry::sessions::ClientLease;
use bkpr_protocol::roles::{
    APPROVAL_TIMEOUT_MS, RoleApproval, RoleEventKind, RoleGrant, RoleGrantId, RoleOrigin,
    RoleRequest, RoleResponse, WorkspaceRole,
};

const ROLE: WorkspaceRole = WorkspaceRole::Coordinator;

fn user(registry: &Registry, client: &ClientLease, request: RoleRequest) -> Result<RoleResponse> {
    let Payload::Roles(reply) = registry.request_roles(client, request, &|| true)? else {
        panic!()
    };
    Ok(reply)
}

fn activity(registry: &Registry, client: &ClientLease, eligible: bool) {
    assert_eq!(
        user(registry, client, RoleRequest::Activity { eligible }).unwrap(),
        RoleResponse::Activity {}
    );
}

fn snapshot(registry: &Registry, client: &ClientLease) -> (Vec<RoleGrant>, Vec<RoleApproval>) {
    let RoleResponse::Snapshot {
        grants, approvals, ..
    } = user(registry, client, RoleRequest::Snapshot {}).unwrap()
    else {
        panic!()
    };
    (grants, approvals)
}

fn approval(prompt: &RoleApproval, approve: bool) -> RoleRequest {
    RoleRequest::Decide {
        request_id: prompt.id.clone(),
        requester: prompt.requester.clone(),
        expected_holder: prompt.prior_holder.as_ref().map(|g| g.id.clone()),
        approve,
    }
}

fn event(stream: &mut UnixStream) -> Option<RoleEventKind> {
    let bytes = wire::read_frame(stream, wire::Kind::ClientEvent, wire::MAX_CLIENT_EVENT).unwrap();
    wire::decode_client_event(&bytes)
        .unwrap()
        .role_event()
        .unwrap()
        .map(|event| event.kind)
}

fn binding(launch: &Launch) -> ChannelMentionBinding {
    ChannelMentionBinding {
        participant_id: launch.channel_participant.clone().unwrap(),
        launch_id: launch.grant.launch_id.clone(),
        binding_generation: launch.binding_generation,
    }
}

fn set(workspace: &str, launch: Option<&Launch>) -> RoleRequest {
    RoleRequest::Set {
        workspace_id: workspace.into(),
        role: ROLE,
        target: launch.map(binding),
    }
}

fn grant(reply: RoleResponse) -> RoleGrant {
    let RoleResponse::Updated { grant: Some(grant) } = reply else {
        panic!("{reply:?}")
    };
    grant
}

fn status(registry: &Registry, launch: &Launch) -> Value {
    agent(registry, launch, AgentRequest::RoleStatus { role: ROLE })
}

fn read_agent(
    registry: &Registry,
    launch: &Launch,
    channel: &ChannelId,
    post: &Value,
) -> Result<Value> {
    agent_result(
        registry,
        launch,
        AgentRequest::ChannelRead {
            channel_id: channel.clone(),
            message_id: MessageId(post["message"]["id"].as_str().unwrap().into()),
            offset: None,
            max_bytes: None,
        },
    )
}

#[test]
fn user_invites_add_exact_live_members_without_replacing_concurrent_edits() {
    use bkpr_protocol::channels::ChannelChangeKind;

    let root = private_root();
    let (registry, _server, first, second) = open_channel_pair(root.path());
    let workspace = registry.snapshot().unwrap().workspaces.remove(0).id;
    let create = |workspace: &str, name: &str, membership| {
        let ChannelResponse::Channel { channel } = channel(
            &registry,
            None,
            ChannelRequest::Create {
                workspace_id: workspace.into(),
                name: name.into(),
                membership,
                members: vec![],
                settings: Default::default(),
            },
        )
        .unwrap() else {
            panic!("expected created channel");
        };
        channel.id
    };
    let restricted = create(&workspace, "invite test", MembershipMode::Explicit);
    let wide = create(&workspace, "wide test", MembershipMode::WorkspaceWide);
    let invite = |channel_id: &ChannelId, target| ChannelRequest::InviteUser {
        channel_id: channel_id.clone(),
        target,
    };
    let request = |request| channel(&registry, None, request);
    let first_binding = binding(&first);
    let second_binding = binding(&second);
    let ChannelResponse::History { cursor, .. } = request(history(&restricted)).unwrap() else {
        panic!("expected change baseline");
    };
    assert!(
        agent_channel(
            &registry,
            &first,
            invite(&restricted, second_binding.clone())
        )
        .is_err()
    );
    assert!(agent_channel(&registry, &second, history(&restricted)).is_err());
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            request(invite(&restricted, first_binding.clone())).unwrap()
        });
        start.wait();
        let second = request(invite(&restricted, second_binding.clone())).unwrap();
        assert_eq!(first.join().unwrap(), second);
    });
    assert_eq!(
        request(invite(&restricted, second_binding.clone())).unwrap(),
        request(invite(&restricted, first_binding.clone())).unwrap()
    );
    let mut expected = [
        first_binding.participant_id.clone(),
        second_binding.participant_id.clone(),
    ];
    expected.sort_unstable();
    assert_eq!(
        request(ChannelRequest::ListExplicitMembers {
            channel_id: restricted.clone(),
            after: None,
            limit: 8,
        })
        .unwrap(),
        ChannelResponse::ExplicitMembers {
            members: expected.into(),
            next_after: None
        }
    );
    assert!(agent_channel(&registry, &second, history(&restricted)).is_ok());

    let stale = ChannelMentionBinding {
        binding_generation: second_binding.binding_generation + 1,
        ..second_binding.clone()
    };
    assert!(request(invite(&restricted, stale)).is_err());
    assert!(request(invite(&wide, second_binding.clone())).is_err());
    let (client, _stream) = registry.test_role_client();
    activity(&registry, &client, true);
    let tenure = grant(user(&registry, &client, set(&workspace, Some(&first))).unwrap());
    assert!(request(invite(&tenure.channel_id, second_binding.clone())).is_err());
    let dm = agent(
        &registry,
        &first,
        direct_post(&second.grant.launch_id, "private", "invite-test"),
    );
    let dm = ChannelId(dm["channel_id"].as_str().unwrap().into());
    assert!(request(invite(&dm, second_binding.clone())).is_err());
    let foreign_spec = launch_spec(root.path(), "foreign", "foreign");
    let foreign = send(
        &registry,
        Operation::WorkspaceCreate {
            name: None,
            initial: Some(foreign_spec),
        },
    );
    let foreign = create(
        foreign["workspace_id"].as_str().unwrap(),
        "foreign test",
        MembershipMode::Explicit,
    );
    assert!(request(invite(&foreign, second_binding)).is_err());

    let ChannelResponse::Changes {
        changes,
        more: false,
        ..
    } = request(ChannelRequest::Subscribe {
        channel_id: restricted,
        after: Some(cursor),
        limit: 8,
        wait_ms: 0,
    })
    .unwrap()
    else {
        panic!("expected membership changes");
    };
    assert_eq!(
        changes.iter().map(|change| change.kind).collect::<Vec<_>>(),
        [ChannelChangeKind::Membership, ChannelChangeKind::Membership]
    );
}

#[test]
fn workspace_role_approval_fences_and_native_wakes() {
    let root = private_root();
    let (registry, _server, first, second) = open_channel_pair(root.path());
    let workspace = registry.snapshot().unwrap().workspaces.remove(0).id;
    let request = AgentRequest::RoleRequest { role: ROLE };
    let initial = agent(&registry, &first, request.clone());
    assert_eq!(initial["state"], "pending");
    assert_eq!(agent(&registry, &first, request.clone()), initial);
    registry.maintain_roles_at(Instant::now() + Duration::from_millis(APPROVAL_TIMEOUT_MS));
    assert_eq!(status(&registry, &first)["request"]["state"], "expired");

    let (a, mut a_stream) = registry.test_role_client();
    let (b, mut b_stream) = registry.test_role_client();
    let (read_only, _read_only_stream) = registry.test_role_client();
    activity(&registry, &a, true);
    activity(&registry, &b, true);
    activity(&registry, &read_only, false);
    assert!(user(&registry, &read_only, set(&workspace, Some(&first))).is_err());
    let pending = agent(&registry, &first, request.clone());
    assert_ne!(pending["request_id"], initial["request_id"]);
    assert_eq!(event(&mut b_stream), Some(RoleEventKind::Approvals));
    assert!(snapshot(&registry, &a).1.is_empty());
    assert!(snapshot(&registry, &read_only).1.is_empty());
    let prompt = snapshot(&registry, &b).1.pop().unwrap();
    let deny = approval(&prompt, false);
    assert!(user(&registry, &a, deny.clone()).is_err());
    user(&registry, &b, deny.clone()).unwrap();
    assert!(user(&registry, &b, deny).is_err());
    assert_eq!(status(&registry, &first)["request"]["state"], "denied");

    agent(&registry, &first, request.clone());
    let prompt = snapshot(&registry, &b).1.pop().unwrap();
    drop(b_stream);
    registry.detach_client(&b, "disconnected");
    assert_eq!(event(&mut a_stream), Some(RoleEventKind::Approvals));
    assert_eq!(
        snapshot(&registry, &a).1.as_slice(),
        std::slice::from_ref(&prompt)
    );
    assert!(user(&registry, &b, approval(&prompt, true)).is_err());

    let first_receiver = receiver(&registry, &first);
    let second_receiver = receiver(&registry, &second);
    let incumbent = grant(user(&registry, &a, set(&workspace, Some(&second))).unwrap());
    assert_eq!(incumbent.origin, RoleOrigin::Command);
    let wake = subscribe(&registry, &second, &second_receiver);
    assert_eq!(wake["role_wake"]["grant_id"], incumbent.id.0);
    assert_eq!(wake["role_wake"]["change"], "granted");
    assert_eq!(
        validate_wake(&registry, &second, &second_receiver, &wake)["current"],
        true
    );
    assert_eq!(
        settle_wake(&registry, &second, &second_receiver, &wake)["recorded"],
        true
    );
    assert!(user(&registry, &a, approval(&prompt, true)).is_err());
    let prompt = snapshot(&registry, &a).1.pop().unwrap();
    assert_eq!(prompt.prior_holder.as_ref().unwrap().id, incumbent.id);

    let database = rusqlite::Connection::open(root.path().join("state/channels.sqlite3")).unwrap();
    database.execute_batch("CREATE TRIGGER fail_role_prepare BEFORE INSERT ON role_threads BEGIN SELECT RAISE(ABORT,'durable preparation failed'); END;").unwrap();
    assert!(user(&registry, &a, approval(&prompt, true)).is_err());
    assert_eq!(snapshot(&registry, &a).0, vec![incumbent.clone()]);
    assert_eq!(status(&registry, &first)["request"]["state"], "pending");
    assert!(subscribe(&registry, &first, &first_receiver).is_null());
    database
        .execute_batch("DROP TRIGGER fail_role_prepare")
        .unwrap();

    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| user(&registry, &a, approval(&prompt, true)));
        let two = scope.spawn(|| user(&registry, &a, approval(&prompt, true)));
        (one.join().unwrap(), two.join().unwrap())
    });
    let current = match (one, two) {
        (Ok(result), Err(_)) | (Err(_), Ok(result)) => grant(result),
        (a, b) => panic!("racing decisions returned {a:?} and {b:?}"),
    };
    assert_eq!(current.origin, RoleOrigin::Approval);
    assert_ne!(current.channel_id, incumbent.channel_id);
    assert_eq!(status(&registry, &first)["request"]["state"], "approved");
    assert!(status(&registry, &second)["grant"].is_null());
    for (launch, receiver, change, id) in [
        (&first, &first_receiver, "granted", &current.id),
        (&second, &second_receiver, "revoked", &incumbent.id),
    ] {
        let next = subscribe(&registry, launch, receiver);
        assert_eq!(next["role_wake"]["grant_id"], id.0);
        assert_eq!(next["role_wake"]["change"], change);
        assert!(next.get("channel_wake").is_none());
        assert_eq!(
            validate_wake(&registry, launch, receiver, &next)["current"],
            true
        );
        assert_eq!(
            settle_wake(&registry, launch, receiver, &next)["recorded"],
            true
        );
        assert_eq!(
            validate_wake(&registry, launch, receiver, &next)["current"],
            false
        );
    }
}

#[test]
fn workspace_role_delegation_does_not_replay_or_share_private_history() {
    let root = private_root();
    let (registry, server, first, second) = open_channel_pair(root.path());
    let (client, _stream) = registry.test_role_client();
    activity(&registry, &client, true);
    let workspace = registry.snapshot().unwrap().workspaces.remove(0).id;
    let ChannelResponse::Listed { channels, .. } = channel(
        &registry,
        None,
        ChannelRequest::List {
            workspace_id: workspace.clone(),
            after: None,
            limit: 10,
        },
    )
    .unwrap() else {
        panic!()
    };
    let desk = &channels.iter().find(|c| c.name == "#desk").unwrap().id;
    let before = agent_channel(
        &registry,
        &first,
        post(desk, "before grant", PostIntent::Note),
    )
    .unwrap();
    assert!(before["message"].get("role").is_none());
    let first_grant = grant(user(&registry, &client, set(&workspace, Some(&first))).unwrap());
    let public = agent_channel(
        &registry,
        &first,
        post(desk, "relay in tenure", PostIntent::Note),
    )
    .unwrap();
    let secret = agent_channel(
        &registry,
        &first,
        post(
            &first_grant.channel_id,
            "first tenure only",
            PostIntent::Note,
        ),
    )
    .unwrap();
    assert_eq!(public["message"]["role"]["grant_id"], first_grant.id.0);
    assert_eq!(
        read_agent(&registry, &second, desk, &public).unwrap()["current_delegation"],
        first_grant.id.0
    );
    assert!(read_agent(&registry, &second, &first_grant.channel_id, &secret).is_err());
    assert!(read_agent(&registry, &second, desk, &before).unwrap()["current_delegation"].is_null());

    let release = |id: &RoleGrantId| AgentRequest::RoleRelease {
        role: ROLE,
        grant_id: id.clone(),
    };
    assert!(agent_result(&registry, &second, release(&first_grant.id)).is_err());
    assert_eq!(
        agent(&registry, &first, release(&first_grant.id))["released"],
        true
    );
    let obsolete = read_agent(&registry, &second, desk, &public).unwrap();
    assert_eq!(obsolete["message"]["role"]["grant_id"], first_grant.id.0);
    assert!(obsolete["current_delegation"].is_null());
    assert!(read_agent(&registry, &first, &first_grant.channel_id, &secret).is_err());
    let second_grant = grant(user(&registry, &client, set(&workspace, Some(&first))).unwrap());
    assert_ne!(second_grant.id, first_grant.id);
    assert_ne!(second_grant.channel_id, first_grant.channel_id);
    assert!(agent_result(&registry, &first, release(&first_grant.id)).is_err());
    assert!(read_agent(&registry, &second, desk, &public).unwrap()["current_delegation"].is_null());
    assert!(read_agent(&registry, &first, &first_grant.channel_id, &secret).is_err());
    assert!(!status(&registry, &first)["grant"].is_null());

    let retired_post = agent_channel(
        &registry,
        &first,
        post(desk, "before retirement", PostIntent::Note),
    )
    .unwrap();
    assert_eq!(
        read_agent(&registry, &second, desk, &retired_post).unwrap()["current_delegation"],
        second_grant.id.0
    );
    launch_request(
        &registry,
        &first,
        Operation::HarnessEvent {
            launch_id: first.grant.launch_id.clone(),
            token: first.grant.token.clone(),
            event: CaptureEvent::End,
        },
    )
    .unwrap();
    assert!(snapshot(&registry, &client).0.is_empty());
    assert!(
        read_agent(&registry, &second, desk, &retired_post).unwrap()["current_delegation"]
            .is_null()
    );
    user(&registry, &client, set(&workspace, Some(&second))).unwrap();

    drop(server);
    drop(registry);
    let (reopened, _server) = open(root.path());
    let (client, _stream) = reopened.test_role_client();
    assert!(snapshot(&reopened, &client).0.is_empty());
    let ChannelResponse::History { messages, .. } =
        channel(&reopened, None, history(&first_grant.channel_id)).unwrap()
    else {
        panic!()
    };
    assert_eq!(messages[0].text, "first tenure only");
    assert!(messages[0].current_delegation.is_none());
}
