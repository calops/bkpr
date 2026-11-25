use super::*;
use crate::channels::*;
use planus::Offset;
use serde_json::json;

fn channel() -> ChannelId {
    ChannelId("channel".into())
}

fn participant() -> Participant {
    Participant {
        id: ParticipantId("stable-user".into()),
        kind: ParticipantKind::User,
        display_name: "User".into(),
    }
}

fn message_id() -> MessageId {
    MessageId("message".into())
}

fn source() -> Box<ChannelSource> {
    Box::new(ChannelSource {
        channel_id: ChannelId("source-channel".into()),
        message_id: MessageId("source-message".into()),
    })
}

fn attachment_id() -> AttachmentId {
    AttachmentId("attachment".into())
}

fn summary() -> ChannelSummary {
    ChannelSummary {
        id: channel(),
        workspace_id: "workspace".into(),
        name: "review".into(),
        membership: MembershipMode::WorkspaceWide,
        settings: ChannelSettings {
            user_wake: WakePolicy::AddressedOrAll,
            agent_wake: WakePolicy::AddressedOnly,
            attention_questions: true,
            attention_reports: false,
        },
        last_sequence: 9,
        read_through: 4,
        unread_attention: true,
    }
}

fn message() -> MessageSummary {
    MessageSummary {
        id: message_id(),
        sequence: 9,
        author: participant(),
        posted_at_ms: 77,
        intent: PostIntent::Question,
        mentions: vec![ParticipantId("stable-agent".into())],
        reply_to: Some(MessageId("previous".into())),
        attachments: vec![attachment_id()],
        source: None,
    }
}

fn descriptor() -> AttachmentDescriptor {
    AttachmentDescriptor {
        id: attachment_id(),
        media_type: AttachmentMediaType::Png,
        alt_text: Some("private picture".into()),
        byte_len: 3,
    }
}

fn renderable_message() -> ChannelMessage {
    ChannelMessage {
        message: message(),
        text: "private body\nwith Unicode \u{1f9ea}".into(),
        attachments: vec![descriptor()],
    }
}

fn cursor(sequence: u64) -> ChannelCursor {
    ChannelCursor {
        channel_id: channel(),
        sequence,
    }
}

fn post() -> ChannelPost {
    ChannelPost {
        idempotency_key: "same-request".into(),
        text: "private @literal\nbody".into(),
        intent: PostIntent::Report,
        mentions: vec![ParticipantId("stable-agent".into())],
        reply_to: Some(message_id()),
        attachments: vec![attachment_id()],
        source: None,
    }
}

#[test]
fn every_channel_request_round_trips_through_authenticated_native_wrapper() {
    let mut codec = Codec::new();
    let authority = ClientAuthority {
        client_id: "client".into(),
        token: "secret".into(),
    };
    for request in [
        ChannelRequest::List {
            workspace_id: "workspace".into(),
            after: Some(channel()),
            limit: 1,
        },
        ChannelRequest::ListArchivedWorkspaces {
            after: Some("workspace-previous".into()),
            limit: 2,
        },
        ChannelRequest::ListMembers {
            channel_id: channel(),
            after: Some(participant().id),
            limit: 64,
        },
        ChannelRequest::ListExplicitMembers {
            channel_id: channel(),
            after: Some(ParticipantId("inactive".into())),
            limit: 2,
        },
        ChannelRequest::Create {
            workspace_id: "workspace".into(),
            name: "review".into(),
            membership: MembershipMode::Explicit,
            members: vec![participant().id],
            settings: summary().settings,
        },
        ChannelRequest::SetSettings {
            channel_id: channel(),
            settings: ChannelSettings::default(),
        },
        ChannelRequest::SetMembership {
            channel_id: channel(),
            membership: MembershipMode::WorkspaceWide,
            members: vec![],
        },
        ChannelRequest::Post {
            channel_id: channel(),
            post: post(),
        },
        ChannelRequest::Post {
            channel_id: channel(),
            post: ChannelPost {
                source: Some(source()),
                ..post()
            },
        },
        ChannelRequest::History {
            channel_id: channel(),
            before: Some(8),
            after: None,
            limit: 16,
        },
        ChannelRequest::History {
            channel_id: channel(),
            before: None,
            after: Some(0),
            limit: 16,
        },
        ChannelRequest::Read {
            channel_id: channel(),
            message_id: message_id(),
        },
        ChannelRequest::MarkRead {
            channel_id: channel(),
            through: 0,
        },
        ChannelRequest::Subscribe {
            channel_id: channel(),
            after: None,
            limit: 5,
            wait_ms: 0,
        },
        ChannelRequest::Subscribe {
            channel_id: channel(),
            after: Some(cursor(0)),
            limit: 5,
            wait_ms: MAX_WAIT_MS,
        },
        ChannelRequest::UploadAttachment {
            channel_id: channel(),
            upload: AttachmentUpload {
                media_type: AttachmentMediaType::Png,
                alt_text: Some("private picture".into()),
                // The receiving service, not the wire, decodes and validates the raster.
                bytes: vec![0, 255, 6],
            },
        },
        ChannelRequest::GetAttachment {
            channel_id: channel(),
            attachment_id: attachment_id(),
        },
    ] {
        let operation = Operation::Channels {
            request: request.clone(),
        };
        let decoded =
            decode_request(codec.encode_client_request(&authority, &operation).unwrap()).unwrap();
        let Operation::Client {
            authority: actual,
            request: wrapped,
        } = decoded.operation
        else {
            panic!("expected client wrapper");
        };
        assert_eq!(authority.client_id, actual.client_id);
        assert_eq!(authority.token, actual.token);
        let Operation::Channels { request: actual } = *wrapped else {
            panic!("expected channel operation");
        };
        assert_eq!(actual, request);
        let dbg = format!("{operation:?}");
        assert!(!dbg.contains("private"));
        assert!(!dbg.contains("same-request"));
    }
}

#[test]
fn every_channel_response_round_trips_and_owned_fetch_shares_wire_storage() {
    let attachment = descriptor();
    let mut linked_message = renderable_message();
    linked_message.message.source = Some(source());
    let mut codec = Codec::new();
    for response in [
        ChannelResponse::Listed {
            channels: vec![summary()],
            next_after: Some(channel()),
        },
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids: vec!["workspace-archive".into()],
            next_after: Some("workspace-archive".into()),
        },
        ChannelResponse::Members {
            participants: vec![participant()],
            next_after: None,
        },
        ChannelResponse::ExplicitMembers {
            members: vec![ParticipantId("inactive".into())],
            next_after: Some(ParticipantId("inactive".into())),
        },
        ChannelResponse::Channel { channel: summary() },
        ChannelResponse::Posted {
            message: message(),
            wake_count: 2,
            attention: true,
        },
        ChannelResponse::History {
            messages: vec![linked_message],
            next_before: Some(9),
            next_after: Some(9),
            cursor: cursor(11),
        },
        ChannelResponse::Message {
            message: renderable_message(),
        },
        ChannelResponse::ReadPosition { through: 4 },
        ChannelResponse::Changes {
            cursor: cursor(11),
            changes: vec![ChannelChange {
                sequence: 10,
                kind: ChannelChangeKind::Post,
            }],
            more: true,
            status: ChannelSubscriptionStatus::Active,
        },
        ChannelResponse::Changes {
            cursor: cursor(11),
            changes: Vec::new(),
            more: false,
            status: ChannelSubscriptionStatus::Closed,
        },
        ChannelResponse::AttachmentUploaded {
            attachment: attachment.clone(),
        },
        ChannelResponse::Attachment {
            attachment,
            bytes: bytes::Bytes::from_static(&[0, 255, 6]),
        },
    ] {
        assert!(!format!("{response:?}").contains("private"));
        let response = reply(response);
        let wire = codec.encode_response(&response).unwrap().to_vec();
        let wire_start = wire.as_ptr() as usize;
        let wire_end = wire_start + wire.len();
        let decoded = decode_response_owned(wire).unwrap();
        let Some(Payload::Channels(expected)) = response.result else {
            unreachable!()
        };
        let Some(Payload::Channels(actual)) = decoded.result else {
            panic!("expected channel response")
        };
        assert_eq!(actual, expected);
        if let ChannelResponse::Attachment { bytes, .. } = actual {
            let start = bytes.as_ptr() as usize;
            assert!(wire_start <= start && start + bytes.len() <= wire_end);
        }
    }
}

fn reply(response: ChannelResponse) -> Response {
    Response {
        version: PROTOCOL_VERSION,
        ok: true,
        result: Some(Payload::Channels(response)),
        error: None,
        failure: None,
    }
}

#[test]
fn channel_validation_rejects_client_authorship_and_invalid_reference_shapes() {
    assert!(
        serde_json::from_value::<ChannelRequest>(json!({
            "method": "mark_read", "channel_id": "channel", "through": 1, "actor": "user"
        }))
        .is_err()
    );
    let decoded: ChannelRequest = serde_json::from_value(json!({
        "method": "post",
        "channel_id": "channel",
        "post": {"idempotency_key": "note", "text": "hello @literal"}
    }))
    .unwrap();
    let ChannelRequest::Post { post: note, .. } = decoded else {
        unreachable!()
    };
    assert_eq!(note.intent, PostIntent::Note);
    assert!(note.mentions.is_empty());
    assert!(note.attachments.is_empty());
    assert!(note.source.is_none());
    assert!(serde_json::to_value(note).unwrap().get("source").is_none());

    let mut duplicate = post();
    duplicate.mentions.push(duplicate.mentions[0].clone());
    let mut escapes = post();
    escapes.text = "text\u{1b}[0m".into();
    let mut codec = Codec::new();
    for request in [
        ChannelRequest::SetMembership {
            channel_id: channel(),
            membership: MembershipMode::WorkspaceWide,
            members: vec![participant().id],
        },
        ChannelRequest::Post {
            channel_id: channel(),
            post: duplicate,
        },
        ChannelRequest::Post {
            channel_id: channel(),
            post: escapes,
        },
        ChannelRequest::Subscribe {
            channel_id: channel(),
            after: Some(ChannelCursor {
                channel_id: ChannelId("other".into()),
                sequence: 0,
            }),
            limit: 1,
            wait_ms: 0,
        },
        ChannelRequest::Subscribe {
            channel_id: channel(),
            after: None,
            limit: 1,
            wait_ms: MAX_WAIT_MS + 1,
        },
        ChannelRequest::History {
            channel_id: channel(),
            before: None,
            after: None,
            limit: MAX_PAGE_LIMIT + 1,
        },
        ChannelRequest::History {
            channel_id: channel(),
            before: Some(9),
            after: Some(0),
            limit: 1,
        },
        ChannelRequest::History {
            channel_id: channel(),
            before: Some(0),
            after: None,
            limit: 1,
        },
        ChannelRequest::ListArchivedWorkspaces {
            after: Some(String::new()),
            limit: 1,
        },
        ChannelRequest::ListExplicitMembers {
            channel_id: channel(),
            after: None,
            limit: 0,
        },
        ChannelRequest::UploadAttachment {
            channel_id: channel(),
            upload: AttachmentUpload {
                media_type: AttachmentMediaType::Png,
                alt_text: None,
                bytes: vec![],
            },
        },
    ] {
        assert!(
            codec
                .encode_request(&Request::new(Operation::Channels { request }))
                .is_err()
        );
    }
}

#[test]
fn channel_source_requires_only_two_exact_bounded_ids() {
    assert!(
        serde_json::from_value::<ChannelSource>(json!({
            "channel_id": "channel", "message_id": "message", "text": "extra"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<ChannelSource>(json!({"channel_id": "channel"})).is_err());

    let too_long = "é".repeat(MAX_ID_BYTES / 2 + 1);
    let mut codec = Codec::new();
    for (channel_id, message_id) in [
        ("#desk", "message"),
        ("#team", "message"),
        ("", "message"),
        (too_long.as_str(), "message"),
        ("channel", ""),
        ("channel", too_long.as_str()),
        ("channel", "message\n"),
    ] {
        let reference = Box::new(ChannelSource {
            channel_id: ChannelId(channel_id.into()),
            message_id: MessageId(message_id.into()),
        });
        let request = ChannelRequest::Post {
            channel_id: channel(),
            post: ChannelPost {
                source: Some(reference.clone()),
                ..post()
            },
        };
        assert!(request.validate().is_err());
        assert!(
            codec
                .encode_response(&reply(ChannelResponse::Posted {
                    message: MessageSummary {
                        source: Some(reference),
                        ..message()
                    },
                    wake_count: 0,
                    attention: false,
                }))
                .is_err()
        );

        let mut b = Builder::new();
        let source_channel = b.create_string(channel_id);
        let source_message = b.create_string(message_id);
        let source = g::ChannelSource::create(&mut b, source_channel, source_message);
        let channel = b.create_string("channel");
        let empty_ids = b.create_vector([] as [Offset<str>; 0]);
        let post = g::ChannelPost::create(
            &mut b,
            channel,
            channel,
            g::PostIntent::Note,
            empty_ids,
            None::<Offset<str>>,
            empty_ids,
            Some(source),
        );
        let post = g::ChannelRequestPost::create(&mut b, channel, post);
        let request = g::ChannelRequest::create_channel_request_post(&mut b, post);
        let channels = g::OperationChannels::create(&mut b, request);
        let operation = g::Operation::create_operation_channels(&mut b, channels);
        let root = g::Request::create(&mut b, PROTOCOL_VERSION, operation);
        assert!(decode_request(b.finish(root, None)).is_err());
    }
}

#[test]
fn malformed_native_channel_vectors_fail_before_expansion() {
    let mut b = Builder::new();
    let channel = b.create_string("channel");
    let aliased_member = b.create_string("participant");
    let members = b.create_vector([aliased_member; MAX_MEMBERS + 1]);
    let membership = g::ChannelRequestSetMembership::create(
        &mut b,
        channel,
        g::MembershipMode::Explicit,
        members,
    );
    let request = g::ChannelRequest::create_channel_request_set_membership(&mut b, membership);
    let channels = g::OperationChannels::create(&mut b, request);
    let operation = g::Operation::create_operation_channels(&mut b, channels);
    assert_eq!(operation.tag(), 36);
    let root = g::Request::create(&mut b, PROTOCOL_VERSION, operation);
    let encoded = b.finish(root, None);
    let mut charged = 0;
    let error = decode_request_charged(encoded, &mut |bytes| {
        charged += bytes;
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("channel vector exceeds limit"));
    assert!(
        charged < 512,
        "charged {charged} before rejecting the aliased vector"
    );
}

#[test]
fn native_fetch_rejects_descriptor_size_mismatch() {
    let mut b = Builder::new();
    let id = b.create_string("attachment");
    let attachment = g::AttachmentDescriptor::create(
        &mut b,
        id,
        g::AttachmentMediaType::Png,
        None::<Offset<str>>,
        2,
    );
    let bytes = b.create_vector([0u8, 255, 6]);
    let response = g::ChannelResponseAttachment::create(&mut b, attachment, bytes);
    let response = g::ChannelResponse::create_channel_response_attachment(&mut b, response);
    let error = decode_native_channel_response(&mut b, response).unwrap_err();
    assert!(error.contains("attachment size mismatch"));
}

fn decode_native_channel_response(
    b: &mut Builder,
    response: planus::UnionOffset<g::ChannelResponse>,
) -> Result<Response, String> {
    let payload = g::ChannelPayload::create(b, response);
    let payload = g::Payload::create_channel_payload(b, payload);
    assert_eq!(payload.tag(), 4);
    let payload = g::OptionalPayload::create(b, payload);
    let root = g::Response::create(
        b,
        PROTOCOL_VERSION,
        true,
        Some(payload),
        None::<Offset<str>>,
        None::<Offset<g::OptionalFailure>>,
    );
    decode_response(b.finish(root, None)).map_err(|error| error.to_string())
}

#[test]
fn native_history_budgets_source_ids_even_for_aliased_rows() {
    let count = MAX_HISTORY_BYTES / MAX_POST_BYTES;
    let mut message = ChannelMessage {
        message: MessageSummary {
            id: MessageId("test".into()),
            author: Participant {
                id: ParticipantId("test".into()),
                display_name: "test".into(),
                ..participant()
            },
            mentions: vec![],
            reply_to: None,
            attachments: vec![],
            ..message()
        },
        text: String::new(),
        attachments: vec![],
    };
    message.text = "x".repeat(MAX_HISTORY_BYTES / count - message.content_bytes());
    let history = |message: &ChannelMessage| {
        reply(ChannelResponse::History {
            messages: (1..=count)
                .map(|sequence| {
                    let mut message = message.clone();
                    message.message.sequence = sequence as u64;
                    message
                })
                .collect(),
            next_before: None,
            next_after: None,
            cursor: cursor(count as u64),
        })
    };
    let mut codec = Codec::new();
    codec.encode_response(&history(&message)).unwrap();
    message.message.source = Some(source());
    let response = history(&message);
    let error = codec.encode_response(&response).unwrap_err().to_string();
    assert!(error.contains("channel history page exceeds byte limit"));

    let mut b = Builder::new();
    let id = b.create_string("test");
    let author = g::ChannelParticipant::create(&mut b, id, g::ParticipantKind::User, id);
    let empty_ids = b.create_vector([] as [Offset<str>; 0]);
    let linked = message.message.source.as_ref().unwrap();
    let source_channel = b.create_string(linked.channel_id.as_ref());
    let source_message = b.create_string(linked.message_id.as_ref());
    let source = g::ChannelSource::create(&mut b, source_channel, source_message);
    let metadata = g::ChannelMessageSummary::create(
        &mut b,
        id,
        1u64,
        author,
        0u64,
        g::PostIntent::Note,
        empty_ids,
        None::<Offset<str>>,
        empty_ids,
        Some(source),
    );
    let text = b.create_string(&message.text);
    let attachments = b.create_vector([] as [Offset<g::AttachmentDescriptor>; 0]);
    let alias = g::ChannelMessage::create(&mut b, metadata, text, attachments);
    let messages = b.create_vector(vec![alias; count].as_slice());
    let cursor = g::ChannelCursor::create(&mut b, id, count as u64);
    let response = g::ChannelResponseHistory::create(
        &mut b,
        messages,
        None::<Offset<g::OptionalU64>>,
        None::<Offset<g::OptionalU64>>,
        cursor,
    );
    let response = g::ChannelResponse::create_channel_response_history(&mut b, response);
    let error = decode_native_channel_response(&mut b, response).unwrap_err();
    assert!(error.contains("channel history page exceeds byte limit"));
}

#[test]
fn native_subscription_requires_explicit_consistent_status() {
    use g::ChannelSubscriptionStatus::{Closed, Unspecified};

    for (status, more, expected_error) in [
        (Unspecified, false, "missing channel subscription status"),
        (
            Closed,
            true,
            "a closed subscription cannot have more changes",
        ),
    ] {
        let mut b = Builder::new();
        let channel = b.create_string("channel");
        let cursor = g::ChannelCursor::create(&mut b, channel, 0u64);
        let changes = b.create_vector([] as [Offset<g::ChannelChange>; 0]);
        let response = g::ChannelResponseChanges::create(&mut b, cursor, changes, more, status);
        let response = g::ChannelResponse::create_channel_response_changes(&mut b, response);
        let error = decode_native_channel_response(&mut b, response).unwrap_err();
        assert!(error.contains(expected_error));
    }
}

#[test]
fn attachment_json_uses_canonical_base64_in_both_directions() {
    let bytes = vec![0, 255, 128, 1];
    let request = ChannelRequest::UploadAttachment {
        channel_id: channel(),
        upload: AttachmentUpload {
            media_type: AttachmentMediaType::Png,
            alt_text: None,
            bytes: bytes.clone(),
        },
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["upload"]["bytes"], "AP+AAQ==");
    assert_eq!(
        serde_json::from_value::<ChannelRequest>(json).unwrap(),
        request
    );

    let response = ChannelResponse::Attachment {
        attachment: AttachmentDescriptor {
            byte_len: 4,
            ..descriptor()
        },
        bytes: bytes::Bytes::from(bytes),
    };
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["bytes"], "AP+AAQ==");
    assert_eq!(
        serde_json::from_value::<ChannelResponse>(json).unwrap(),
        response
    );
}

#[test]
fn attachment_json_accepts_size_limit_and_rejects_malformed_or_oversized_input() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let bytes = vec![0xff; MAX_ATTACHMENT_BYTES];
    let upload = AttachmentUpload {
        media_type: AttachmentMediaType::Png,
        alt_text: None,
        bytes,
    };
    let json = serde_json::to_string(&upload).unwrap();
    assert_eq!(
        serde_json::from_str::<AttachmentUpload>(&json).unwrap(),
        upload
    );

    // The first oversized resource occupies the same number of encoded characters
    // as the largest legal one, so checking the Base64 string length alone is insufficient.
    let oversized = STANDARD.encode(vec![0xff; MAX_ATTACHMENT_BYTES + 1]);
    assert_eq!(oversized.len(), MAX_ATTACHMENT_BYTES.div_ceil(3) * 4);
    for invalid in [
        json!(oversized),
        json!([0, 255]),
        json!(""),
        json!("AP8"),
        json!("!!!!"),
    ] {
        assert!(
            serde_json::from_value::<AttachmentUpload>(json!({
                "media_type": "image/png", "bytes": &invalid
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChannelResponse>(json!({
                "kind": "attachment", "attachment": descriptor(), "bytes": invalid
            }))
            .is_err()
        );
    }
}
