use super::{
    ChannelAccessLost, ChannelAuthority, ChannelEngine, MAX_SUBSCRIBERS, WorkspaceParticipant,
    store::{MAX_ARCHIVE_CANDIDATES, MAX_STAGED_BYTES, MAX_STAGING_RECLAIM},
};
use anyhow::{Result, ensure};
use bkpr_protocol::channels::*;
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
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
                },
                WorkspaceParticipant {
                    actor: self.a.clone(),
                    launch_id: Some("launch-a".into()),
                },
                WorkspaceParticipant {
                    actor: self.b.clone(),
                    launch_id: Some("launch-b".into()),
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

    fn work(&self, message: &MessageId) -> Vec<(String, Option<String>, bool, bool)> {
        self.engine.with_store(|store| {
            let mut query = store.connection.prepare(
                "SELECT participant_id,launch_id,addressed,pending FROM notification_work WHERE message_id=?1 ORDER BY participant_id",
            )?;
            Ok(query.query_map([message.as_ref()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?)
        }).unwrap()
    }
}

fn quiet() -> ChannelSettings {
    ChannelSettings {
        user_wake: WakePolicy::None,
        agent_wake: WakePolicy::None,
        ..ChannelSettings::default()
    }
}

fn post(key: &str) -> ChannelPost {
    ChannelPost {
        idempotency_key: key.into(),
        text: "a post quoting @Coordinator".into(),
        intent: PostIntent::Note,
        mentions: Vec::new(),
        reply_to: None,
        attachments: Vec::new(),
    }
}

fn send(channel: &ChannelId, post: ChannelPost) -> ChannelRequest {
    ChannelRequest::Post {
        channel_id: channel.clone(),
        post,
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

    let roster = |after| ChannelRequest::ListMembers {
        channel_id: id.clone(),
        after,
        limit: 2,
    };
    assert_eq!(
        f.request(roster(None)).unwrap(),
        ChannelResponse::Members {
            participants: vec![f.a.clone(), f.b.clone()],
            next_after: Some(f.b.id.clone()),
        }
    );
    assert_eq!(
        f.request(roster(Some(f.b.id.clone()))).unwrap(),
        ChannelResponse::Members {
            participants: vec![f.user.clone()],
            next_after: None,
        }
    );

    let mut directed = post("directed");
    directed.mentions.push(f.b.id.clone());
    directed.reply_to = Some(literal.id.clone());
    let addressed = posted(f.request(send(&id, directed)).unwrap());
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
    let broad = posted(f.request(send(&id, broad)).unwrap());
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
    assert_eq!(f.work(&attention.id), [(String::new(), None, false, true)]);
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
    let ChannelResponse::Message { message: actual } = run(ChannelRequest::Read {
        channel_id: id.clone(),
        message_id: message.id.clone(),
    })
    .unwrap() else {
        panic!("read reply")
    };
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
    f.sql(
        "DROP INDEX visible_explicit_channels;
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
    assert_eq!((sync, version), (3, 7));
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
    // Simulate a version-two store with the full staging budget plus one extra
    // unreferenced row. Only the two small real PNGs will be fetched or posted.
    f.sql(
        "DROP TABLE staged_attachments;
         DROP TABLE attachment_budget;
         PRAGMA user_version=2;",
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
