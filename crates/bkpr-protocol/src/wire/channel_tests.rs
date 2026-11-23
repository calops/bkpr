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
        ChannelRequest::ListMembers {
            channel_id: channel(),
            after: Some(participant().id),
            limit: 64,
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
        ChannelRequest::History {
            channel_id: channel(),
            before: Some(8),
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
    let attachment = AttachmentDescriptor {
        id: attachment_id(),
        media_type: AttachmentMediaType::Png,
        alt_text: Some("private picture".into()),
        byte_len: 3,
    };
    let mut codec = Codec::new();
    for response in [
        ChannelResponse::Listed {
            channels: vec![summary()],
            next_after: Some(channel()),
        },
        ChannelResponse::Members {
            participants: vec![participant()],
            next_after: None,
        },
        ChannelResponse::Channel { channel: summary() },
        ChannelResponse::Posted {
            message: message(),
            wake_count: 2,
            attention: true,
        },
        ChannelResponse::History {
            messages: vec![message()],
            next_before: Some(9),
        },
        ChannelResponse::Message {
            message: message(),
            text: "private body".into(),
        },
        ChannelResponse::ReadPosition { through: 4 },
        ChannelResponse::Changes {
            cursor: cursor(11),
            changes: vec![ChannelChange {
                sequence: 10,
                kind: ChannelChangeKind::Post,
            }],
            more: true,
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
            limit: MAX_PAGE_LIMIT + 1,
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
    let payload = g::ChannelPayload::create(&mut b, response);
    let payload = g::Payload::create_channel_payload(&mut b, payload);
    assert_eq!(payload.tag(), 4);
    let payload = g::OptionalPayload::create(&mut b, payload);
    let root = g::Response::create(
        &mut b,
        PROTOCOL_VERSION,
        true,
        Some(payload),
        None::<Offset<str>>,
        None::<Offset<g::OptionalFailure>>,
    );
    let error = decode_response(b.finish(root, None)).unwrap_err();
    assert!(error.to_string().contains("attachment size mismatch"));
}
