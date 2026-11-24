use super::{
    CatalogRead, ChannelAccessLost, ChannelAuthority, ChannelClaim, ChannelEngine,
    ChannelRecipient, ChannelWakeAccess, MAX_SUBSCRIBERS, WorkspaceParticipant,
    store::{MAX_ARCHIVE_CANDIDATES, MAX_STAGED_BYTES, MAX_STAGING_RECLAIM},
};
use anyhow::{Result, ensure};
use bkpr_protocol::channels::*;
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

const WORKSPACE: &str = "workspace-one";
const OTHER_WORKSPACE: &str = "workspace-two";

struct Fixture {
    root: tempfile::TempDir,
    engine: ChannelEngine,
    user: Participant,
    a: Participant,
    b: Participant,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let engine = ChannelEngine::new(root.path().to_owned());
        let participant = |id: &str, kind| Participant {
            id: ParticipantId(id.into()),
            kind,
            display_name: if kind == ParticipantKind::Agent {
                "Coordinator"
            } else {
                "User"
            }
            .into(),
        };
        Self {
            root,
            engine,
            user: participant("session-user", ParticipantKind::User),
            a: participant("participant-a", ParticipantKind::Agent),
            b: participant("participant-b", ParticipantKind::Agent),
        }
    }

    fn reopen(mut self) -> Self {
        self.engine = ChannelEngine::new(self.root.path().to_owned());
        self
    }

    fn staged_bytes(&self) -> u64 {
        let (tracked, actual) = self
            .engine
            .with_store(|store| {
                Ok(store.connection.query_row(
                    "SELECT unposted_bytes,(SELECT COALESCE(sum(length(a.bytes)),0)
                     FROM staged_attachments s JOIN attachments a ON a.id=s.id)
                     FROM attachment_budget WHERE singleton=1",
                    [],
                    |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(tracked, actual);
        tracked
    }

    fn authority(&self, actor: &Participant, workspace: &str) -> Result<ChannelAuthority> {
        ensure!(
            workspace == WORKSPACE || actor.kind == ParticipantKind::User,
            ChannelAccessLost
        );
        let participants = if workspace == WORKSPACE {
            vec![
                WorkspaceParticipant {
                    actor: self.user.clone(),
                    launch_id: None,
                    binding_revision: 0,
                },
                WorkspaceParticipant {
                    actor: self.a.clone(),
                    launch_id: Some("launch-a".into()),
                    binding_revision: 1,
                },
                WorkspaceParticipant {
                    actor: self.b.clone(),
                    launch_id: Some("launch-b".into()),
                    binding_revision: 1,
                },
            ]
        } else {
            Vec::new()
        };
        Ok(ChannelAuthority {
            actor: actor.clone(),
            writable: true,
            workspace_open: true,
            participants,
        })
    }

    fn as_actor(&self, actor: &Participant, request: ChannelRequest) -> Result<ChannelResponse> {
        self.engine.request(
            request,
            |workspace| self.authority(actor, workspace),
            |_| true,
            || false,
        )
    }

    fn request(&self, request: ChannelRequest) -> Result<ChannelResponse> {
        self.as_actor(&self.user, request)
    }

    fn create(
        &self,
        actor: &Participant,
        workspace: &str,
        membership: MembershipMode,
        members: Vec<ParticipantId>,
        settings: ChannelSettings,
    ) -> ChannelId {
        let ChannelResponse::Channel { channel } = self
            .as_actor(
                actor,
                ChannelRequest::Create {
                    workspace_id: workspace.into(),
                    name: "ordinary".into(),
                    membership,
                    members,
                    settings,
                },
            )
            .unwrap()
        else {
            panic!("create reply")
        };
        channel.id
    }

    fn wide(&self, settings: ChannelSettings) -> ChannelId {
        self.create(
            &self.user,
            WORKSPACE,
            MembershipMode::WorkspaceWide,
            Vec::new(),
            settings,
        )
    }

    fn list(&self, actor: &Participant) -> Vec<ChannelSummary> {
        let ChannelResponse::Listed {
            channels,
            next_after: None,
        } = self
            .as_actor(
                actor,
                ChannelRequest::List {
                    workspace_id: WORKSPACE.into(),
                    after: None,
                    limit: MAX_PAGE_LIMIT,
                },
            )
            .unwrap()
        else {
            panic!("list reply")
        };
        channels
    }

    fn sql(&self, sql: &str) {
        self.engine
            .with_store(|store| Ok(store.connection.execute_batch(sql)?))
            .unwrap();
    }

    fn drop_ordered_wake_schema(&self) {
        self.sql(
            "DROP INDEX pending_launch_enqueue;
             DROP INDEX pending_channel_sequence;
             DROP INDEX wake_batch_membership;
             DROP INDEX active_batch_launch;
             DROP INDEX unsettled_batch_deadline;
             ALTER TABLE channel_batches DROP COLUMN revoked;
             ALTER TABLE notification_work DROP COLUMN enqueue_ordinal;
             ALTER TABLE notification_work DROP COLUMN channel_sequence;",
        );
    }

    fn drop_delivery_receipt_schema(&self) {
        self.sql(
            "ALTER TABLE notification_work DROP COLUMN read_at;
             ALTER TABLE notification_work DROP COLUMN acknowledged_at;",
        );
    }

    fn drop_wake_schema(&self) {
        self.drop_ordered_wake_schema();
        self.drop_delivery_receipt_schema();
        self.sql(
            "DROP INDEX claimed_wake_items;
             ALTER TABLE notification_work DROP COLUMN batch_id;
             ALTER TABLE notification_work DROP COLUMN binding_revision;
             DROP TABLE channel_batches;",
        );
    }

    fn restore_legacy_wake_indexes(&self) {
        self.drop_ordered_wake_schema();
        self.drop_delivery_receipt_schema();
        self.sql(
            "CREATE UNIQUE INDEX active_batch_launch ON channel_batches(launch_id) WHERE outcome IS NULL;
             CREATE INDEX pending_launch_wakes ON notification_work(launch_id,binding_revision,channel_id)
                 WHERE pending=1 AND launch_id IS NOT NULL;",
        );
    }

    fn work(&self, message: &MessageId) -> Vec<(String, Option<String>, bool, bool)> {
        self.engine.with_store(|store| {
            let mut query = store.connection.prepare(
                "SELECT participant_id,launch_id,addressed,pending FROM notification_work WHERE message_id=?1 ORDER BY participant_id",
            )?;
            Ok(query.query_map([message.as_ref()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?)
        }).unwrap()
    }

    fn wake_claim<'a>(&'a self, batch_id: &'a str, generation: u64) -> ChannelClaim<'a> {
        ChannelClaim {
            recipient: ChannelRecipient {
                launch: "launch-a",
                binding: 1,
                participant: self.a.id.as_ref(),
            },
            batch_id,
            receiver_id: "receiver",
            generation,
            deadline: u64::MAX / 2,
        }
    }

    fn claim(&self, claim: &ChannelClaim<'_>) -> Option<ChannelWake> {
        self.engine
            .with_store(|store| {
                store.claim_wakes(
                    claim,
                    None,
                    &|workspace| self.authority(&self.a, workspace),
                    || Ok(()),
                )
            })
            .unwrap()
    }

    fn wake_current(&self, claim: &ChannelClaim<'_>) -> bool {
        self.engine
            .with_store(|store| {
                store.channel_wake_current(claim, 1, None, &|workspace| {
                    self.authority(&self.a, workspace)
                })
            })
            .unwrap()
    }

    fn settle(
        &self,
        claim: &ChannelClaim<'_>,
        outcome: &str,
        reason: Option<&str>,
        recheck: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        self.engine.with_store(|store| {
            store.settle_channel_wake(
                claim.recipient.launch,
                claim.receiver_id,
                claim.batch_id,
                claim.generation,
                outcome,
                reason,
                recheck,
            )
        })
    }

    fn wake_record(&self, batch_id: &str) -> (bool, Option<String>, Option<String>) {
        self.engine
            .with_store(|store| {
                Ok(store.connection.query_row(
                    "SELECT revoked,outcome,reason FROM channel_batches WHERE id=?1",
                    [batch_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .unwrap()
    }

    fn wake_lookup(
        &self,
        claim: &ChannelClaim<'_>,
        maximum: Option<usize>,
    ) -> Result<serde_json::Value> {
        self.engine.with_store(|store| {
            store.channel_wake_lookup(
                claim.recipient,
                claim.batch_id.to_owned(),
                maximum,
                None,
                None,
                ChannelWakeAccess {
                    admit: &|workspace| self.authority(&self.a, workspace),
                    author_mention: &|_, _| unreachable!("fixture posts are user-authored"),
                    connected: &|| true,
                },
            )
        })
    }
}

fn quiet() -> ChannelSettings {
    ChannelSettings {
        user_wake: WakePolicy::None,
        agent_wake: WakePolicy::None,
        ..ChannelSettings::default()
    }
}

fn waking() -> ChannelSettings {
    ChannelSettings {
        user_wake: WakePolicy::All,
        ..quiet()
    }
}

fn post(key: &str) -> ChannelPost {
    ChannelPost {
        idempotency_key: key.into(),
        text: "a post quoting @Coordinator".into(),
        intent: PostIntent::Note,
        mentions: Vec::new(),
        mention_spans: Vec::new(),
        reply_to: None,
        attachments: Vec::new(),
        source: None,
    }
}

fn send(channel: &ChannelId, post: ChannelPost) -> ChannelRequest {
    ChannelRequest::Post {
        channel_id: channel.clone(),
        post,
    }
}

fn send_user(
    channel: &ChannelId,
    post: ChannelPost,
    bindings: Vec<ChannelMentionBinding>,
) -> ChannelRequest {
    ChannelRequest::PostUser {
        channel_id: channel.clone(),
        post,
        bindings,
    }
}

fn posted(response: ChannelResponse) -> MessageSummary {
    let ChannelResponse::Posted { message, .. } = response else {
        panic!("post reply")
    };
    message
}

fn messages(channel: &ChannelId, before: Option<u64>, limit: u16) -> ChannelRequest {
    ChannelRequest::History {
        channel_id: channel.clone(),
        before,
        after: None,
        limit,
    }
}

fn subscribe(
    channel_id: &ChannelId,
    after: Option<u64>,
    limit: u16,
    wait_ms: u32,
) -> ChannelRequest {
    ChannelRequest::Subscribe {
        channel_id: channel_id.clone(),
        after: after.map(|sequence| ChannelCursor {
            channel_id: channel_id.clone(),
            sequence,
        }),
        limit,
        wait_ms,
    }
}

fn mark(channel_id: &ChannelId, through: u64) -> ChannelRequest {
    ChannelRequest::MarkRead {
        channel_id: channel_id.clone(),
        through,
    }
}

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[24, 42, 71])
        .unwrap();
    bytes
}

fn upload(channel: &ChannelId, bytes: Vec<u8>) -> ChannelRequest {
    ChannelRequest::UploadAttachment {
        channel_id: channel.clone(),
        upload: AttachmentUpload {
            media_type: AttachmentMediaType::Png,
            alt_text: Some("Example".into()),
            bytes,
        },
    }
}

fn uploaded(response: ChannelResponse) -> AttachmentDescriptor {
    let ChannelResponse::AttachmentUploaded { attachment } = response else {
        panic!("attachment upload reply")
    };
    attachment
}

#[test]
fn access_revocation_and_retry_freeze_target_without_replay() {
    let f = Fixture::new();
    let id = f.create(
        &f.a,
        WORKSPACE,
        MembershipMode::Explicit,
        Vec::new(),
        ChannelSettings::default(),
    );
    assert!(f.list(&f.a).iter().any(|channel| channel.id == id));
    assert!(!f.list(&f.b).iter().any(|channel| channel.id == id));
    assert!(f.as_actor(&f.b, messages(&id, None, 1)).is_err());
    assert!(f.as_actor(&f.b, send(&id, post("intruder"))).is_err());
    assert!(
        f.as_actor(
            &f.a,
            ChannelRequest::SetMembership {
                channel_id: id.clone(),
                membership: MembershipMode::Explicit,
                members: vec![f.a.id.clone(), f.b.id.clone()],
            }
        )
        .is_err()
    );
    assert!(
        f.request(ChannelRequest::SetMembership {
            channel_id: id.clone(),
            membership: MembershipMode::Explicit,
            members: vec![ParticipantId("outside".into())],
        })
        .is_err()
    );

    let set = |ids| ChannelRequest::SetMembership {
        channel_id: id.clone(),
        membership: MembershipMode::Explicit,
        members: ids,
    };
    f.request(set(vec![f.a.id.clone(), f.b.id.clone()]))
        .unwrap();
    let mut request = post("address-b");
    request.mentions.push(f.b.id.clone());
    let accepted = f.as_actor(&f.a, send(&id, request.clone())).unwrap();
    let message = posted(accepted.clone());
    assert_eq!(message.author, f.a);
    assert_eq!(
        f.work(&message.id),
        [(f.b.id.0.clone(), Some("launch-b".into()), true, true)]
    );

    f.request(set(vec![f.a.id.clone()])).unwrap();
    f.request(ChannelRequest::SetSettings {
        channel_id: id.clone(),
        settings: quiet(),
    })
    .unwrap();
    assert!(f.as_actor(&f.b, messages(&id, None, 1)).is_err());
    assert!(
        f.as_actor(
            &f.b,
            ChannelRequest::Read {
                channel_id: id.clone(),
                message_id: message.id.clone()
            }
        )
        .is_err()
    );
    assert_eq!(
        f.as_actor(&f.b, subscribe(&id, Some(0), 1, 0)).unwrap(),
        ChannelResponse::Changes {
            cursor: ChannelCursor {
                channel_id: id.clone(),
                sequence: 0
            },
            changes: Vec::new(),
            more: false,
            status: ChannelSubscriptionStatus::Closed,
        }
    );
    assert_eq!(
        f.as_actor(&f.a, send(&id, request.clone())).unwrap(),
        accepted
    );
    let original = request.clone();
    request.idempotency_key = "stale-mention".into();
    assert!(f.as_actor(&f.a, send(&id, request)).is_err());
    assert_eq!(
        f.work(&message.id),
        [(f.b.id.0.clone(), Some("launch-b".into()), true, false)]
    );

    f.request(set(vec![f.b.id.clone()])).unwrap();
    assert!(f.as_actor(&f.a, send(&id, original)).is_err());
    assert!(
        f.as_actor(
            &f.b,
            ChannelRequest::Read {
                channel_id: id.clone(),
                message_id: message.id.clone()
            }
        )
        .is_ok()
    );
    assert!(!f.work(&message.id)[0].3);
}

#[test]
fn typed_mentions_and_policy_select_only_current_agents_and_preserve_unread_attention() {
    let f = Fixture::new();
    let settings = ChannelSettings {
        user_wake: WakePolicy::AddressedOrAll,
        agent_wake: WakePolicy::AddressedOnly,
        attention_questions: true,
        attention_reports: false,
    };
    let id = f.wide(settings);
    let literal = posted(f.request(send(&id, post("literal"))).unwrap());
    assert_eq!(
        f.work(&literal.id),
        [
            (f.a.id.0.clone(), Some("launch-a".into()), false, true),
            (f.b.id.0.clone(), Some("launch-b".into()), false, true),
        ]
    );

    let roster = |after| ChannelRequest::UserRoster {
        workspace_id: WORKSPACE.into(),
        channel_id: Some(id.clone()),
        after,
        limit: 2,
    };
    let entry = |participant: &Participant, launch: &str| ChannelRosterEntry {
        participant: participant.clone(),
        binding: Some(ChannelMentionBinding {
            participant_id: participant.id.clone(),
            launch_id: launch.into(),
            binding_generation: 1,
        }),
    };
    let a = entry(&f.a, "launch-a");
    let b = entry(&f.b, "launch-b");
    assert_eq!(
        f.request(roster(None)).unwrap(),
        ChannelResponse::UserRoster {
            participants: vec![a, b.clone()],
            next_after: Some(f.b.id.clone()),
        }
    );
    assert_eq!(
        f.request(roster(Some(f.b.id.clone()))).unwrap(),
        ChannelResponse::UserRoster {
            participants: vec![ChannelRosterEntry {
                participant: f.user.clone(),
                binding: None,
            }],
            next_after: None,
        }
    );
    let binding = b.binding.unwrap();

    let mut directed = post("directed");
    directed.mentions.push(f.b.id.clone());
    directed.reply_to = Some(literal.id.clone());
    let error = f
        .request(send(&id, directed.clone()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("require an exact binding"), "{error}");
    let addressed = posted(
        f.request(send_user(&id, directed, vec![binding.clone()]))
            .unwrap(),
    );
    assert_eq!(
        f.work(&addressed.id),
        [(f.b.id.0.clone(), Some("launch-b".into()), true, true)]
    );

    f.request(ChannelRequest::SetSettings {
        channel_id: id.clone(),
        settings: ChannelSettings {
            user_wake: WakePolicy::All,
            ..settings
        },
    })
    .unwrap();
    let mut broad = post("broad");
    broad.mentions.push(f.b.id.clone());
    let broad = posted(f.request(send_user(&id, broad, vec![binding])).unwrap());
    assert_eq!(
        f.work(&broad.id),
        [
            (f.a.id.0.clone(), Some("launch-a".into()), false, true),
            (f.b.id.0.clone(), Some("launch-b".into()), true, true),
        ]
    );

    let mut question = post("question");
    question.intent = PostIntent::Question;
    question
        .mentions
        .extend([f.a.id.clone(), f.user.id.clone()]);
    let attention = posted(f.as_actor(&f.a, send(&id, question)).unwrap());
    assert!(f.work(&attention.id).is_empty());
    let unread = |actor| {
        f.list(actor)
            .into_iter()
            .find(|channel| channel.id == id)
            .unwrap()
            .unread_attention
    };
    assert!(unread(&f.user));
    assert!(!unread(&f.a));
    f.as_actor(&f.a, mark(&id, attention.sequence)).unwrap();
    assert!(unread(&f.user));
    f.request(mark(&id, addressed.sequence)).unwrap();
    assert!(unread(&f.user));
    f.request(mark(&id, attention.sequence)).unwrap();
    assert!(!unread(&f.user));

    let mut own = post("own-question");
    own.intent = PostIntent::Question;
    assert!(matches!(
        f.request(send(&id, own)).unwrap(),
        ChannelResponse::Posted {
            attention: false,
            ..
        }
    ));
    let mut report = post("report");
    report.intent = PostIntent::Report;
    assert!(matches!(
        f.as_actor(&f.b, send(&id, report)).unwrap(),
        ChannelResponse::Posted {
            wake_count: 0,
            attention: false,
            ..
        }
    ));

    let late = Participant {
        id: ParticipantId("later".into()),
        kind: ParticipantKind::Agent,
        display_name: f.b.display_name.clone(),
    };
    assert!(
        f.engine
            .request(
                messages(&id, None, 1),
                |workspace| f.authority(&late, workspace),
                |_| true,
                || false
            )
            .is_ok()
    );
}

#[test]
fn wake_source_rechecks_read_authority_and_omits_unavailable_links() {
    let f = Fixture::new();
    let source_channel = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    let ChannelResponse::Channel {
        channel: destination,
    } = f
        .request(ChannelRequest::Create {
            workspace_id: WORKSPACE.into(),
            name: "wake destination".into(),
            membership: MembershipMode::WorkspaceWide,
            members: Vec::new(),
            settings: ChannelSettings {
                user_wake: WakePolicy::All,
                ..quiet()
            },
        })
        .unwrap()
    else {
        panic!("create reply")
    };
    let original = posted(f.request(send(&source_channel, post("original"))).unwrap());
    let recipient = ChannelRecipient {
        launch: "launch-a",
        binding: 1,
        participant: f.a.id.as_ref(),
    };
    let admit = |workspace: &str| f.authority(&f.a, workspace);
    let linked = |key, channel_id, message_id| {
        let mut link = post(key);
        link.source = Some(Box::new(ChannelSource {
            channel_id,
            message_id,
        }));
        f.request(send(&destination.id, link)).unwrap();
        f.engine
            .with_store(|store| {
                store.claim_wakes(
                    &ChannelClaim {
                        recipient,
                        batch_id: key,
                        receiver_id: "receiver",
                        generation: 1,
                        deadline: u64::MAX / 2,
                    },
                    None,
                    &admit,
                    || Ok(()),
                )
            })
            .unwrap()
            .expect("linked post enqueued a wake")
    };
    let lookup = |wake: &ChannelWake, include_source: bool, connected: &dyn Fn() -> bool| {
        f.engine.with_store(|store| {
            store.channel_wake_lookup(
                recipient,
                wake.batch_id.clone(),
                Some(16),
                None,
                include_source.then_some(WORKSPACE),
                super::ChannelWakeAccess {
                    admit: &admit,
                    author_mention: &|_, _| unreachable!("fixture posts are user-authored"),
                    connected,
                },
            )
        })
    };
    let accessible = linked("accessible", source_channel.clone(), original.id.clone());
    let expanded = lookup(&accessible, true, &|| true).unwrap();
    assert_eq!(
        expanded["post"]["source"]["message"]["id"],
        original.id.as_ref()
    );
    assert_eq!(
        expanded["post"]["source"]["reply_target"],
        serde_json::json!({"channel_id": source_channel, "reply_to": original.id})
    );
    let only_destination = || {
        let first = AtomicBool::new(true);
        move || first.swap(false, Ordering::Relaxed)
    };
    assert!(lookup(&accessible, false, &|| true).is_ok());
    let error = lookup(&accessible, true, &only_destination()).unwrap_err();
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string() == "channel request disconnected")
    );

    let missing_message = linked(
        "missing-message",
        source_channel.clone(),
        MessageId("missing-post".into()),
    );
    let missing_channel = linked(
        "missing-channel",
        ChannelId("missing-channel".into()),
        original.id.clone(),
    );
    let foreign = f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        quiet(),
    );
    let foreign_post = posted(f.request(send(&foreign, post("foreign"))).unwrap());
    let denied = linked("foreign-source", foreign, foreign_post.id);
    for wake in [&missing_message, &missing_channel, &denied] {
        let omitted = lookup(wake, true, &|| true).unwrap();
        assert!(omitted["post"].get("source").is_none());
        assert_eq!(omitted, lookup(wake, false, &|| true).unwrap());
    }
    f.request(ChannelRequest::SetMembership {
        channel_id: source_channel,
        membership: MembershipMode::Explicit,
        members: Vec::new(),
    })
    .unwrap();
    let revoked = lookup(&accessible, true, &|| true).unwrap();
    assert!(revoked["post"].get("source").is_none());
    assert_eq!(revoked, lookup(&accessible, false, &|| true).unwrap());
}

#[test]
fn wake_claim_uses_oldest_channel_and_caps_each_batch_in_sequence_order() {
    let f = Fixture::new();
    let first = f.wide(quiet());
    f.request(send(&first, post("quiet-history"))).unwrap();
    f.request(ChannelRequest::SetSettings {
        channel_id: first.clone(),
        settings: waking(),
    })
    .unwrap();
    let desk = f
        .list(&f.user)
        .into_iter()
        .find(|c| c.name == "#desk")
        .unwrap()
        .id;
    let older = posted(f.request(send(&first, post("first"))).unwrap());
    let between = posted(f.request(send(&desk, post("between"))).unwrap());
    let mut remaining = vec![older.id];
    for sequence in 2..=33 {
        let mut next = post(&format!("post-{sequence}"));
        if matches!(sequence, 2 | 32 | 33) {
            next.mentions.push(f.a.id.clone());
        }
        remaining.push(posted(f.request(send(&first, next)).unwrap()).id);
    }

    let claim = f.wake_claim("first-32", 1);
    let first_batch = f.claim(&claim).unwrap();
    assert_eq!(first_batch.channel_id, first);
    assert_eq!(first_batch.pending_count, 32);
    assert_eq!(
        first_batch.addressed_message_id.as_ref(),
        Some(&remaining[31])
    );
    let lookup = f.wake_lookup(&claim, None).unwrap();
    let messages = lookup["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 32);
    for (index, item) in messages.iter().enumerate() {
        assert_eq!(item["id"], remaining[index].as_ref());
        assert_eq!(item["sequence"], index + 2);
    }

    let claim = f.wake_claim("between", 2);
    let second = f.claim(&claim).unwrap();
    assert_eq!(second.channel_id, desk);
    assert_eq!(second.pending_count, 1);
    let expanded = f.wake_lookup(&claim, Some(16)).unwrap();
    let exact = f
        .engine
        .with_store(|store| {
            store.compact_message(
                ChannelRequest::Read {
                    channel_id: ChannelId("#desk".into()),
                    message_id: between.id.clone(),
                },
                &|workspace| f.authority(&f.a, workspace),
                0,
                16,
                Some(WORKSPACE),
            )
        })
        .unwrap();
    assert_eq!(exact["reply_target"], expanded["post"]["reply_target"]);
    assert_eq!(
        exact["reply_target"],
        serde_json::json!({"channel_id": desk, "reply_to": between.id})
    );

    let tail = f.claim(&f.wake_claim("tail", 3)).unwrap();
    assert_eq!(tail.channel_id, first);
    assert_eq!(tail.pending_count, 1);
    assert_eq!(tail.addressed_message_id.as_ref(), Some(&remaining[32]));
    assert!(f.claim(&f.wake_claim("no-replay", 4)).is_none());
}

#[test]
fn wake_revocation_refunds_capacity_without_losing_exact_native_confirmation() {
    let f = Fixture::new();
    let channel = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        waking(),
    );
    let recorded = f.wake_claim("recorded", 1);
    f.request(send(&channel, post("recorded"))).unwrap();
    f.claim(&recorded).unwrap();
    assert!(f.settle(&recorded, "accepted", None, || Ok(())).unwrap());
    assert!(f.wake_lookup(&recorded, None).is_ok());

    let active = f.wake_claim("active", 2);
    f.request(send(&channel, post("active"))).unwrap();
    f.claim(&active).unwrap();
    assert!(f.wake_current(&active));
    assert!(f.wake_lookup(&active, None).is_ok());
    let queued = posted(f.request(send(&channel, post("not-yet-claimed"))).unwrap());

    let membership = |members| ChannelRequest::SetMembership {
        channel_id: channel.clone(),
        membership: MembershipMode::Explicit,
        members,
    };
    f.request(membership(Vec::new())).unwrap();
    assert!(!f.wake_current(&active));
    assert!(
        f.wake_lookup(&active, None)
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    assert!(f.work(&queued.id).iter().all(|entry| !entry.3));
    assert_eq!(
        f.wake_record(recorded.batch_id),
        (true, Some("accepted".into()), None)
    );
    assert_eq!(f.wake_record(active.batch_id), (true, None, None));

    f.request(membership(vec![f.a.id.clone()])).unwrap();
    assert!(f.as_actor(&f.a, messages(&channel, None, 3)).is_ok());
    for claim in [&recorded, &active] {
        assert!(!f.wake_current(claim));
        assert!(
            f.wake_lookup(claim, None)
                .unwrap_err()
                .is::<ChannelAccessLost>()
        );
    }
    assert!(f.claim(&f.wake_claim("no-replay", 3)).is_none());

    // Leave just one pending slot. The revoked batch must not consume it or be settled by reclaim.
    f.sql(
        "WITH RECURSIVE fill(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM fill WHERE x<4095)
         INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed)
         SELECT p.channel_id,p.id,'extra-'||x,'launch-b',0 FROM fill CROSS JOIN posts p
         WHERE p.request_key='not-yet-claimed';",
    );
    f.request(send(&channel, post("after-readd"))).unwrap();
    let next = f.wake_claim("after-readd", 4);
    assert_eq!(f.claim(&next).unwrap().pending_count, 1);
    assert!(f.wake_current(&next));
    assert_eq!(f.wake_record(active.batch_id), (true, None, None));
    assert!(
        f.settle(&active, "failed", Some("native rejection"), || Ok(()))
            .unwrap()
    );
    assert_eq!(
        f.wake_record(active.batch_id),
        (true, Some("failed".into()), Some("native rejection".into()))
    );
    assert!(f.wake_current(&next));
    assert!(f.claim(&f.wake_claim("still-no-replay", 5)).is_none());
}

#[test]
fn wake_native_outcomes_are_exact_durable_and_not_overwritten() {
    let f = Fixture::new();
    let channel = f.wide(waking());
    for (generation, outcome, reason) in [
        (1, "accepted", None),
        (2, "failed", Some("native rejected")),
        (3, "unknown", Some("native returned no result")),
    ] {
        let claim = f.wake_claim(outcome, generation);
        let message = posted(f.request(send(&channel, post(outcome))).unwrap());
        let reject = || anyhow::bail!("receiver replaced before commit");
        assert!(
            f.engine
                .with_store(|store| {
                    store.claim_wakes(
                        &claim,
                        None,
                        &|workspace| f.authority(&f.a, workspace),
                        reject,
                    )
                })
                .is_err()
        );
        assert!(f.work(&message.id).iter().all(|work| work.3));
        f.claim(&claim).unwrap();

        let stale = ChannelClaim {
            receiver_id: "replaced-receiver",
            ..f.wake_claim(outcome, generation)
        };
        assert!(
            !f.settle(&stale, "accepted", None, || unreachable!())
                .unwrap()
        );
        assert!(f.settle(&claim, outcome, reason, reject).is_err());
        assert_eq!(f.wake_record(claim.batch_id), (false, None, None));
        assert!(f.settle(&claim, outcome, reason, || Ok(())).unwrap());
        let stored = (false, Some(outcome.into()), reason.map(str::to_owned));
        assert_eq!(f.wake_record(claim.batch_id), stored);
        assert!(
            !f.settle(&claim, "unknown", Some("duplicate"), || unreachable!())
                .unwrap()
        );
        assert_eq!(f.wake_record(claim.batch_id), stored);
    }

    let late = ChannelClaim {
        deadline: 0,
        ..f.wake_claim("expired", 4)
    };
    f.request(send(&channel, post("expired"))).unwrap();
    f.claim(&late).unwrap();
    assert!(!f.wake_current(&late));
    assert!(!f.settle(&late, "accepted", None, || Ok(())).unwrap());
    assert_eq!(
        f.wake_record(late.batch_id),
        (
            false,
            Some("unknown".into()),
            Some("native confirmation deadline elapsed".into())
        )
    );
}

#[test]
fn user_roster_is_observational_but_scoped_to_workspace_and_channel_members() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    let roster = |workspace_id: &str, channel_id| ChannelRequest::UserRoster {
        workspace_id: workspace_id.into(),
        channel_id,
        after: None,
        limit: MAX_PAGE_LIMIT,
    };
    let read_only = |workspace: &str| {
        let mut authority = f.authority(&f.user, workspace)?;
        authority.writable = false;
        Ok(authority)
    };
    let observe = |request| f.engine.request(request, read_only, |_| true, || false);
    let all = roster(WORKSPACE, None);
    assert!(all.is_observational());
    assert!(f.as_actor(&f.a, all.clone()).is_err());
    let ids = |request| {
        let ChannelResponse::UserRoster { participants, .. } = observe(request).unwrap() else {
            panic!("expected roster");
        };
        participants
            .into_iter()
            .map(|entry| entry.participant.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(all),
        [f.a.id.clone(), f.b.id.clone(), f.user.id.clone()]
    );
    assert_eq!(
        ids(roster(WORKSPACE, Some(id.clone()))),
        [f.a.id.clone(), f.user.id.clone()]
    );
    assert!(observe(roster(OTHER_WORKSPACE, Some(id.clone()))).is_err());

    let mut body = post("forged-unselected-agent");
    body.mentions.push(f.b.id.clone());
    let request = send_user(
        &id,
        body,
        vec![ChannelMentionBinding {
            participant_id: f.b.id.clone(),
            launch_id: "launch-b".into(),
            binding_generation: 1,
        }],
    );
    assert!(!request.is_observational());
    assert!(f.as_actor(&f.a, request.clone()).is_err());
    let error = f.request(request).unwrap_err().to_string();
    assert!(error.contains("left the channel"), "{error}");
}

#[test]
fn durable_history_recovery_and_atomic_commit_failure() {
    let f = Fixture::new();
    let id = f.wide(ChannelSettings::default());
    f.sql(
        "CREATE TABLE injected_parent(id INTEGER PRIMARY KEY);
         CREATE TABLE injected_child(id INTEGER REFERENCES injected_parent(id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER inject_commit_failure AFTER INSERT ON notification_work BEGIN INSERT INTO injected_child VALUES(1); END;",
    );
    let attachment = uploaded(f.request(upload(&id, png())).unwrap());
    let mut initial = post("commit-fails");
    initial.attachments.push(attachment.id);
    let source = Box::new(ChannelSource {
        channel_id: ChannelId("unknown-source-channel".into()),
        message_id: MessageId("unknown-source-post".into()),
    });
    initial.source = Some(source.clone());
    assert!(f.request(send(&id, initial.clone())).is_err());
    assert_eq!(f.staged_bytes(), u64::from(attachment.byte_len));
    assert_eq!(
        f.list(&f.user)
            .into_iter()
            .find(|c| c.id == id)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(
        f.request(messages(&id, None, 1)).unwrap(),
        ChannelResponse::History {
            messages: Vec::new(),
            next_before: None,
            next_after: None,
            cursor: ChannelCursor {
                channel_id: id.clone(),
                sequence: 0
            },
        }
    );
    assert!(matches!(
        f.request(subscribe(&id, Some(0), 1, 0)).unwrap(),
        ChannelResponse::Changes {
            cursor: ChannelCursor { sequence: 0, .. },
            ..
        }
    ));
    f.sql("DROP TRIGGER inject_commit_failure");
    let accepted = f.request(send(&id, initial.clone())).unwrap();
    assert_eq!(f.staged_bytes(), 0);
    let message = posted(accepted.clone());
    assert_eq!(message.sequence, 1);
    assert_eq!(f.work(&message.id).len(), 2);
    f.request(mark(&id, 1)).unwrap();

    let Fixture {
        root, engine, user, ..
    } = f;
    drop(engine);
    let recovered = ChannelEngine::new(root.path().to_owned());
    let run = |request| {
        recovered.request(
            request,
            |_| {
                Ok(ChannelAuthority {
                    actor: user.clone(),
                    writable: true,
                    workspace_open: true,
                    participants: Vec::new(),
                })
            },
            |_| true,
            || false,
        )
    };
    assert_eq!(run(send(&id, initial.clone())).unwrap(), accepted);
    let mut changed = initial.clone();
    changed.source.as_mut().unwrap().message_id = MessageId("different-source-post".into());
    assert!(run(send(&id, changed)).is_err());
    let ChannelResponse::Message { message: actual } = run(ChannelRequest::Read {
        channel_id: id.clone(),
        message_id: message.id.clone(),
    })
    .unwrap() else {
        panic!("read reply")
    };
    assert_eq!(actual.message.source, Some(source));
    assert_eq!((actual.message, actual.text), (message, initial.text));
    assert_eq!(
        run(mark(&id, 0)).unwrap(),
        ChannelResponse::ReadPosition { through: 1 }
    );
    let pending = recovered
        .with_store(|store| {
            Ok(store.connection.query_row(
                "SELECT count(*) FROM notification_work WHERE pending=1",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(pending, 0);
}

#[test]
fn recovery_preserves_explicit_membership_and_author_snapshot_not_display_name_authority() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    let original = post("recovered");
    let accepted = f.as_actor(&f.a, send(&id, original.clone())).unwrap();
    let previous = posted(accepted.clone());
    let Fixture {
        root,
        engine,
        mut a,
        b,
        ..
    } = f;
    drop(engine);
    let recovered = ChannelEngine::new(root.path().to_owned());
    let run = |actor: &Participant, writable, request| {
        recovered.request(
            request,
            |_| {
                Ok(ChannelAuthority {
                    actor: actor.clone(),
                    writable,
                    workspace_open: true,
                    participants: Vec::new(),
                })
            },
            |_| true,
            || false,
        )
    };
    assert!(run(&b, true, messages(&id, None, 1)).is_err());
    a.display_name = "New label".into();
    assert!(
        matches!(run(&a, false, messages(&id, None, 1)).unwrap(), ChannelResponse::History { messages, .. } if messages.len() == 1 && messages[0].message == previous)
    );
    assert_eq!(run(&a, false, send(&id, original)).unwrap(), accepted);
    assert!(run(&a, false, send(&id, post("read-only"))).is_err());
    assert!(run(&a, false, mark(&id, 1)).is_err());
    let latest = posted(run(&a, true, send(&id, post("renamed"))).unwrap());
    assert_eq!(latest.author, a);
    assert_eq!(latest.sequence, 2);
}

#[test]
fn retry_conflicts_are_scoped_to_authenticated_sender_and_exact_request() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    let other = f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        quiet(),
    );
    let original = post("retry");
    f.request(send(&id, original.clone())).unwrap();
    assert!(f.request(send(&other, original.clone())).is_err());
    let mut conflict = original.clone();
    conflict.text.push('!');
    assert!(f.request(send(&id, conflict)).is_err());
    let mut conflict = original.clone();
    conflict.intent = PostIntent::Report;
    assert!(f.request(send(&id, conflict)).is_err());
    let other_sender = posted(f.as_actor(&f.a, send(&id, original)).unwrap());
    assert_eq!(other_sender.sequence, 2);
    assert_eq!(other_sender.author.id, f.a.id);
}

#[test]
fn unannotated_posts_keep_legacy_fingerprints_and_receipts() {
    use sha2::{Digest, Sha256};

    let mut f = Fixture::new();
    let id = f.wide(quiet());
    let legacy = concat!(
        r#"{"idempotency_key":"legacy","text":"a post quoting @Coordinator","#,
        r#""intent":"note","mentions":[],"reply_to":null,"attachments":[]}"#
    );
    let accepted = f.request(send(&id, post("legacy"))).unwrap();
    let (fingerprint, span_type): (Vec<u8>, Option<String>) = f
        .engine
        .with_store(|store| {
            Ok(store.connection.query_row(
                "SELECT fingerprint,json_type(summary,'$.mention_spans') FROM posts WHERE id=?1",
                [posted(accepted.clone()).id.as_ref()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    let expected: [u8; 32] = Sha256::digest(legacy.as_bytes()).into();
    assert_eq!(fingerprint.as_slice(), expected.as_slice());
    assert_eq!(span_type, None);

    f.engine = ChannelEngine::new(f.root.path().to_owned());
    let old_post = serde_json::from_str(legacy).unwrap();
    assert_eq!(f.request(send(&id, old_post)).unwrap(), accepted);
}

#[test]
fn user_mention_spans_require_targets_and_retain_ordered_snapshots() {
    let mut f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.b.id.clone()],
        ChannelSettings {
            user_wake: WakePolicy::AddressedOnly,
            ..quiet()
        },
    );
    let binding = ChannelMentionBinding {
        participant_id: f.b.id.clone(),
        launch_id: "launch-b".into(),
        binding_generation: 1,
    };
    let text = "é @東京 + @user + @東京";
    let targets = [(3, 10, &f.b), (13, 18, &f.user), (21, 28, &f.b)];
    let post = ChannelPost {
        text: text.into(),
        mentions: vec![f.user.id.clone(), f.b.id.clone()],
        mention_spans: targets
            .iter()
            .map(|&(start, end, target)| ChannelMentionSpan {
                start,
                end,
                participant_id: target.id.clone(),
            })
            .collect(),
        ..post("annotated")
    };
    let request = |post| send_user(&id, post, vec![binding.clone()]);
    let mut invalid = post.clone();
    invalid.mention_spans[0].participant_id = f.a.id.clone();
    let error = f.request(request(invalid)).unwrap_err().to_string();
    assert!(error.contains("does not reference an addressed participant"));
    let mut invalid = post.clone();
    invalid.mention_spans[0].start = 1;
    let error = f.request(request(invalid)).unwrap_err().to_string();
    assert!(error.contains("splits a UTF-8 character"));

    let accepted = f.request(request(post.clone())).unwrap();
    let summary = posted(accepted.clone());
    assert_eq!(summary.sequence, 1);
    assert_eq!(
        summary.mention_spans,
        targets.map(|(start, end, target)| ChannelResolvedMentionSpan {
            start,
            end,
            participant: target.clone(),
        })
    );
    assert_eq!(
        f.work(&summary.id),
        [(f.b.id.0.clone(), Some("launch-b".into()), true, true)]
    );

    let mut conflict = post.clone();
    conflict.mention_spans[0].start = 4;
    let error = f.request(request(conflict)).unwrap_err().to_string();
    assert!(error.contains("conflicts with an existing retry key"));
    f.b.display_name = "Renamed after commit".into();
    f.request(ChannelRequest::SetMembership {
        channel_id: id.clone(),
        membership: MembershipMode::Explicit,
        members: Vec::new(),
    })
    .unwrap();
    f.engine = ChannelEngine::new(f.root.path().to_owned());

    assert_eq!(f.request(request(post.clone())).unwrap(), accepted);
    let mut stale = post;
    stale.idempotency_key = "fresh-key".into();
    let error = f.request(request(stale)).unwrap_err().to_string();
    assert_eq!(error, super::STALE_USER_MENTION);
    assert!(
        matches!(f.request(messages(&id, None, 8)).unwrap(), ChannelResponse::History { messages, .. }
            if messages.len() == 1 && messages[0].message == summary && messages[0].text == text)
    );
}

#[test]
fn attachments_are_validated_immutable_channel_resources_in_archives() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    let other = f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::Explicit,
        Vec::new(),
        quiet(),
    );
    let sample = png();
    assert!(
        f.request(upload(&id, sample[..sample.len() - 8].to_vec()))
            .is_err()
    );
    let ChannelResponse::AttachmentUploaded { attachment } =
        f.as_actor(&f.a, upload(&id, sample.clone())).unwrap()
    else {
        panic!("attachment upload reply")
    };
    let fetch = |id: &ChannelId| ChannelRequest::GetAttachment {
        channel_id: id.clone(),
        attachment_id: attachment.id.clone(),
    };
    assert!(f.as_actor(&f.b, fetch(&id)).is_err());
    assert!(f.request(fetch(&other)).is_err());
    let mut attached = post("attachment");
    attached.attachments.push(attachment.id.clone());
    assert!(f.request(send(&other, attached.clone())).is_err());
    let message = posted(f.request(send(&id, attached)).unwrap());
    assert_eq!(message.attachments, std::slice::from_ref(&attachment.id));
    let mut wrong_reply = post("bad-reply");
    wrong_reply.reply_to = Some(message.id.clone());
    assert!(f.request(send(&other, wrong_reply)).is_err());
    f.request(ChannelRequest::SetMembership {
        channel_id: id.clone(),
        membership: MembershipMode::Explicit,
        members: Vec::new(),
    })
    .unwrap();
    assert!(f.as_actor(&f.a, fetch(&id)).is_err());

    let Fixture {
        root, engine, user, ..
    } = f;
    drop(engine);
    let recovered = ChannelEngine::new(root.path().to_owned());
    let run = |request| {
        recovered.request(
            request,
            |workspace| {
                assert_eq!(workspace, WORKSPACE);
                Ok(ChannelAuthority {
                    actor: user.clone(),
                    writable: true,
                    workspace_open: false,
                    participants: Vec::new(),
                })
            },
            |_| false,
            || false,
        )
    };
    let fetched = run(fetch(&id)).unwrap();
    assert_eq!(
        fetched,
        ChannelResponse::Attachment {
            attachment: attachment.clone(),
            bytes: sample.into()
        }
    );
    assert!(run(send(&id, post("closed"))).is_err());
    assert!(
        matches!(run(messages(&id, None, 1)).unwrap(), ChannelResponse::History { messages, .. }
            if messages.len() == 1 && messages[0].message == message && messages[0].attachments == [attachment.clone()])
    );
    let response = run(ChannelRequest::Read {
        channel_id: id,
        message_id: message.id.clone(),
    })
    .unwrap();
    assert!(
        matches!(response, ChannelResponse::Message { message: actual }
        if actual.message == message && actual.attachments == [attachment])
    );
}

#[test]
fn history_and_subscription_pages_are_bounded_resumable_and_actor_filtered() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    for i in 0..5 {
        f.request(send(&id, post(&format!("post-{i}")))).unwrap();
    }
    let ChannelResponse::History {
        messages: page,
        next_before,
        next_after: None,
        ..
    } = f.request(messages(&id, None, 2)).unwrap()
    else {
        panic!("history reply")
    };
    assert_eq!(
        page.iter()
            .map(|message| message.message.sequence)
            .collect::<Vec<_>>(),
        [4, 5]
    );
    assert_eq!(next_before, Some(4));
    let ChannelResponse::History {
        messages: page,
        next_before,
        next_after: Some(3),
        ..
    } = f.request(messages(&id, next_before, 2)).unwrap()
    else {
        panic!("history reply")
    };
    assert_eq!(
        page.iter()
            .map(|message| message.message.sequence)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(next_before, Some(2));

    f.as_actor(&f.a, mark(&id, 3)).unwrap();
    assert!(f.request(mark(&id, 6)).is_err());
    assert_eq!(
        f.as_actor(&f.a, mark(&id, 2)).unwrap(),
        ChannelResponse::ReadPosition { through: 3 }
    );
    let mut page_start = 0;
    let mut observed = Vec::new();
    loop {
        let ChannelResponse::Changes {
            cursor,
            changes,
            more,
            status: ChannelSubscriptionStatus::Active,
        } = f.request(subscribe(&id, Some(page_start), 2, 0)).unwrap()
        else {
            panic!("changes reply")
        };
        assert!(changes.len() <= 2);
        observed.extend(changes);
        page_start = cursor.sequence;
        if !more {
            break;
        }
    }
    assert_eq!(
        observed,
        (1..=5)
            .map(|sequence| ChannelChange {
                sequence,
                kind: ChannelChangeKind::Post
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(page_start, 6);
    assert!(f.request(subscribe(&id, Some(7), 2, 0)).is_err());
    assert!(
        matches!(f.request(subscribe(&id, None, 2, 0)).unwrap(), ChannelResponse::Changes { cursor: ChannelCursor { sequence: 6, .. }, changes, .. } if changes.is_empty())
    );
    assert!(
        matches!(f.as_actor(&f.a, subscribe(&id, Some(5), 2, 0)).unwrap(), ChannelResponse::Changes { changes, .. } if changes == [ChannelChange { sequence: 6, kind: ChannelChangeKind::ReadPosition }])
    );
}

#[test]
fn subscription_commit_revocation_and_capacity_do_not_depend_on_slow_consumers() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    thread::scope(|scope| {
        let (observed, ready) = crossbeam_channel::bounded(0);
        let (completed, result) = crossbeam_channel::bounded(1);
        let (fixture, channel) = (&f, &id);
        scope.spawn(move || {
            let sampled = Cell::new(false);
            let signalled = Cell::new(false);
            completed
                .send(fixture.engine.request(
                    subscribe(channel, Some(0), 1, MAX_WAIT_MS),
                    |workspace| {
                        sampled.set(true);
                        fixture.authority(&fixture.a, workspace)
                    },
                    |_| true,
                    || {
                        // After the empty store observation, just before waiting for a commit.
                        if sampled.get() && !signalled.replace(true) {
                            observed.send(()).unwrap();
                        }
                        false
                    },
                ))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        f.request(send(&id, post("race"))).unwrap();
        // A final re-observation at MAX_WAIT_MS also sees the commit; it is too late.
        let response = result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(
            matches!(response, ChannelResponse::Changes { changes, .. } if changes == [ChannelChange { sequence: 1, kind: ChannelChangeKind::Post }])
        );
    });
    thread::scope(|scope| {
        let (observed, ready) = crossbeam_channel::bounded(MAX_SUBSCRIBERS);
        let mut waiters = Vec::with_capacity(MAX_SUBSCRIBERS);
        for _ in 0..MAX_SUBSCRIBERS {
            let (f, id, observed) = (&f, &id, observed.clone());
            waiters.push(scope.spawn(move || {
                let signalled = AtomicBool::new(false);
                f.engine.request(
                    subscribe(id, Some(1), 1, MAX_WAIT_MS),
                    |workspace| {
                        if !signalled.swap(true, Ordering::Relaxed) {
                            observed.send(()).unwrap();
                        }
                        f.authority(&f.a, workspace)
                    },
                    |_| true,
                    || false,
                )
            }));
        }
        for _ in 0..MAX_SUBSCRIBERS {
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let busy = f.request(subscribe(&id, Some(1), 1, 0)).unwrap_err();
        assert!(busy.to_string().contains("capacity"));
        f.request(ChannelRequest::SetMembership {
            channel_id: id.clone(),
            membership: MembershipMode::Explicit,
            members: Vec::new(),
        })
        .unwrap();
        for waiter in waiters {
            assert!(
                matches!(waiter.join().unwrap().unwrap(), ChannelResponse::Changes {
                cursor: ChannelCursor { sequence: 1, .. },
                changes,
                more: false,
                status: ChannelSubscriptionStatus::Closed,
            } if changes.is_empty())
            );
        }
    });
    assert!(
        matches!(f.request(subscribe(&id, Some(1), 1, 0)).unwrap(), ChannelResponse::Changes { changes, .. } if changes == [ChannelChange { sequence: 2, kind: ChannelChangeKind::Membership }])
    );
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let (observed, ready) = crossbeam_channel::bounded(1);
        let (fixture, channel, disconnected) = (&f, &id, &cancelled);
        let waiter = scope.spawn(move || {
            fixture.engine.request(
                subscribe(channel, Some(2), 1, MAX_WAIT_MS),
                |workspace| {
                    observed.send(()).unwrap();
                    fixture.authority(&fixture.user, workspace)
                },
                |_| true,
                || disconnected.load(Ordering::Acquire),
            )
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(
            matches!(waiter.join().unwrap().unwrap(), ChannelResponse::Changes {
            cursor: ChannelCursor { sequence: 2, .. },
            changes,
            more: false,
            status: ChannelSubscriptionStatus::Closed,
        } if changes.is_empty())
        );
    });
}

#[test]
fn full_notification_queue_rejects_only_new_work_and_keeps_accepted_history() {
    let f = Fixture::new();
    let id = f.wide(ChannelSettings::default());
    let first = post("accepted");
    let accepted = f.request(send(&id, first.clone())).unwrap();
    f.sql(
        "WITH RECURSIVE fill(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM fill WHERE x<4094)
         INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed)
         SELECT p.channel_id,p.id,'extra-'||x,'extra-launch-'||x,0 FROM fill CROSS JOIN posts p;",
    );
    assert_eq!(f.request(send(&id, first)).unwrap(), accepted);
    assert!(
        f.request(send(&id, post("overload")))
            .unwrap_err()
            .to_string()
            .contains("capacity")
    );
    assert_eq!(
        f.list(&f.user)
            .into_iter()
            .find(|c| c.id == id)
            .unwrap()
            .last_sequence,
        1
    );
    f.request(ChannelRequest::SetSettings {
        channel_id: id.clone(),
        settings: quiet(),
    })
    .unwrap();
    assert_eq!(
        posted(f.request(send(&id, post("overload"))).unwrap()).sequence,
        2
    );
}

#[test]
fn lazy_store_rejects_unsafe_main_file_and_sidecars() {
    let f = Fixture::new();
    let path = f.root.path().join("channels.sqlite3");
    let sidecar = f.root.path().join("channels.sqlite3-journal");
    let external = tempfile::NamedTempFile::new().unwrap();
    assert!(!path.exists());
    symlink(external.path(), &path).unwrap();
    let list = || ChannelRequest::List {
        workspace_id: WORKSPACE.into(),
        after: None,
        limit: 1,
    };
    assert!(f.request(list()).is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(external.path(), &path).unwrap();
    assert!(f.request(list()).is_err());
    fs::remove_file(&path).unwrap();
    symlink(external.path(), &sidecar).unwrap();
    assert!(f.request(list()).is_err());
    assert_eq!(external.as_file().metadata().unwrap().len(), 0);
    fs::remove_file(&sidecar).unwrap();
    assert!(f.request(list()).is_ok());
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn retained_archive_discovery_and_membership_do_not_depend_on_live_roster() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone(), f.b.id.clone()],
        quiet(),
    );
    f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        quiet(),
    );
    let run = |actor: &Participant, request, close_both: bool| {
        f.engine.request(
            request,
            |workspace| {
                let mut authority = f.authority(actor, workspace)?;
                authority.workspace_open = workspace == OTHER_WORKSPACE && !close_both;
                authority.participants.clear();
                Ok(authority)
            },
            |workspace| workspace == OTHER_WORKSPACE && !close_both,
            || false,
        )
    };
    let archives = |after| ChannelRequest::ListArchivedWorkspaces { after, limit: 1 };
    assert_eq!(
        run(&f.user, archives(None), false).unwrap(),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: vec![WORKSPACE.into()],
            next_after: Some(WORKSPACE.into()),
        }
    );
    assert_eq!(
        run(&f.user, archives(Some(WORKSPACE.into())), false).unwrap(),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: Vec::new(),
            next_after: None,
        }
    );
    assert_eq!(
        run(&f.user, archives(None), true).unwrap(),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: vec![WORKSPACE.into()],
            next_after: Some(WORKSPACE.into()),
        }
    );
    assert_eq!(
        run(&f.user, archives(Some(WORKSPACE.into())), true).unwrap(),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: vec![OTHER_WORKSPACE.into()],
            next_after: None,
        }
    );
    assert_eq!(
        run(&f.a, archives(None), true).unwrap(),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: Vec::new(),
            next_after: None
        }
    );
    let list = |workspace_id: &str| ChannelRequest::List {
        workspace_id: workspace_id.into(),
        after: None,
        limit: 8,
    };
    assert!(run(&f.user, list("not-retained"), true).is_err());
    assert!(run(&f.a, list(WORKSPACE), true).is_err());
    assert!(matches!(
        run(&f.user, list(WORKSPACE), true).unwrap(),
        ChannelResponse::Listed { channels, next_after: None } if channels.iter().any(|channel| channel.id == id)
    ));
    assert_eq!(
        run(
            &f.user,
            ChannelRequest::ListMembers {
                channel_id: id.clone(),
                after: None,
                limit: 1,
            },
            true
        )
        .unwrap(),
        ChannelResponse::Members {
            participants: Vec::new(),
            next_after: None
        }
    );
    let explicit = |after| ChannelRequest::ListExplicitMembers {
        channel_id: id.clone(),
        after,
        limit: 1,
    };
    assert_eq!(
        run(&f.user, explicit(None), true).unwrap(),
        ChannelResponse::ExplicitMembers {
            members: vec![f.a.id.clone()],
            next_after: Some(f.a.id.clone()),
        }
    );
    assert_eq!(
        run(&f.user, explicit(Some(f.a.id.clone())), true).unwrap(),
        ChannelResponse::ExplicitMembers {
            members: vec![f.b.id.clone()],
            next_after: None
        }
    );
    assert!(run(&f.a, explicit(None), true).is_err());
}

#[test]
fn archive_scan_is_bounded_and_continues_after_empty_pages_without_agent_disclosure() {
    let f = Fixture::new();
    f.engine
        .with_store(|store| {
            store.connection.execute(
                "WITH RECURSIVE candidates(i) AS (
                     VALUES(0) UNION ALL SELECT i+1 FROM candidates WHERE i<?1
                 )
                 INSERT INTO channels(id,workspace_id,name,workspace_wide,settings)
                 SELECT printf('first-%03d',i),printf('workspace-%03d',i),'first',1,?2 FROM candidates
                 UNION ALL
                 SELECT printf('second-%03d',i),printf('workspace-%03d',i),'second',1,?2 FROM candidates",
                rusqlite::params![MAX_ARCHIVE_CANDIDATES, serde_json::to_string(&quiet())?],
            )?;
            Ok(())
        })
        .unwrap();
    let archived = format!("workspace-{MAX_ARCHIVE_CANDIDATES:03}");
    let last_examined = format!("workspace-{:03}", MAX_ARCHIVE_CANDIDATES - 1);
    let examined = Cell::new(0);
    let open = |workspace: &str| workspace != archived;
    let run = |actor: &Participant, after| {
        f.engine
            .request(
                ChannelRequest::ListArchivedWorkspaces {
                    after,
                    limit: MAX_PAGE_LIMIT,
                },
                |workspace| {
                    examined.set(examined.get() + 1);
                    Ok(ChannelAuthority {
                        actor: actor.clone(),
                        writable: false,
                        workspace_open: open(workspace),
                        participants: Vec::new(),
                    })
                },
                open,
                || false,
            )
            .unwrap()
    };
    assert_eq!(
        run(&f.user, None),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: Vec::new(),
            next_after: Some(last_examined.clone()),
        }
    );
    assert_eq!(examined.replace(0), MAX_ARCHIVE_CANDIDATES);
    assert_eq!(
        run(&f.user, Some(last_examined)),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: vec![archived.clone()],
            next_after: None,
        }
    );
    assert_eq!(examined.replace(0), 1);
    assert_eq!(
        run(&f.a, None),
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: Vec::new(),
            next_after: None,
        }
    );
    assert_eq!(examined.get(), 1);
}

#[test]
fn internal_authority_failures_are_neither_subscription_closure_nor_archive_filtering() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        quiet(),
    );
    let failure = || std::io::Error::other("registry persistence unavailable");
    let result = f.engine.request(
        subscribe(&id, Some(0), 1, 0),
        |_| Err(failure().into()),
        |_| true,
        || false,
    );
    assert!(result.unwrap_err().is::<std::io::Error>());
    let result = f.engine.request(
        ChannelRequest::ListArchivedWorkspaces {
            after: None,
            limit: 2,
        },
        |workspace| {
            if workspace == OTHER_WORKSPACE {
                return Err(failure().into());
            }
            let mut authority = f.authority(&f.user, workspace)?;
            authority.workspace_open = false;
            Ok(authority)
        },
        |_| false,
        || false,
    );
    assert!(result.unwrap_err().is::<std::io::Error>());
    assert_eq!(
        f.engine
            .request(
                subscribe(&id, Some(0), 1, 0),
                |_| Err(ChannelAccessLost.into()),
                |_| true,
                || false,
            )
            .unwrap(),
        ChannelResponse::Changes {
            cursor: ChannelCursor {
                channel_id: id,
                sequence: 0,
            },
            changes: Vec::new(),
            more: false,
            status: ChannelSubscriptionStatus::Closed,
        }
    );
}

#[test]
fn body_budget_and_cursor_allow_contiguous_forward_catch_up() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    let text = "x".repeat(MAX_POST_BYTES);
    let count = u64::try_from(MAX_HISTORY_BYTES / MAX_POST_BYTES + 4).unwrap();
    for index in 1..=count {
        f.request(send(
            &id,
            ChannelPost {
                text: text.clone(),
                ..post(&format!("large-{index}"))
            },
        ))
        .unwrap();
    }
    f.as_actor(&f.a, mark(&id, count)).unwrap();
    let after = |after| ChannelRequest::History {
        channel_id: id.clone(),
        before: None,
        after: Some(after),
        limit: MAX_PAGE_LIMIT,
    };
    let mut bound = 0;
    let cursor = loop {
        let ChannelResponse::History {
            messages,
            next_before,
            next_after,
            cursor,
        } = f.request(after(bound)).unwrap()
        else {
            panic!("history reply")
        };
        assert!(!messages.is_empty());
        assert!(
            messages
                .iter()
                .map(ChannelMessage::content_bytes)
                .sum::<usize>()
                <= MAX_HISTORY_BYTES
        );
        assert_eq!(next_before, (bound > 0).then_some(bound + 1));
        for entry in messages {
            bound += 1;
            assert_eq!(entry.text, text);
            assert_eq!(entry.message.sequence, bound);
        }
        assert_eq!(next_after, (bound < count).then_some(bound));
        assert_eq!(cursor.sequence, count + 1);
        if next_after.is_none() {
            break cursor;
        }
    };
    assert_eq!(bound, count);
    f.request(send(&id, post("after-snapshot"))).unwrap();
    assert_eq!(
        f.request(subscribe(&id, Some(cursor.sequence), MAX_PAGE_LIMIT, 0))
            .unwrap(),
        ChannelResponse::Changes {
            cursor: ChannelCursor {
                channel_id: id,
                sequence: count + 2
            },
            changes: vec![ChannelChange {
                sequence: count + 2,
                kind: ChannelChangeKind::Post
            }],
            more: false,
            status: ChannelSubscriptionStatus::Active,
        }
    );
}

#[test]
fn compact_history_pages_bound_serialized_mention_metadata() {
    let mut f = Fixture::new();
    f.b.display_name = "\\".repeat(MAX_NAME_BYTES);
    let id = f.wide(quiet());
    let count = u64::from(MAX_PAGE_LIMIT);
    for index in 0..count {
        f.as_actor(
            &f.a,
            send(
                &id,
                ChannelPost {
                    text: "x".repeat(MAX_MENTIONS),
                    mentions: vec![f.b.id.clone()],
                    mention_spans: (0..MAX_MENTIONS as u32)
                        .map(|start| ChannelMentionSpan {
                            start,
                            end: start + 1,
                            participant_id: f.b.id.clone(),
                        })
                        .collect(),
                    ..post(&format!("dense-{index}"))
                },
            ),
        )
        .unwrap();
    }

    let mut bound = 0;
    let mut pages = 0;
    loop {
        let page = f
            .engine
            .with_store(|store| {
                store.compact_history(
                    ChannelRequest::History {
                        channel_id: id.clone(),
                        before: None,
                        after: Some(bound),
                        limit: MAX_PAGE_LIMIT,
                    },
                    &|workspace| f.authority(&f.a, workspace),
                    Some(WORKSPACE),
                )
            })
            .unwrap();
        pages += 1;
        let summaries = page["messages"].as_array().unwrap();
        assert!(!summaries.is_empty());
        assert!(
            summaries
                .iter()
                .map(|summary| serde_json::to_vec(summary).unwrap().len())
                .sum::<usize>()
                <= MAX_HISTORY_BYTES
        );
        for summary in summaries {
            bound += 1;
            assert_eq!(summary["sequence"], bound);
            assert_eq!(
                summary["mention_spans"].as_array().unwrap().len(),
                MAX_MENTIONS
            );
        }
        assert_eq!(
            page["next_after"].as_u64(),
            (bound < count).then_some(bound)
        );
        if page["next_after"].is_null() {
            break;
        }
    }
    assert_eq!(bound, count);
    assert!(pages > 1);
}

#[test]
fn unauthorized_upload_does_not_decode_and_valid_upload_rechecks_before_insert() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    assert!(
        f.as_actor(&f.b, upload(&id, vec![1]))
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    assert!(
        f.as_actor(&f.a, upload(&id, vec![1]))
            .unwrap_err()
            .to_string()
            .contains("PNG")
    );
    let sampled = std::cell::Cell::new(0);
    let error = f
        .engine
        .request(
            upload(&id, png()),
            |workspace| {
                sampled.set(sampled.get() + 1);
                let mut authority = f.authority(&f.a, workspace)?;
                authority.writable = sampled.get() == 1;
                Ok(authority)
            },
            |_| true,
            || false,
        )
        .unwrap_err();
    assert_eq!(sampled.get(), 2);
    assert!(error.to_string().contains("read-only"));
    assert_eq!(
        f.engine
            .with_store(|store| {
                Ok(store
                    .connection
                    .query_row("SELECT count(*) FROM attachments", [], |row| {
                        row.get::<_, u64>(0)
                    })?)
            })
            .unwrap(),
        0
    );
}

#[test]
fn queued_store_admission_observes_poll_deadline_and_cancellation() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    let _writer = f
        .engine
        .acquire_store(std::time::Instant::now(), &|| false)
        .map_err(super::StoreAdmission::into_error)
        .unwrap();
    assert!(
        f.request(subscribe(&id, None, 1, 0))
            .unwrap_err()
            .to_string()
            .contains("store busy")
    );
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let (observed, ready) = crossbeam_channel::bounded(1);
        let (completed, result) = crossbeam_channel::bounded(1);
        let (fixture, channel) = (&f, &id);
        let disconnected = &cancelled;
        scope.spawn(move || {
            completed
                .send(fixture.engine.request(
                    messages(channel, None, 1),
                    |_| panic!("cancelled admission reached channel authority"),
                    |_| true,
                    || {
                        let disconnected = disconnected.load(Ordering::Acquire);
                        let _ = observed.try_send(());
                        disconnected
                    },
                ))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(
            result
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    });
}

#[test]
fn close_retires_old_and_racing_work_before_new_capacity_is_used() {
    let f = Fixture::new();
    let old = f.wide(ChannelSettings::default());
    let active = f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        ChannelSettings::default(),
    );
    let original = post("archived");
    let accepted = f.request(send(&old, original.clone())).unwrap();
    let archived = posted(accepted.clone());
    f.sql(
        "WITH RECURSIVE fill(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM fill WHERE x<4094)
         INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed)
         SELECT p.channel_id,p.id,'extra-'||x,'extra-launch-'||x,0 FROM fill CROSS JOIN posts p;",
    );
    let active_open = std::cell::Cell::new(true);
    let workspace_open = |workspace: &str| workspace == OTHER_WORKSPACE && active_open.get();
    f.engine.retire_workspace();
    let active_authority = |_: &str| f.authority(&f.user, WORKSPACE);
    let next = posted(
        f.engine
            .request(
                send(&active, post("new-work")),
                active_authority,
                workspace_open,
                || false,
            )
            .unwrap(),
    );
    assert_eq!(next.sequence, 1);
    assert_eq!(f.work(&next.id).iter().filter(|entry| entry.3).count(), 2);
    assert_eq!(
        f.work(&archived.id).iter().filter(|entry| !entry.3).count(),
        4096
    );
    let retry = f
        .engine
        .request(
            send(&old, original),
            |workspace| {
                let mut authority = f.authority(&f.user, workspace)?;
                authority.workspace_open = false;
                Ok(authority)
            },
            workspace_open,
            || false,
        )
        .unwrap();
    assert_eq!(retry, accepted);
    let racing = posted(
        f.engine
            .acquire_store(std::time::Instant::now(), &|| false)
            .map_err(super::StoreAdmission::into_error)
            .unwrap()
            .run(&workspace_open, |store| {
                let (response, changed) = store.request(
                    send(&active, post("racing-close")),
                    active_authority,
                    super::ChannelRoutes {
                        workspace: None,
                        exact_mentions: None,
                        publish: &|_| {},
                        binding_live: &|_, _| true,
                        direct_peer: &|_| Ok(None),
                        fence_user_mentions: &|_, _| unreachable!("no selected user mentions"),
                    },
                    None,
                    |store, workspace| {
                        f.engine.flush_retirements(store, &workspace_open)?;
                        let closed = !workspace_open(workspace);
                        // Publish closure after the writer's last capacity fence, before its commit.
                        active_open.set(false);
                        f.engine.retire_workspace();
                        Ok(closed)
                    },
                )?;
                assert!(changed);
                Ok(response)
            })
            .unwrap(),
    );
    assert_eq!(racing.sequence, 2);
    for id in [&racing.id, &next.id] {
        let work = f.work(id);
        assert_eq!(work.len(), 2);
        assert!(work.iter().all(|entry| !entry.3));
    }
}

#[test]
fn workspace_close_wakes_subscription_and_archived_catchup_ends() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    f.request(send(&id, post("before-close-1"))).unwrap();
    f.request(send(&id, post("before-close-2"))).unwrap();
    let open = AtomicBool::new(true);
    let is_open = |_: &str| open.load(Ordering::Acquire);
    let authority = |workspace: &str| {
        let mut authority = f.authority(&f.user, workspace)?;
        authority.workspace_open = is_open(workspace);
        Ok(authority)
    };
    thread::scope(|scope| {
        let (observed, ready) = crossbeam_channel::bounded(1);
        let (completed, result) = crossbeam_channel::bounded(1);
        let (fixture, channel) = (&f, &id);
        scope.spawn(move || {
            completed
                .send(fixture.engine.request(
                    subscribe(channel, Some(2), 1, MAX_WAIT_MS),
                    |workspace| {
                        let sampled = authority(workspace);
                        let _ = observed.try_send(());
                        sampled
                    },
                    is_open,
                    || false,
                ))
                .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        open.store(false, Ordering::Release);
        f.engine.retire_workspace();
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            ChannelResponse::Changes {
                cursor: ChannelCursor {
                    channel_id: id.clone(),
                    sequence: 2
                },
                changes: Vec::new(),
                more: false,
                status: ChannelSubscriptionStatus::Closed,
            }
        );
    });
    for (from, status, more) in [
        (0, ChannelSubscriptionStatus::Active, true),
        (1, ChannelSubscriptionStatus::Closed, false),
    ] {
        assert_eq!(
            f.engine
                .request(
                    subscribe(&id, Some(from), 1, MAX_WAIT_MS),
                    authority,
                    is_open,
                    || false
                )
                .unwrap(),
            ChannelResponse::Changes {
                cursor: ChannelCursor {
                    channel_id: id.clone(),
                    sequence: from + 1
                },
                changes: vec![ChannelChange {
                    sequence: from + 1,
                    kind: ChannelChangeKind::Post
                }],
                more,
                status,
            }
        );
    }
}

#[test]
fn visibility_index_migration_preserves_member_access_and_durable_history() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        quiet(),
    );
    let original = post("migrated");
    let accepted = f.as_actor(&f.a, send(&id, original.clone())).unwrap();
    f.drop_wake_schema();
    f.sql(
        "DROP INDEX pending_channel_attention;
         DROP TRIGGER catalog_channel_created;
         DROP TRIGGER catalog_channel_updated;
         DROP TABLE catalog_workspaces;
         DROP TABLE catalog_events;
         DROP TABLE catalog_identity;
         DROP TABLE direct_channels;
         DROP INDEX visible_explicit_channels;
         DROP TABLE staged_attachments;
         DROP TABLE attachment_budget;
         DROP INDEX workspace_wide_channels;
         ALTER TABLE members DROP COLUMN workspace_id;
         PRAGMA user_version=1;",
    );
    let f = f.reopen();
    assert!(f.list(&f.a).iter().any(|channel| channel.id == id));
    assert!(!f.list(&f.b).iter().any(|channel| channel.id == id));
    assert_eq!(f.as_actor(&f.a, send(&id, original)).unwrap(), accepted);
    let (sync, version) = f
        .engine
        .with_store(|store| {
            Ok((
                store
                    .connection
                    .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))?,
                store
                    .connection
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            ))
        })
        .unwrap();
    assert_eq!((sync, version), (3, 16));
}

#[test]
fn version_seven_migration_keeps_history_and_staged_png_but_retires_pending_work() {
    let f = Fixture::new();
    let id = f.wide(waking());
    let original = post("version-seven");
    let accepted = f.request(send(&id, original.clone())).unwrap();
    let message = posted(accepted.clone());
    let bytes = png();
    let staged = uploaded(f.request(upload(&id, bytes.clone())).unwrap());
    let pending = f.work(&message.id);
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|work| work.3));
    // Version 7 had this same history and staging schema, without the wake tables and columns.
    f.drop_wake_schema();
    f.sql("DROP TABLE direct_channels; PRAGMA user_version=7;");

    let f = f.reopen();
    assert_eq!(f.request(send(&id, original)).unwrap(), accepted);
    assert!(matches!(
        f.request(messages(&id, None, 2)).unwrap(),
        ChannelResponse::History { messages, .. } if messages[0].message == message
    ));
    assert_eq!(f.staged_bytes(), u64::from(staged.byte_len));
    assert_eq!(
        f.request(ChannelRequest::GetAttachment {
            channel_id: id.clone(),
            attachment_id: staged.id.clone(),
        })
        .unwrap(),
        ChannelResponse::Attachment {
            attachment: staged.clone(),
            bytes: bytes.into()
        }
    );
    let retired = f.work(&message.id);
    assert_eq!(retired.len(), pending.len());
    assert!(retired.iter().all(|work| !work.3));
    assert!(f.claim(&f.wake_claim("old-work", 1)).is_none());
    let mut next = post("after-migration");
    next.attachments.push(staged.id);
    f.request(send(&id, next)).unwrap();
    assert_eq!(f.staged_bytes(), 0);
    assert_eq!(
        f.claim(&f.wake_claim("fresh-work", 2))
            .unwrap()
            .pending_count,
        1
    );
}

#[test]
fn version_eight_migration_separates_legacy_revocation_from_native_outcomes() {
    let f = Fixture::new();
    let id = f.create(
        &f.user,
        WORKSPACE,
        MembershipMode::Explicit,
        vec![f.a.id.clone()],
        waking(),
    );
    let accepted = f.wake_claim("legacy-accepted", 1);
    f.request(send(&id, post("legacy-accepted"))).unwrap();
    f.claim(&accepted).unwrap();
    assert!(f.settle(&accepted, "accepted", None, || Ok(())).unwrap());

    let revoked = f.wake_claim("legacy-revoked", 2);
    f.request(send(&id, post("legacy-revoked"))).unwrap();
    f.claim(&revoked).unwrap();
    f.sql("UPDATE channel_batches SET outcome='revoked' WHERE id='legacy-revoked';");
    let unresolved = f.wake_claim("legacy-unresolved", 3);
    f.request(send(&id, post("legacy-unresolved"))).unwrap();
    f.claim(&unresolved).unwrap();
    f.request(send(&id, post("legacy-pending"))).unwrap();

    f.restore_legacy_wake_indexes();
    f.sql("DROP TABLE direct_channels; PRAGMA user_version=8;");
    let f = f.reopen();
    assert_eq!(
        f.wake_record("legacy-accepted"),
        (false, Some("accepted".into()), None)
    );
    assert_eq!(
        f.wake_record("legacy-revoked"),
        (true, Some("unknown".into()), None)
    );
    assert_eq!(
        f.wake_record("legacy-unresolved"),
        (false, Some("unknown".into()), None)
    );
    let accepted = f.wake_claim("legacy-accepted", 1);
    assert!(f.wake_lookup(&accepted, None).is_ok());
    let revoked = f.wake_claim("legacy-revoked", 2);
    assert!(
        f.wake_lookup(&revoked, None)
            .unwrap_err()
            .is::<ChannelAccessLost>()
    );
    assert!(
        !f.settle(&revoked, "failed", Some("stale"), || unreachable!())
            .unwrap()
    );
    assert!(f.claim(&f.wake_claim("no-old-work", 4)).is_none());
}

#[test]
fn ordered_version_nine_migration_preserves_revocation_and_native_outcomes() {
    let f = Fixture::new();
    let channel = f.wide(waking());
    let accepted = f.wake_claim("accepted-v9", 1);
    let message = posted(f.request(send(&channel, post("accepted"))).unwrap());
    f.claim(&accepted).unwrap();
    assert!(f.settle(&accepted, "accepted", None, || Ok(())).unwrap());
    let revoked = f.wake_claim("revoked-v9", 2);
    f.request(send(&channel, post("revoked"))).unwrap();
    f.claim(&revoked).unwrap();
    f.sql("UPDATE channel_batches SET revoked=1 WHERE id='revoked-v9';");
    f.request(send(&channel, post("pending"))).unwrap();
    f.drop_delivery_receipt_schema();
    f.sql("DROP TABLE direct_channels; PRAGMA user_version=9;");

    let f = f.reopen();
    assert_eq!(
        f.wake_record("accepted-v9"),
        (false, Some("accepted".into()), None)
    );
    assert_eq!(
        f.wake_record("revoked-v9"),
        (true, Some("unknown".into()), None)
    );
    let reply = f
        .wake_lookup(&f.wake_claim("accepted-v9", 1), Some(64))
        .unwrap();
    assert_eq!(reply["post"]["message"]["id"], message.id.as_ref());
    assert!(f.claim(&f.wake_claim("no-replay", 3)).is_none());
}

#[test]
fn direct_version_nine_migration_keeps_history_and_uses_the_ordered_queue() {
    let f = Fixture::new();
    let post_to_a = |f: &Fixture, key: &str| {
        let mut direct_post = post(key);
        direct_post.text = key.into();
        let (response, changed) = f
            .engine
            .with_store(|store| {
                store.post_direct(
                    direct_post,
                    f.authority(&f.b, WORKSPACE)?,
                    "launch-a",
                    WORKSPACE,
                    super::ChannelRoutes {
                        workspace: Some(WORKSPACE),
                        exact_mentions: None,
                        publish: &|_| {},
                        binding_live: &|_, _| true,
                        direct_peer: &|_| Ok(None),
                    },
                    || {
                        Ok(WorkspaceParticipant {
                            actor: f.a.clone(),
                            launch_id: Some("launch-a".into()),
                            binding_revision: 1,
                        })
                    },
                )
            })
            .unwrap();
        assert!(changed);
        response
    };
    let ChannelResponse::Posted {
        channel_id: Some(direct),
        message: old,
        ..
    } = post_to_a(&f, "old-direct")
    else {
        panic!("direct post receipt")
    };
    // Before the cutover, direct delivery was stored only in agent_state.
    f.sql("DELETE FROM notification_work WHERE channel_id IN (SELECT channel_id FROM direct_channels);");
    let shared = f.wide(waking());
    let revoked = f.wake_claim("old-revoked", 1);
    f.request(send(&shared, post("old-revoked"))).unwrap();
    f.claim(&revoked).unwrap();
    f.request(send(&shared, post("old-pending"))).unwrap();
    f.restore_legacy_wake_indexes();
    f.sql(
        "UPDATE channel_batches SET outcome='revoked' WHERE id='old-revoked';
         CREATE TABLE agent_state (singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot BLOB NOT NULL) STRICT;
         INSERT INTO agent_state VALUES(1,X'00');
         PRAGMA user_version=9;",
    );

    let f = f.reopen();
    assert!(matches!(
        f.as_actor(&f.a, messages(&direct, None, 2)).unwrap(),
        ChannelResponse::History { messages, .. }
            if messages.len() == 1 && messages[0].message == old && messages[0].text == "old-direct"
    ));
    assert!(
        f.engine
            .with_store(|store| {
                Ok(store.connection.query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='agent_state')",
                    [],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    );
    assert_eq!(
        f.wake_record("old-revoked"),
        (true, Some("unknown".into()), None)
    );
    assert!(f.claim(&f.wake_claim("no-replay", 2)).is_none());
    f.request(send(&shared, post("new-shared"))).unwrap();
    let ChannelResponse::Posted {
        channel_id: Some(reused),
        message: next,
        ..
    } = post_to_a(&f, "new-direct")
    else {
        panic!("direct post receipt")
    };
    assert_eq!(reused, direct);
    assert_eq!(next.sequence, 2);
    let first_batch = f.claim(&f.wake_claim("new-shared", 3)).unwrap();
    let second_batch = f.claim(&f.wake_claim("new-direct", 4)).unwrap();
    assert_eq!(first_batch.channel_id, shared);
    assert_eq!(second_batch.channel_id, direct);
    assert_eq!(second_batch.addressed_message_id, Some(next.id));
}

#[test]
fn first_direct_post_accepts_descending_pair_and_replays_without_resolving_target() {
    let f = Fixture::new();
    let mut original = post("descending-pair");
    original.mentions = vec![ParticipantId(super::agent_mention_id("launch-b", 1))];
    let send = |resolve_target: fn(&Fixture) -> Result<WorkspaceParticipant>| {
        f.engine
            .with_store(|store| {
                let mut sender = f.authority(&f.b, WORKSPACE)?;
                sender.participants.clear();
                store.post_direct(
                    original.clone(),
                    sender,
                    "launch-a",
                    WORKSPACE,
                    super::ChannelRoutes {
                        workspace: Some(WORKSPACE),
                        exact_mentions: Some(("launch-b", 1)),
                        publish: &|_| {},
                        binding_live: &|_, _| true,
                        direct_peer: &|_| Ok(None),
                    },
                    || resolve_target(&f),
                )
            })
            .unwrap()
    };
    let (receipt, committed) = send(|f| {
        Ok(WorkspaceParticipant {
            actor: f.a.clone(),
            launch_id: Some("launch-a".into()),
            binding_revision: 1,
        })
    });
    assert!(committed);
    let ChannelResponse::Posted {
        message,
        wake_count: 1,
        ..
    } = &receipt
    else {
        panic!("first direct post must queue one wake");
    };
    assert_eq!(message.mentions, std::slice::from_ref(&f.b.id));
    assert_eq!(
        send(|_| anyhow::bail!("recipient has retired")),
        (receipt, false)
    );
}

#[test]
fn legacy_attachment_migration_caps_staged_bytes_and_retains_posted_history() {
    let f = Fixture::new();
    let original = f.wide(quiet());
    let other = f.create(
        &f.user,
        OTHER_WORKSPACE,
        MembershipMode::WorkspaceWide,
        Vec::new(),
        quiet(),
    );
    let sample = png();
    let kept = uploaded(f.request(upload(&original, sample.clone())).unwrap());
    let resumed = uploaded(f.request(upload(&original, sample.clone())).unwrap());
    let mut attached = post("legacy-posted");
    attached.attachments.push(kept.id.clone());
    let message = posted(f.request(send(&original, attached)).unwrap());
    // A legacy store has the full budget plus one extra unreferenced row.
    // Only the two small real PNGs will be fetched or posted.
    f.restore_legacy_wake_indexes();
    f.sql(
        "DROP TABLE staged_attachments;
         DROP TABLE attachment_budget;
         DROP INDEX pending_channel_attention;
         DROP TRIGGER catalog_channel_created;
         DROP TRIGGER catalog_channel_updated;
         DROP TABLE catalog_workspaces;
         DROP TABLE catalog_events;
         DROP TABLE catalog_identity;
         PRAGMA user_version=4;",
    );
    f.engine
        .with_store(|store| {
            store.connection.execute(
                "WITH RECURSIVE fill(n) AS (
                     VALUES(1) UNION ALL SELECT n+1 FROM fill WHERE n<?4
                 )
                 INSERT INTO attachments(id,channel_id,descriptor,bytes)
                 SELECT printf('legacy-%02d',n),?1,'{}',
                        zeroblob(?2-CASE WHEN n=1 THEN ?3 ELSE 0 END) FROM fill
                 UNION ALL SELECT '!legacy-overflow',?1,'{}',zeroblob(1)",
                rusqlite::params![
                    original.as_ref(),
                    MAX_ATTACHMENT_BYTES,
                    resumed.byte_len,
                    MAX_STAGED_BYTES / MAX_ATTACHMENT_BYTES as u64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let f = f.reopen();
    assert_eq!(f.staged_bytes(), MAX_STAGED_BYTES);
    let fetch =
        |channel_id: &ChannelId, attachment_id: &AttachmentId| ChannelRequest::GetAttachment {
            channel_id: channel_id.clone(),
            attachment_id: attachment_id.clone(),
        };
    assert_eq!(
        f.request(fetch(&original, &kept.id)).unwrap(),
        ChannelResponse::Attachment {
            attachment: kept.clone(),
            bytes: sample.clone().into(),
        }
    );
    assert_eq!(
        f.request(fetch(&original, &resumed.id)).unwrap(),
        ChannelResponse::Attachment {
            attachment: resumed.clone(),
            bytes: sample.clone().into(),
        }
    );
    assert!(
        f.request(upload(&other, sample.clone()))
            .unwrap_err()
            .to_string()
            .contains("capacity")
    );
    let mut attach_resumed = post("legacy-pending");
    attach_resumed.attachments.push(resumed.id);
    f.request(send(&original, attach_resumed)).unwrap();
    assert_eq!(
        f.staged_bytes(),
        MAX_STAGED_BYTES - u64::from(resumed.byte_len)
    );
    let expiring = uploaded(f.request(upload(&other, sample.clone())).unwrap());
    let f = f.reopen();
    assert_eq!(f.staged_bytes(), MAX_STAGED_BYTES);
    assert!(f.request(fetch(&other, &expiring.id)).is_ok());
    f.engine
        .with_store(|store| {
            store.connection.execute(
                "UPDATE staged_attachments SET expires_ms=0 WHERE id=?1",
                [expiring.id.as_ref()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(f.request(fetch(&other, &expiring.id)).is_err());
    let mut expired_post = post("expired");
    expired_post.attachments.push(expiring.id.clone());
    assert!(f.request(send(&other, expired_post)).is_err());
    f.request(upload(&other, sample)).unwrap();
    assert_eq!(f.staged_bytes(), MAX_STAGED_BYTES);
    f.engine
        .with_store(|store| {
            let removed: u64 = store.connection.query_row(
                "SELECT count(*) FROM attachments WHERE id IN ('!legacy-overflow',?1)",
                [expiring.id.as_ref()],
                |row| row.get(0),
            )?;
            assert_eq!(removed, 0);
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        f.request(messages(&original, None, 2)).unwrap(),
        ChannelResponse::History { messages, .. }
            if messages[0].message == message && messages[0].attachments == [kept]
    ));
}

#[test]
fn upload_reclaims_a_bounded_oldest_first_batch_of_staged_rows() {
    let f = Fixture::new();
    let channel = f.wide(quiet());
    f.engine
        .with_store(|store| {
            let tx = store.connection.transaction()?;
            tx.execute(
                "WITH RECURSIVE fill(n) AS (
                     VALUES(0) UNION ALL SELECT n+1 FROM fill WHERE n<?2
                 )
                 INSERT INTO attachments(id,channel_id,descriptor,bytes)
                 SELECT printf('legacy-%03d',n),?1,'{}',zeroblob(1) FROM fill",
                rusqlite::params![channel.as_ref(), MAX_STAGING_RECLAIM + 1],
            )?;
            // Row zero has the latest expiry; ties among the rest use ID order.
            tx.execute_batch(
                "INSERT INTO staged_attachments(id,expires_ms,byte_len)
                 SELECT id,CASE WHEN id='legacy-000' THEN 1 ELSE 0 END,1 FROM attachments;
                 UPDATE attachment_budget SET unposted_bytes=(SELECT count(*) FROM attachments);",
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let first = uploaded(f.request(upload(&channel, png())).unwrap());
    assert_eq!(f.staged_bytes(), u64::from(first.byte_len) + 2);
    let survivors = f
        .engine
        .with_store(|store| {
            let mut statement = store.connection.prepare(
                "SELECT id FROM staged_attachments WHERE expires_ms<=1 ORDER BY expires_ms,id",
            )?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    assert_eq!(
        survivors,
        [
            format!("legacy-{:03}", MAX_STAGING_RECLAIM + 1),
            "legacy-000".into(),
        ]
    );
    let second = uploaded(f.request(upload(&channel, png())).unwrap());
    assert_eq!(
        f.staged_bytes(),
        u64::from(first.byte_len + second.byte_len)
    );
}

#[test]
fn catalog_pages_stay_at_their_baseline_and_resume_authoritative_unread() {
    const FIRST_WORKSPACE: &str = "workspace-baseline";
    let f = Fixture::new();
    let id = f.wide(ChannelSettings {
        attention_reports: true,
        ..quiet()
    });
    let report = |key| {
        posted(
            f.as_actor(
                &f.a,
                send(
                    &id,
                    ChannelPost {
                        intent: PostIntent::Report,
                        ..post(key)
                    },
                ),
            )
            .unwrap(),
        )
    };
    let first = report("first report");
    let workspaces = || {
        Ok([FIRST_WORKSPACE.into(), WORKSPACE.into()]
            .into_iter()
            .collect())
    };
    let fetch = |read: CatalogRead<'_>| {
        f.engine
            .catalog_request(read, f.user.id.as_ref(), 1, 0, workspaces, || false)
            .unwrap()
    };
    let ChannelResponse::CatalogSnapshot {
        mut channels,
        cursor,
        mut next_after,
    } = fetch(CatalogRead::Snapshot(None))
    else {
        panic!("expected catalog baseline");
    };
    assert_eq!(channels[0].workspace_id, FIRST_WORKSPACE);
    assert_ne!(channels[0].id, id);
    let channel_created_later = f.create(
        &f.user,
        FIRST_WORKSPACE,
        MembershipMode::WorkspaceWide,
        vec![],
        quiet(),
    );
    f.request(ChannelRequest::MarkRead {
        channel_id: id.clone(),
        through: first.sequence,
    })
    .unwrap();
    let second = report("second report");
    while let Some(after) = next_after {
        let ChannelResponse::CatalogSnapshot {
            channels: page,
            cursor: same,
            next_after: next,
        } = fetch(CatalogRead::Snapshot(Some(&after)))
        else {
            panic!("expected stable continuation");
        };
        assert_eq!(same, cursor);
        assert_eq!(page.len(), 1);
        channels.extend(page);
        next_after = next;
    }
    assert_eq!(channels.len(), 5);
    assert_eq!(
        channels
            .iter()
            .filter(|c| c.workspace_id == FIRST_WORKSPACE)
            .count(),
        2
    );
    let baseline = channels.iter().find(|c| c.id == id).unwrap();
    assert_eq!(
        (
            baseline.last_sequence,
            baseline.read_through,
            baseline.unread_attention
        ),
        (1, 0, true)
    );
    assert!(channels.iter().all(|c| c.id != channel_created_later));

    let mut cursor = cursor;
    let mut created = false;
    loop {
        let ChannelResponse::CatalogChanges {
            changes,
            cursor: next,
            more,
        } = fetch(CatalogRead::Changes(&cursor))
        else {
            panic!("expected catalog catchup");
        };
        assert_eq!(changes.len(), 1);
        assert!(next.sequence > cursor.sequence);
        cursor = next;
        let ChannelCatalogChange::Upsert { channel } = &changes[0] else {
            panic!("expected channel upsert");
        };
        if channel.id == id {
            assert_eq!(
                (
                    channel.last_sequence,
                    channel.read_through,
                    channel.unread_attention
                ),
                (2, 1, true)
            );
        } else {
            assert_eq!(channel.id, channel_created_later);
            created = true;
        }
        if !more {
            break;
        }
    }
    assert!(created);
    f.as_actor(
        &f.a,
        ChannelRequest::MarkRead {
            channel_id: id.clone(),
            through: second.sequence,
        },
    )
    .unwrap();
    assert_eq!(
        fetch(CatalogRead::Changes(&cursor)),
        ChannelResponse::CatalogChanges {
            changes: vec![],
            cursor: cursor.clone(),
            more: false
        }
    );
    f.request(ChannelRequest::MarkRead {
        channel_id: id,
        through: second.sequence,
    })
    .unwrap();
    let ChannelResponse::CatalogChanges { changes, .. } = fetch(CatalogRead::Changes(&cursor))
    else {
        panic!("expected user read update");
    };
    assert!(
        matches!(changes.as_slice(), [ChannelCatalogChange::Upsert { channel }]
        if channel.read_through == second.sequence && !channel.unread_attention)
    );
}

#[test]
fn catalog_retirement_only_reconciles_topology_for_workspace_closure() {
    let f = Fixture::new();
    let fetch = |workspaces: &[&str]| {
        f.engine
            .catalog_request(
                CatalogRead::Snapshot(None),
                f.user.id.as_ref(),
                8,
                0,
                || {
                    Ok(workspaces
                        .iter()
                        .map(|workspace| (*workspace).into())
                        .collect())
                },
                || false,
            )
            .unwrap()
    };
    let baseline = fetch(&[WORKSPACE, OTHER_WORKSPACE]);
    f.engine.reconcile_retired_work();
    // The topology provider's empty set is ignored when only pending work was retired.
    assert_eq!(fetch(&[]), baseline);
    f.engine.retire_workspace();
    let ChannelResponse::CatalogSnapshot { channels, .. } = fetch(&[WORKSPACE]) else {
        panic!("expected fresh catalog after closure");
    };
    assert_eq!(channels.len(), 2);
    assert!(
        channels
            .iter()
            .all(|channel| channel.workspace_id == WORKSPACE)
    );
}

#[test]
fn catalog_subscription_returns_progress_past_filtered_changes() {
    let f = Fixture::new();
    let id = f.wide(quiet());
    let workspaces = || Ok([OTHER_WORKSPACE.into()].into_iter().collect());
    let ChannelResponse::CatalogSnapshot { cursor, .. } = f
        .engine
        .catalog_request(
            CatalogRead::Snapshot(None),
            f.user.id.as_ref(),
            8,
            0,
            workspaces,
            || false,
        )
        .unwrap()
    else {
        panic!("expected catalog baseline");
    };
    // This journal entry is in a workspace outside the live catalog.
    f.engine
        .with_store(|store| {
            store.connection.execute(
                "UPDATE channels SET workspace_wide=1-workspace_wide WHERE id=?1",
                [id.as_ref()],
            )?;
            Ok(())
        })
        .unwrap();
    thread::scope(|scope| {
        let (completed, result) = crossbeam_channel::bounded(1);
        let (fixture, cursor) = (&f, &cursor);
        scope.spawn(move || {
            let response = fixture.engine.catalog_request(
                CatalogRead::Changes(cursor),
                fixture.user.id.as_ref(),
                8,
                MAX_WAIT_MS,
                workspaces,
                || false,
            );
            let _ = completed.send(response);
        });
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            ChannelResponse::CatalogChanges {
                cursor: ChannelCatalogCursor {
                    store_id: cursor.store_id.clone(),
                    sequence: cursor.sequence + 1,
                },
                changes: vec![],
                more: false,
            }
        );
    });
}

#[test]
fn catalog_overrun_resets_without_losing_history_or_unread() {
    let f = Fixture::new();
    let id = f.wide(ChannelSettings {
        attention_questions: true,
        ..quiet()
    });
    let workspaces = || Ok([WORKSPACE.into()].into_iter().collect());
    let fetch = |read: CatalogRead<'_>| {
        f.engine
            .catalog_request(read, f.user.id.as_ref(), 1, 0, workspaces, || false)
            .unwrap()
    };
    let ChannelResponse::CatalogSnapshot {
        cursor,
        next_after: Some(after),
        ..
    } = fetch(CatalogRead::Snapshot(None))
    else {
        panic!("expected paged baseline");
    };
    f.as_actor(
        &f.a,
        send(
            &id,
            ChannelPost {
                intent: PostIntent::Question,
                ..post("still unread")
            },
        ),
    )
    .unwrap();
    // Exercise the real bounded journal without one fsync per synthetic settings update.
    f.engine
        .with_store(|store| {
            let tx = store.connection.transaction()?;
            {
                let mut update =
                    tx.prepare("UPDATE channels SET workspace_wide=1-workspace_wide WHERE id=?1")?;
                for _ in 0..4096 {
                    update.execute([id.as_ref()])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fetch(CatalogRead::Changes(&cursor)),
        ChannelResponse::CatalogReset {}
    );
    assert_eq!(
        fetch(CatalogRead::Snapshot(Some(&after))),
        ChannelResponse::CatalogReset {}
    );
    let user_catalog = f
        .engine
        .catalog_request(
            CatalogRead::Snapshot(None),
            f.user.id.as_ref(),
            4,
            0,
            workspaces,
            || false,
        )
        .unwrap();
    let ChannelResponse::CatalogSnapshot {
        channels, cursor, ..
    } = user_catalog
    else {
        panic!("expected fresh catalog");
    };
    assert!(
        channels
            .iter()
            .any(|c| c.id == id && c.unread_attention && c.last_sequence == 1)
    );
    assert_eq!(
        fetch(CatalogRead::Changes(&cursor)),
        ChannelResponse::CatalogChanges {
            changes: vec![],
            cursor: cursor.clone(),
            more: false,
        }
    );
    let ChannelResponse::History { messages, .. } = f.request(messages(&id, None, 4)).unwrap()
    else {
        panic!("expected history");
    };
    assert_eq!(messages[0].text, post("still unread").text);
}

#[test]
fn pending_channel_attention_survives_migrating_pre_ordered_catalog() {
    let f = Fixture::new();
    let id = f.wide(ChannelSettings {
        attention_reports: true,
        ..quiet()
    });
    let post = posted(
        f.as_actor(
            &f.a,
            send(
                &id,
                ChannelPost {
                    intent: PostIntent::Report,
                    ..post("recoverable report")
                },
            ),
        )
        .unwrap(),
    );
    f.drop_ordered_wake_schema();
    f.sql(
        "DROP INDEX pending_channel_attention;
         ALTER TABLE notification_work DROP COLUMN attention_posted_ms;
         ALTER TABLE notification_work DROP COLUMN attention_origin;
         CREATE INDEX pending_channel_attention ON notification_work(message_id)
            WHERE pending=1 AND launch_id IS NULL;
         PRAGMA user_version=14;",
    );
    let Fixture { root, engine, .. } = f;
    drop(engine);
    let read_pending = || {
        let store = super::store::AttentionStore::open(root.path())
            .unwrap()
            .unwrap();
        let event = store.next().unwrap().unwrap();
        assert_eq!(event.channel_id, id);
        assert_eq!(event.message_id, post.id);
        assert_eq!(event.posted_at_ms, post.posted_at_ms);
        assert!(event.origin.is_none());
        store
    };
    read_pending();
    let reopened = ChannelEngine::new(root.path().to_owned());
    reopened.with_store(|_| Ok(())).unwrap();
    let store = read_pending();
    store.finish(&post.id, false).unwrap();
    assert!(store.next().unwrap().is_none());
}

#[test]
fn attention_survives_last_workspace_closure_and_waits_for_host_ack() {
    use crate::notifications::NotificationService;
    use crate::registry::notification_capture::ChannelAttentionWorker;
    use bkpr_protocol::{
        ChannelAttentionKind, ChannelNotificationSource, NotificationPage, NotificationQuery,
        NotificationRequest, NotificationSource, SessionInfo,
    };
    use std::time::Instant;

    let mut f = Fixture::new();
    let origin = SessionInfo {
        name: format!("x{}", "é".repeat(600)),
        session_id: "origin-session".into(),
        daemon_id: "origin-daemon".into(),
        session_instance_id: "origin-instance".into(),
    };
    let sessions = f.root.path().join("sessions");
    let session_root = sessions.join(bkpr_protocol::session_key(&origin.name).unwrap());
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&session_root)
        .unwrap();
    f.engine = ChannelEngine::for_session(session_root.clone(), &origin);
    let channel = f.wide(ChannelSettings {
        attention_reports: true,
        agent_wake: WakePolicy::All,
        ..quiet()
    });
    let committed = posted(
        f.engine
            .request(
                send(
                    &channel,
                    ChannelPost {
                        intent: PostIntent::Report,
                        ..post("accepted-before-workspace-closure")
                    },
                ),
                |workspace| f.authority(&f.a, workspace),
                |_| false, // Closed after authority admitted the post.
                || false,
            )
            .unwrap(),
    );
    let work = f.work(&committed.id);
    assert!(
        work.iter()
            .any(|(_, launch, _, pending)| launch.is_none() && *pending)
    );
    assert!(
        work.iter()
            .any(|(_, launch, _, pending)| launch.is_some() && !*pending)
    );
    let Fixture { root, engine, .. } = f;
    drop(engine);

    let notifications = root.path().join("notifications");
    let host = NotificationService::open(&notifications).unwrap();
    host.shutdown().unwrap();
    assert!(
        super::deliver_channel_attention(
            &session_root,
            &host,
            || panic!("origin must be persisted"),
            &mut 0,
            &|| false,
        )
        .unwrap()
        .is_some()
    );
    let outbox = super::store::AttentionStore::open(&session_root)
        .unwrap()
        .unwrap();
    let failed = outbox.next().unwrap().unwrap();
    assert_eq!(failed.message_id, committed.id);
    let source = ChannelNotificationSource {
        session: failed.origin.unwrap(),
        workspace_id: failed.workspace_id,
        channel_id: failed.channel_id,
        message_id: committed.id,
        sequence: committed.sequence,
        intent: ChannelAttentionKind::Report,
    };
    assert_eq!(source.session.session_id, origin.session_id);
    assert_eq!(source.session.daemon_id, origin.daemon_id);
    assert_eq!(
        source.session.session_instance_id,
        origin.session_instance_id
    );
    assert_eq!(source.session.name, origin.name[..1023]);

    let host = NotificationService::open(&notifications).unwrap();
    // Simulate host commit followed by loss of the ACK before clearing the outbox.
    host.record_channel_attention(source.clone(), committed.posted_at_ms, &|| true)
        .unwrap();
    assert!(outbox.next().unwrap().is_some());
    let worker = ChannelAttentionWorker::host(sessions, host.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while outbox.next().unwrap().is_some() {
        assert!(
            Instant::now() < deadline,
            "closed session's outbox did not drain"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let page: NotificationPage = serde_json::from_value(
        host.request(
            NotificationRequest::Inspect {
                query: NotificationQuery {
                    include_channel_attention: true,
                    ..Default::default()
                },
                after: None,
            },
            None,
            &|| true,
        )
        .unwrap()
        .into_json()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(page.notifications.len(), 1);
    assert_eq!(
        page.notifications[0].source,
        NotificationSource::Channel(source),
    );
    worker.stop();
    host.shutdown().unwrap();
}

#[test]
fn blocked_host_rechecks_at_the_oldest_post_expiry() {
    use super::store::{AttentionStore, unix_time_ms};
    use crate::notifications::{CHANNEL_ATTENTION_RETRY_MS, NotificationService};

    let f = Fixture::new();
    let channel = f.wide(ChannelSettings {
        attention_reports: true,
        ..quiet()
    });
    let send_report = |key| {
        posted(
            f.as_actor(
                &f.a,
                send(
                    &channel,
                    ChannelPost {
                        intent: PostIntent::Report,
                        ..post(key)
                    },
                ),
            )
            .unwrap(),
        )
        .id
    };
    let mut ids = [send_report("one"), send_report("two")];
    ids.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
    let [younger, older] = ids;
    let set_timestamp = |id: &MessageId, posted_at_ms| {
        f.engine
            .with_store(|store| {
                let tx = store.connection.transaction()?;
                tx.execute(
                    "UPDATE posts SET summary=json_set(summary,'$.posted_at_ms',?2) WHERE id=?1",
                    rusqlite::params![id.as_ref(), posted_at_ms],
                )?;
                tx.execute(
                    "UPDATE notification_work SET attention_posted_ms=?2 WHERE message_id=?1",
                    rusqlite::params![id.as_ref(), posted_at_ms],
                )?;
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    };
    let now = unix_time_ms().unwrap();
    set_timestamp(&younger, now);
    let nearly_expired = now - CHANNEL_ATTENTION_RETRY_MS + 60_000;
    set_timestamp(&older, nearly_expired);

    let service = NotificationService::open(&f.root.path().join("notifications")).unwrap();
    let next_due = || {
        let mut host_retry_at = u64::MAX;
        super::deliver_channel_attention(
            f.root.path(),
            &service,
            || panic!("a blocked host must not try to deliver"),
            &mut host_retry_at,
            &|| false,
        )
        .unwrap()
    };
    assert_eq!(
        next_due(),
        Some(nearly_expired + CHANNEL_ATTENTION_RETRY_MS)
    );
    set_timestamp(&older, now - CHANNEL_ATTENTION_RETRY_MS);
    assert_eq!(next_due(), Some(now + CHANNEL_ATTENTION_RETRY_MS));
    let outbox = AttentionStore::open(f.root.path()).unwrap().unwrap();
    assert_eq!(outbox.next().unwrap().unwrap().message_id, younger);
    service.shutdown().unwrap();
}
