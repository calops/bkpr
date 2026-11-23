use super::{ChannelAuthority, ChannelEngine, MAX_SUBSCRIBERS, WorkspaceParticipant};
use anyhow::{Result, ensure};
use bkpr_protocol::channels::*;
use std::{
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

    fn authority(&self, actor: &Participant, workspace: &str) -> Result<ChannelAuthority> {
        ensure!(
            workspace == WORKSPACE || actor.kind == ParticipantKind::User,
            "workspace unavailable"
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
    assert_eq!(f.list(&f.a).len(), 1);
    assert!(f.list(&f.b).is_empty());
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
    assert!(f.as_actor(&f.b, subscribe(&id, Some(0), 1, 0)).is_err());
    assert_eq!(
        f.as_actor(&f.a, send(&id, request.clone())).unwrap(),
        accepted
    );
    request.idempotency_key = "stale-mention".into();
    assert!(f.as_actor(&f.a, send(&id, request)).is_err());
    assert_eq!(
        f.work(&message.id),
        [(f.b.id.0.clone(), Some("launch-b".into()), true, false)]
    );

    f.request(set(vec![f.b.id.clone()])).unwrap();
    assert!(f.as_actor(&f.a, send(&id, post("address-b"))).is_err());
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
    assert!(f.list(&f.user)[0].unread_attention);
    assert!(!f.list(&f.a)[0].unread_attention);
    f.as_actor(&f.a, mark(&id, attention.sequence)).unwrap();
    assert!(f.list(&f.user)[0].unread_attention);
    f.request(mark(&id, addressed.sequence)).unwrap();
    assert!(f.list(&f.user)[0].unread_attention);
    f.request(mark(&id, attention.sequence)).unwrap();
    assert!(!f.list(&f.user)[0].unread_attention);

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
    let initial = post("commit-fails");
    assert!(f.request(send(&id, initial.clone())).is_err());
    assert_eq!(f.list(&f.user)[0].last_sequence, 0);
    assert_eq!(
        f.request(messages(&id, None, 1)).unwrap(),
        ChannelResponse::History {
            messages: Vec::new(),
            next_before: None
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
            || false,
        )
    };
    assert_eq!(run(send(&id, initial.clone())).unwrap(), accepted);
    let ChannelResponse::Message {
        message: actual,
        text,
    } = run(ChannelRequest::Read {
        channel_id: id.clone(),
        message_id: message.id.clone(),
    })
    .unwrap()
    else {
        panic!("read reply")
    };
    assert_eq!((actual, text), (message, initial.text));
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
    let previous = posted(f.as_actor(&f.a, send(&id, post("recovered"))).unwrap());
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
            || false,
        )
    };
    assert!(run(&b, true, messages(&id, None, 1)).is_err());
    a.display_name = "New label".into();
    assert!(
        matches!(run(&a, false, messages(&id, None, 1)).unwrap(), ChannelResponse::History { messages, .. } if messages == [previous])
    );
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
            || false,
        )
    };
    let fetched = run(fetch(&id)).unwrap();
    assert_eq!(
        fetched,
        ChannelResponse::Attachment {
            attachment,
            bytes: sample.into()
        }
    );
    assert!(run(send(&id, post("closed"))).is_err());
    assert!(
        matches!(run(messages(&id, None, 1)).unwrap(), ChannelResponse::History { messages, .. } if messages == [message])
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
    } = f.request(messages(&id, None, 2)).unwrap()
    else {
        panic!("history reply")
    };
    assert_eq!(
        page.iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        [4, 5]
    );
    assert_eq!(next_before, Some(4));
    let ChannelResponse::History {
        messages: page,
        next_before,
    } = f.request(messages(&id, next_before, 2)).unwrap()
    else {
        panic!("history reply")
    };
    assert_eq!(
        page.iter()
            .map(|message| message.sequence)
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
        let (observed, ready) = crossbeam_channel::bounded(1);
        let (fixture, channel) = (&f, &id);
        let waiter = scope.spawn(move || {
            let signalled = AtomicBool::new(false);
            fixture.engine.request(
                subscribe(channel, Some(0), 1, MAX_WAIT_MS),
                |workspace| {
                    if !signalled.swap(true, Ordering::Relaxed) {
                        observed.send(()).unwrap();
                    }
                    fixture.authority(&fixture.a, workspace)
                },
                || false,
            )
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        f.request(send(&id, post("race"))).unwrap();
        let response = waiter.join().unwrap().unwrap();
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
            assert!(waiter.join().unwrap().is_err());
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
                || disconnected.load(Ordering::Acquire),
            )
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(
            waiter
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("cancelled")
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
    assert_eq!(f.list(&f.user)[0].last_sequence, 1);
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
