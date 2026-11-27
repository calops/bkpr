use super::*;
use bkpr_protocol::roles::{
    RoleAttribution, RoleGrant, RoleGrantId, RoleOrigin, RoleThread, WorkspaceRole,
};
use serde_json::json;
use std::sync::Arc;

fn grant(actor: &Participant, launch: &str, tenure: &str) -> RoleGrant {
    RoleGrant {
        id: RoleGrantId(tenure.into()),
        workspace_id: WORKSPACE.into(),
        role: WorkspaceRole::Coordinator,
        holder: ChannelMentionBinding {
            participant_id: actor.id.clone(),
            launch_id: launch.into(),
            binding_generation: 1,
        },
        display_name: actor.display_name.clone(),
        origin: RoleOrigin::Command,
        granted_at_ms: 1,
        channel_id: ChannelId(format!("channel-{tenure}")),
    }
}

fn install(f: &Fixture, grant: &RoleGrant) -> Result<Option<RoleGrant>> {
    f.engine
        .with_delivery_store(&|| false, &|_| true, |store| store.set_role(grant.clone()))
}

fn fetch(channel_id: &ChannelId, message: &MessageSummary) -> ChannelRequest {
    ChannelRequest::Read {
        channel_id: channel_id.clone(),
        message_id: message.id.clone(),
    }
}

fn roster(f: &Fixture, channel: &ChannelId) -> Vec<ChannelRosterEntry> {
    let ChannelResponse::UserRoster {
        participants,
        next_after: None,
    } = f
        .request(ChannelRequest::UserRoster {
            workspace_id: WORKSPACE.into(),
            channel_id: Some(channel.clone()),
            after: None,
            limit: 10,
        })
        .unwrap()
    else {
        panic!("user roster reply");
    };
    participants
}

fn claim_for(
    f: &Fixture,
    recipient: &Participant,
    claim: &ChannelClaim<'_>,
) -> Option<ChannelWake> {
    f.engine
        .with_store(|store| {
            store.claim_wakes(
                claim,
                Some(WORKSPACE),
                &|workspace| f.authority(recipient, workspace),
                || Ok(()),
            )
        })
        .unwrap()
}

#[test]
fn each_private_tenure_follows_its_exact_launch_and_then_is_user_only() {
    let f = Fixture::new();
    f.engine
        .set_launch_liveness(|id| matches!(id, "launch-a" | "launch-b"));
    for actor in [&f.user, &f.a] {
        for name in ["#coordinator", "Coordinator", "  #COORDINATOR  "] {
            let denied = f.as_actor(
                actor,
                ChannelRequest::Create {
                    workspace_id: WORKSPACE.into(),
                    name: name.into(),
                    membership: MembershipMode::WorkspaceWide,
                    members: Vec::new(),
                    settings: quiet(),
                },
            );
            assert!(denied.unwrap_err().to_string().contains("reserved"));
        }
    }

    let a = grant(&f.a, "launch-a", "first");
    let b = grant(&f.b, "launch-b", "second");
    let old = &a.channel_id;
    assert_eq!(install(&f, &a).unwrap(), None);
    let role = f
        .list(&f.a)
        .into_iter()
        .find(|channel| channel.id == *old)
        .unwrap();
    assert_eq!(role.name, "#coordinator");
    assert_eq!(
        role.role_thread,
        Some(RoleThread {
            role: a.role,
            archived: false
        })
    );
    assert!(f.list(&f.b).iter().all(|channel| channel.id != *old));
    assert!(
        f.as_actor(&f.b, messages(old, None, 1))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    assert!(
        f.request(ChannelRequest::SetMembership {
            channel_id: old.clone(),
            membership: MembershipMode::WorkspaceWide,
            members: Vec::new(),
        })
        .is_err()
    );
    assert!(
        f.engine
            .with_store(|store| Ok(store.connection.query_row(
                "SELECT count(*) FROM members WHERE channel_id=?1",
                [old.as_ref()],
                |r| r.get::<_, u64>(0),
            )?))
            .unwrap()
            == 0
    );

    let entries = roster(&f, old);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].participant.id, f.a.id);
    assert_eq!(
        entries[0].role,
        Some(RoleAttribution {
            role: a.role,
            grant_id: a.id.clone()
        })
    );
    assert_eq!(entries[1].participant.id, f.user.id);
    let read_with_launch = |launch: &str, actor: &Participant, request| {
        f.engine.request(
            request,
            |workspace| {
                let mut authority = f.authority(actor, workspace)?;
                authority.launch_id = Some(launch.into());
                Ok(authority)
            },
            |_| true,
            || false,
        )
    };
    assert!(
        read_with_launch("recycled-launch", &f.a, messages(old, None, 1))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    let rebound = Participant {
        id: ParticipantId("rebound-a".into()),
        ..f.a.clone()
    };
    let written =
        posted(read_with_launch("launch-a", &rebound, send(old, post("rebound"))).unwrap());
    assert_eq!(
        written.role.as_ref().map(|role| &role.grant_id),
        Some(&a.id)
    );

    let mut user_post = post("private-user");
    user_post.mentions.push(f.a.id.clone());
    let delivered = posted(
        f.request(send_user(old, user_post, vec![a.holder.clone()]))
            .unwrap(),
    );
    assert!(delivered.role.is_none());
    let claim = f.wake_claim("private-role-wake", 1);
    assert_eq!(claim_for(&f, &f.a, &claim).unwrap().channel_id, *old);
    assert!(f.wake_lookup(&claim, Some(64)).is_ok());

    assert_eq!(install(&f, &b).unwrap(), Some(a.clone()));
    assert!(
        f.wake_lookup(&claim, Some(64))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    for agent in [&f.a, &f.b] {
        assert!(
            f.as_actor(agent, fetch(old, &written))
                .unwrap_err()
                .is::<ChannelAccessLost>()
        );
        assert!(f.list(agent).iter().all(|channel| channel.id != *old));
    }
    let archived = f
        .list(&f.user)
        .into_iter()
        .find(|channel| channel.id == *old)
        .unwrap();
    assert!(archived.role_thread.unwrap().archived);
    let ChannelResponse::Message { message } = f.request(fetch(old, &written)).unwrap() else {
        panic!("archived exact read");
    };
    assert_eq!(message.message, written);
    assert_eq!(message.current_delegation, None);
    assert!(
        f.request(send_user(old, post("archived-compose"), Vec::new()))
            .is_err()
    );
    let entries = roster(&f, old);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].participant.id, f.user.id);
    assert!(
        f.as_actor(&f.b, send(&b.channel_id, post("second-tenure")))
            .is_ok()
    );
}

#[test]
fn role_attribution_outlives_delegation_without_widening_wake_source_access() {
    let f = Fixture::new();
    let a_live = Arc::new(AtomicBool::new(true));
    let liveness = a_live.clone();
    f.engine
        .set_launch_liveness(move |id| id == "launch-a" && liveness.load(Ordering::Relaxed));
    let relay = f.wide(ChannelSettings {
        agent_wake: WakePolicy::AddressedOnly,
        ..quiet()
    });
    let a = grant(&f.a, "launch-a", "live-grant");
    let before = posted(
        f.as_actor(&f.a, send(&relay, post("before-grant")))
            .unwrap(),
    );
    assert!(before.role.is_none());
    install(&f, &a).unwrap();
    let private = posted(
        f.as_actor(&f.a, send(&a.channel_id, post("private-source")))
            .unwrap(),
    );
    let relayed = |text: &str, source_channel: &ChannelId, source: &MessageSummary| {
        posted(
            f.as_actor(
                &f.a,
                send(
                    &relay,
                    ChannelPost {
                        mentions: vec![f.b.id.clone()],
                        source: Some(Box::new(ChannelSource {
                            channel_id: source_channel.clone(),
                            message_id: source.id.clone(),
                        })),
                        ..post(text)
                    },
                ),
            )
            .unwrap(),
        )
    };
    let public = relayed("first relay", &a.channel_id, &private);
    let claim = |batch_id, generation| ChannelClaim {
        recipient: ChannelRecipient {
            launch: "launch-b",
            binding: 1,
            participant: f.b.id.as_ref(),
        },
        batch_id,
        receiver_id: "recipient-b",
        generation,
        deadline: u64::MAX / 2,
    };
    let lookup = |claim: &ChannelClaim<'_>, source: bool| {
        f.engine
            .with_store(|store| {
                store.channel_wake_lookup(
                    claim.recipient,
                    claim.batch_id.to_owned(),
                    Some(1024),
                    Some(WORKSPACE),
                    source.then_some(WORKSPACE),
                    ChannelWakeAccess {
                        admit: &|workspace| f.authority(&f.b, workspace),
                        author_mention: &|_, _| Ok(None),
                        connected: &|| true,
                    },
                )
            })
            .unwrap()
    };
    let first = claim("first", 1);
    claim_for(&f, &f.b, &first).unwrap();
    let expanded = lookup(&first, true);
    assert_eq!(expanded["post"]["message"]["role"]["grant_id"], json!(a.id));
    assert_eq!(expanded["post"]["current_delegation"], json!(a.id));
    assert!(expanded["post"].get("source").is_none());

    let second_post = relayed("second relay", &relay, &public);
    let second = claim("second", 2);
    claim_for(&f, &f.b, &second).unwrap();
    assert!(lookup(&second, false)["post"].get("source").is_none());
    let with_source = lookup(&second, true);
    assert_eq!(
        with_source["post"]["source"]["current_delegation"],
        json!(a.id)
    );
    assert_eq!(
        with_source["post"]["source"]["message"]["id"],
        json!(public.id)
    );
    assert!(
        with_source["post"]["source"]["message"]
            .get("source")
            .is_none()
    );

    let exact = |workspace| {
        f.engine
            .with_store(|store| {
                store.compact_message(
                    fetch(&relay, &second_post),
                    &|id| f.authority(&f.b, id),
                    0,
                    1024,
                    Some(workspace),
                )
            })
            .unwrap()
    };
    assert_eq!(exact(WORKSPACE)["current_delegation"], json!(a.id));
    assert!(exact(OTHER_WORKSPACE).get("current_delegation").is_none());
    let ChannelResponse::Message { message } =
        f.as_actor(&f.b, fetch(&relay, &second_post)).unwrap()
    else {
        panic!("typed exact read");
    };
    assert_eq!(message.current_delegation, Some(a.id.clone()));

    a_live.store(false, Ordering::Relaxed);
    assert!(exact(WORKSPACE).get("current_delegation").is_none());
    let retired_batch = lookup(&second, true);
    assert!(retired_batch["post"].get("current_delegation").is_none());
    assert!(
        retired_batch["post"]["source"]
            .get("current_delegation")
            .is_none()
    );
    assert_eq!(
        retired_batch["post"]["message"]["role"]["grant_id"],
        json!(a.id)
    );
    f.engine
        .with_store(|store| {
            assert_eq!(store.remove_role(WORKSPACE), Some(a.clone()));
            Ok(())
        })
        .unwrap();
    a_live.store(true, Ordering::Relaxed);
    let next = grant(&f.a, "launch-a", "new-grant");
    assert_eq!(install(&f, &next).unwrap(), None);
    assert!(exact(WORKSPACE).get("current_delegation").is_none());
    let fresh = posted(f.as_actor(&f.a, send(&relay, post("next-tenure"))).unwrap());
    assert_eq!(fresh.role.unwrap().grant_id, next.id);
}

#[test]
fn archive_catalog_survives_recovery_and_failed_preparation_cannot_replace_a_grant() {
    let f = Fixture::new();
    f.engine.set_launch_liveness(|_| true);
    let a = grant(&f.a, "launch-a", "original");
    let b = grant(&f.b, "launch-b", "replacement");
    let catalog = |f: &Fixture, read| {
        f.engine.catalog_request(
            read,
            f.user.id.as_ref(),
            20,
            0,
            || Ok([WORKSPACE.to_owned()].into()),
            || false,
        )
    };
    let changes = |f: &Fixture, since| match catalog(f, CatalogRead::Changes(since)).unwrap() {
        ChannelResponse::CatalogChanges {
            cursor,
            changes,
            more: false,
        } => (cursor, changes),
        _ => panic!("catalog changes"),
    };
    let ChannelResponse::CatalogSnapshot { cursor, .. } =
        catalog(&f, CatalogRead::Snapshot(None)).unwrap()
    else {
        panic!("catalog snapshot");
    };
    install(&f, &a).unwrap();
    let (cursor, created) = changes(&f, &cursor);
    assert!(
        matches!(created.as_slice(), [ChannelCatalogChange::Upsert { channel }]
        if channel.id == a.channel_id && !channel.role_thread.unwrap().archived)
    );

    f.sql("CREATE TRIGGER fail_prepare BEFORE INSERT ON role_threads BEGIN SELECT RAISE(ABORT, 'broken role prep'); END;");
    assert!(
        install(&f, &b)
            .unwrap_err()
            .to_string()
            .contains("broken role prep")
    );
    assert!(changes(&f, &cursor).1.is_empty());
    f.sql("DROP TRIGGER fail_prepare;");
    assert_eq!(
        f.engine
            .with_store(|store| Ok(store.role_grants().cloned().collect::<Vec<_>>()))
            .unwrap(),
        vec![a.clone()]
    );

    f.sql("CREATE TRIGGER fail_archive BEFORE UPDATE ON role_threads BEGIN SELECT RAISE(ABORT, 'broken archive'); END;");
    f.engine
        .with_store(|store| {
            assert_eq!(store.remove_role(WORKSPACE), Some(a.clone()));
            assert_eq!(store.role_grant(WORKSPACE), None);
            Ok(())
        })
        .unwrap();
    assert!(
        f.as_actor(&f.a, messages(&a.channel_id, None, 1))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    assert!(
        f.list(&f.user)
            .into_iter()
            .find(|c| c.id == a.channel_id)
            .unwrap()
            .role_thread
            .unwrap()
            .archived
    );
    assert!(
        catalog(&f, CatalogRead::Changes(&cursor))
            .unwrap_err()
            .to_string()
            .contains("broken archive")
    );
    f.sql("DROP TRIGGER fail_archive;");
    let (cursor, archived) = changes(&f, &cursor);
    assert!(
        matches!(archived.as_slice(), [ChannelCatalogChange::Upsert { channel }]
        if channel.id == a.channel_id && channel.role_thread.unwrap().archived)
    );

    install(&f, &b).unwrap();
    let body = posted(
        f.as_actor(&f.b, send(&b.channel_id, post("before-recovery")))
            .unwrap(),
    );
    let (cursor, _) = changes(&f, &cursor);
    let f = f.reopen();
    f.engine.set_launch_liveness(|_| true);
    assert!(
        f.engine
            .with_store(|store| Ok(store.role_grants().next().is_none()))
            .unwrap()
    );
    assert!(
        f.as_actor(&f.b, fetch(&b.channel_id, &body))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    let ChannelResponse::Message { message } = f.request(fetch(&b.channel_id, &body)).unwrap()
    else {
        panic!("recovered user's archive");
    };
    assert_eq!(message.message, body);
    assert_eq!(message.current_delegation, None);
    let (_, event) = changes(&f, &cursor);
    assert!(
        matches!(event.as_slice(), [ChannelCatalogChange::Upsert { channel }]
        if channel.id == b.channel_id && channel.role_thread.unwrap().archived)
    );
}
