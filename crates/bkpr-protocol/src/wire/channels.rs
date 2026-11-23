use super::types::{read_text, write_optional_text, write_text};
use super::{Budget, generated::native_wire as g, retain_binary};
use crate::channels::*;
use anyhow::{Result, ensure};
use planus::{Builder, Offset, UnionOffset, Vector};

macro_rules! wire_enum {
    ($name:ident { $($case:ident),+ $(,)? }) => {
        impl From<$name> for g::$name {
            fn from(value: $name) -> Self {
                match value { $($name::$case => Self::$case),+ }
            }
        }
        impl From<g::$name> for $name {
            fn from(value: g::$name) -> Self {
                match value { $(g::$name::$case => Self::$case),+ }
            }
        }
    };
}

wire_enum!(MembershipMode {
    Explicit,
    WorkspaceWide
});
wire_enum!(WakePolicy {
    All,
    AddressedOrAll,
    AddressedOnly,
    None
});
wire_enum!(ParticipantKind { User, Agent });
wire_enum!(PostIntent {
    Note,
    Question,
    Report
});
wire_enum!(AttachmentMediaType { Png });
wire_enum!(ChannelChangeKind {
    Post,
    Settings,
    Membership,
    ReadPosition
});

pub(super) fn write_request(
    b: &mut Builder,
    request: &ChannelRequest,
    budget: &mut Budget,
    depth: usize,
) -> Result<UnionOffset<g::ChannelRequest>> {
    use g::ChannelRequest as G;
    request.validate()?;
    budget.node(depth)?;
    let depth = depth + 1;
    Ok(match request {
        ChannelRequest::List {
            workspace_id,
            after,
            limit,
        } => {
            let workspace = write_text(b, workspace_id, budget)?;
            let after = write_optional_id(b, after.as_ref(), budget)?;
            let offset = g::ChannelRequestList::create(b, workspace, after, *limit);
            G::create_channel_request_list(b, offset)
        }
        ChannelRequest::ListMembers {
            channel_id,
            after,
            limit,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let after = write_optional_id(b, after.as_ref(), budget)?;
            let offset = g::ChannelRequestListMembers::create(b, channel, after, *limit);
            G::create_channel_request_list_members(b, offset)
        }
        ChannelRequest::Create {
            workspace_id,
            name,
            membership,
            members,
            settings,
        } => {
            let workspace = write_text(b, workspace_id, budget)?;
            let name = write_text(b, name, budget)?;
            let members = write_ids(b, members, budget)?;
            let settings = write_settings(b, *settings, budget, depth)?;
            let offset = g::ChannelRequestCreate::create(
                b,
                workspace,
                name,
                g::MembershipMode::from(*membership),
                members,
                settings,
            );
            G::create_channel_request_create(b, offset)
        }
        ChannelRequest::SetSettings {
            channel_id,
            settings,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let settings = write_settings(b, *settings, budget, depth)?;
            let offset = g::ChannelRequestSetSettings::create(b, channel, settings);
            G::create_channel_request_set_settings(b, offset)
        }
        ChannelRequest::SetMembership {
            channel_id,
            membership,
            members,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let members = write_ids(b, members, budget)?;
            let offset = g::ChannelRequestSetMembership::create(
                b,
                channel,
                g::MembershipMode::from(*membership),
                members,
            );
            G::create_channel_request_set_membership(b, offset)
        }
        ChannelRequest::Post { channel_id, post } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let post = write_post(b, post, budget, depth)?;
            let offset = g::ChannelRequestPost::create(b, channel, post);
            G::create_channel_request_post(b, offset)
        }
        ChannelRequest::History {
            channel_id,
            before,
            limit,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let before = write_optional_sequence(b, *before, budget, depth)?;
            let offset = g::ChannelRequestHistory::create(b, channel, before, *limit);
            G::create_channel_request_history(b, offset)
        }
        ChannelRequest::Read {
            channel_id,
            message_id,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let message = write_text(b, message_id.as_ref(), budget)?;
            let offset = g::ChannelRequestRead::create(b, channel, message);
            G::create_channel_request_read(b, offset)
        }
        ChannelRequest::MarkRead {
            channel_id,
            through,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let offset = g::ChannelRequestMarkRead::create(b, channel, *through);
            G::create_channel_request_mark_read(b, offset)
        }
        ChannelRequest::Subscribe {
            channel_id,
            after,
            limit,
            wait_ms,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let after = after
                .as_ref()
                .map(|value| write_cursor(b, value, budget, depth))
                .transpose()?;
            let offset = g::ChannelRequestSubscribe::create(b, channel, after, *limit, *wait_ms);
            G::create_channel_request_subscribe(b, offset)
        }
        ChannelRequest::UploadAttachment { channel_id, upload } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let alt = write_optional_text(b, upload.alt_text.as_deref(), budget)?;
            budget.bytes(upload.bytes.len())?;
            let bytes = b.create_vector(upload.bytes.as_slice());
            let offset = g::ChannelRequestUploadAttachment::create(
                b,
                channel,
                g::AttachmentMediaType::from(upload.media_type),
                alt,
                bytes,
            );
            G::create_channel_request_upload_attachment(b, offset)
        }
        ChannelRequest::GetAttachment {
            channel_id,
            attachment_id,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let attachment = write_text(b, attachment_id.as_ref(), budget)?;
            let offset = g::ChannelRequestGetAttachment::create(b, channel, attachment);
            G::create_channel_request_get_attachment(b, offset)
        }
    })
}

pub(super) fn read_request(
    value: g::ChannelRequestRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelRequest> {
    use g::ChannelRequestRef as R;
    budget.node(depth)?;
    let depth = depth + 1;
    let request = match value {
        R::ChannelRequestList(v) => ChannelRequest::List {
            workspace_id: read_id(v.workspace_id()?, budget)?,
            after: read_optional_id(v.after()?, ChannelId, budget)?,
            limit: v.limit()?,
        },
        R::ChannelRequestListMembers(v) => ChannelRequest::ListMembers {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            after: read_optional_id(v.after()?, ParticipantId, budget)?,
            limit: v.limit()?,
        },
        R::ChannelRequestCreate(v) => ChannelRequest::Create {
            workspace_id: read_id(v.workspace_id()?, budget)?,
            name: read_name(v.name()?, budget)?,
            membership: v.membership()?.into(),
            members: read_ids(v.members()?, MAX_MEMBERS, ParticipantId, budget)?,
            settings: read_settings(v.settings()?, budget, depth)?,
        },
        R::ChannelRequestSetSettings(v) => ChannelRequest::SetSettings {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            settings: read_settings(v.settings()?, budget, depth)?,
        },
        R::ChannelRequestSetMembership(v) => ChannelRequest::SetMembership {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            membership: v.membership()?.into(),
            members: read_ids(v.members()?, MAX_MEMBERS, ParticipantId, budget)?,
        },
        R::ChannelRequestPost(v) => ChannelRequest::Post {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            post: read_post(v.post()?, budget, depth)?,
        },
        R::ChannelRequestHistory(v) => ChannelRequest::History {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            before: read_optional_sequence(v.before()?, budget, depth)?,
            limit: v.limit()?,
        },
        R::ChannelRequestRead(v) => ChannelRequest::Read {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            message_id: MessageId(read_id(v.message_id()?, budget)?),
        },
        R::ChannelRequestMarkRead(v) => ChannelRequest::MarkRead {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            through: v.through()?,
        },
        R::ChannelRequestSubscribe(v) => ChannelRequest::Subscribe {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            after: v
                .after()?
                .map(|value| read_cursor(value, budget, depth))
                .transpose()?,
            limit: v.limit()?,
            wait_ms: v.wait_ms()?,
        },
        R::ChannelRequestUploadAttachment(v) => {
            let channel_id = ChannelId(read_id(v.channel_id()?, budget)?);
            let media_type = v.media_type()?.into();
            let alt_text = read_alt(v.alt_text()?, budget)?;
            let bytes = v.bytes()?;
            valid_attachment_size(bytes.len())?;
            budget.bytes(bytes.len())?;
            ChannelRequest::UploadAttachment {
                channel_id,
                upload: AttachmentUpload {
                    media_type,
                    alt_text,
                    bytes: bytes.to_vec(),
                },
            }
        }
        R::ChannelRequestGetAttachment(v) => ChannelRequest::GetAttachment {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            attachment_id: AttachmentId(read_id(v.attachment_id()?, budget)?),
        },
    };
    request.validate()?;
    Ok(request)
}

pub(super) fn write_response(
    b: &mut Builder,
    response: &ChannelResponse,
    budget: &mut Budget,
) -> Result<UnionOffset<g::ChannelResponse>> {
    use g::ChannelResponse as G;
    budget.node(0)?;
    Ok(match response {
        ChannelResponse::Listed {
            channels,
            next_after,
        } => {
            let channels = write_page(b, channels, budget, write_summary)?;
            let next_after = write_reply_id(b, next_after.as_ref(), budget)?;
            let offset = g::ChannelResponseListed::create(b, channels, next_after);
            G::create_channel_response_listed(b, offset)
        }
        ChannelResponse::Members {
            participants,
            next_after,
        } => {
            let participants = write_page(b, participants, budget, write_participant)?;
            let next_after = write_reply_id(b, next_after.as_ref(), budget)?;
            let offset = g::ChannelResponseMembers::create(b, participants, next_after);
            G::create_channel_response_members(b, offset)
        }
        ChannelResponse::Channel { channel } => {
            let channel = write_summary(b, channel, budget, 1)?;
            let offset = g::ChannelResponseChannel::create(b, channel);
            G::create_channel_response_channel(b, offset)
        }
        ChannelResponse::Posted {
            message,
            wake_count,
            attention,
        } => {
            let message = write_message(b, message, budget, 1)?;
            let offset = g::ChannelResponsePosted::create(b, message, *wake_count, *attention);
            G::create_channel_response_posted(b, offset)
        }
        ChannelResponse::History {
            messages,
            next_before,
        } => {
            let messages = write_page(b, messages, budget, write_message)?;
            let next_before = write_optional_sequence(b, *next_before, budget, 1)?;
            let offset = g::ChannelResponseHistory::create(b, messages, next_before);
            G::create_channel_response_history(b, offset)
        }
        ChannelResponse::Message { message, text } => {
            let message = write_message(b, message, budget, 1)?;
            valid_post_text(text)?;
            let text = write_text(b, text, budget)?;
            let offset = g::ChannelResponseMessage::create(b, message, text);
            G::create_channel_response_message(b, offset)
        }
        ChannelResponse::ReadPosition { through } => {
            let offset = g::ChannelResponseReadPosition::create(b, *through);
            G::create_channel_response_read_position(b, offset)
        }
        ChannelResponse::Changes {
            cursor,
            changes,
            more,
        } => {
            let cursor = write_cursor(b, cursor, budget, 1)?;
            let changes = write_page(b, changes, budget, write_change)?;
            let offset = g::ChannelResponseChanges::create(b, cursor, changes, *more);
            G::create_channel_response_changes(b, offset)
        }
        ChannelResponse::AttachmentUploaded { attachment } => {
            let attachment = write_descriptor(b, attachment, budget, 1)?;
            let offset = g::ChannelResponseAttachmentUploaded::create(b, attachment);
            G::create_channel_response_attachment_uploaded(b, offset)
        }
        ChannelResponse::Attachment { attachment, bytes } => {
            ensure!(
                attachment.byte_len as usize == bytes.len(),
                "attachment size mismatch"
            );
            let attachment = write_descriptor(b, attachment, budget, 1)?;
            budget.bytes(bytes.len())?;
            let bytes = b.create_vector(bytes.as_ref());
            let offset = g::ChannelResponseAttachment::create(b, attachment, bytes);
            G::create_channel_response_attachment(b, offset)
        }
    })
}

pub(super) fn read_response(
    value: g::ChannelResponseRef<'_>,
    budget: &mut Budget,
    owner: Option<&bytes::Bytes>,
) -> Result<ChannelResponse> {
    use g::ChannelResponseRef as R;
    budget.node(0)?;
    Ok(match value {
        R::ChannelResponseListed(v) => ChannelResponse::Listed {
            channels: read_vector(v.channels()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_summary(v, b, 1)
            })?,
            next_after: read_optional_id(v.next_after()?, ChannelId, budget)?,
        },
        R::ChannelResponseMembers(v) => ChannelResponse::Members {
            participants: read_vector(v.participants()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_participant(v, b, 1)
            })?,
            next_after: read_optional_id(v.next_after()?, ParticipantId, budget)?,
        },
        R::ChannelResponseChannel(v) => ChannelResponse::Channel {
            channel: read_summary(v.channel()?, budget, 1)?,
        },
        R::ChannelResponsePosted(v) => ChannelResponse::Posted {
            message: read_message(v.message()?, budget, 1)?,
            wake_count: v.wake_count()?,
            attention: v.attention()?,
        },
        R::ChannelResponseHistory(v) => ChannelResponse::History {
            messages: read_vector(v.messages()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_message(v, b, 1)
            })?,
            next_before: read_optional_sequence(v.next_before()?, budget, 1)?,
        },
        R::ChannelResponseMessage(v) => ChannelResponse::Message {
            message: read_message(v.message()?, budget, 1)?,
            text: read_body(v.text()?, budget)?,
        },
        R::ChannelResponseReadPosition(v) => ChannelResponse::ReadPosition {
            through: v.through()?,
        },
        R::ChannelResponseChanges(v) => ChannelResponse::Changes {
            cursor: read_cursor(v.cursor()?, budget, 1)?,
            changes: read_vector(v.changes()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_change(v, b, 1)
            })?,
            more: v.more()?,
        },
        R::ChannelResponseAttachmentUploaded(v) => ChannelResponse::AttachmentUploaded {
            attachment: read_descriptor(v.attachment()?, budget, 1)?,
        },
        R::ChannelResponseAttachment(v) => {
            let attachment = read_descriptor(v.attachment()?, budget, 1)?;
            let bytes = v.bytes()?;
            ensure!(
                attachment.byte_len as usize == bytes.len(),
                "attachment size mismatch"
            );
            ChannelResponse::Attachment {
                attachment,
                bytes: retain_binary(bytes, owner, budget)?,
            }
        }
    })
}

fn write_post(
    b: &mut Builder,
    value: &ChannelPost,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelPost>> {
    budget.node(depth)?;
    let key = write_text(b, &value.idempotency_key, budget)?;
    let text = write_text(b, &value.text, budget)?;
    let mentions = write_ids(b, &value.mentions, budget)?;
    let reply_to = write_optional_id(b, value.reply_to.as_ref(), budget)?;
    let attachments = write_ids(b, &value.attachments, budget)?;
    Ok(g::ChannelPost::create(
        b,
        key,
        text,
        g::PostIntent::from(value.intent),
        mentions,
        reply_to,
        attachments,
    ))
}

fn read_post(v: g::ChannelPostRef<'_>, budget: &mut Budget, depth: usize) -> Result<ChannelPost> {
    budget.node(depth)?;
    Ok(ChannelPost {
        idempotency_key: read_id(v.idempotency_key()?, budget)?,
        text: read_body(v.text()?, budget)?,
        intent: v.intent()?.into(),
        mentions: read_ids(v.mentions()?, MAX_MENTIONS, ParticipantId, budget)?,
        reply_to: read_optional_id(v.reply_to()?, MessageId, budget)?,
        attachments: read_ids(v.attachments()?, MAX_ATTACHMENTS, AttachmentId, budget)?,
    })
}

fn write_summary(
    b: &mut Builder,
    value: &ChannelSummary,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelSummary>> {
    budget.node(depth)?;
    valid_id(value.id.as_ref())?;
    valid_id(&value.workspace_id)?;
    valid_name(&value.name)?;
    ensure!(
        value.read_through <= value.last_sequence,
        "invalid channel read position"
    );
    let id = write_text(b, value.id.as_ref(), budget)?;
    let workspace = write_text(b, &value.workspace_id, budget)?;
    let name = write_text(b, &value.name, budget)?;
    let settings = write_settings(b, value.settings, budget, depth + 1)?;
    Ok(g::ChannelSummary::create(
        b,
        id,
        workspace,
        name,
        g::MembershipMode::from(value.membership),
        settings,
        value.last_sequence,
        value.read_through,
        value.unread_attention,
    ))
}

fn read_summary(
    v: g::ChannelSummaryRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelSummary> {
    budget.node(depth)?;
    let last_sequence = v.last_sequence()?;
    let read_through = v.read_through()?;
    ensure!(
        read_through <= last_sequence,
        "invalid channel read position"
    );
    Ok(ChannelSummary {
        id: ChannelId(read_id(v.id()?, budget)?),
        workspace_id: read_id(v.workspace_id()?, budget)?,
        name: read_name(v.name()?, budget)?,
        membership: v.membership()?.into(),
        settings: read_settings(v.settings()?, budget, depth + 1)?,
        last_sequence,
        read_through,
        unread_attention: v.unread_attention()?,
    })
}

fn write_message(
    b: &mut Builder,
    value: &MessageSummary,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelMessageSummary>> {
    budget.node(depth)?;
    valid_id(value.id.as_ref())?;
    ensure!(value.sequence > 0, "zero channel post sequence");
    valid_references(&value.mentions, value.reply_to.as_ref(), &value.attachments)?;
    let id = write_text(b, value.id.as_ref(), budget)?;
    let author = write_participant(b, &value.author, budget, depth + 1)?;
    let mentions = write_ids(b, &value.mentions, budget)?;
    let reply = write_optional_id(b, value.reply_to.as_ref(), budget)?;
    let attachments = write_ids(b, &value.attachments, budget)?;
    Ok(g::ChannelMessageSummary::create(
        b,
        id,
        value.sequence,
        author,
        value.posted_at_ms,
        g::PostIntent::from(value.intent),
        mentions,
        reply,
        attachments,
    ))
}

fn read_message(
    v: g::ChannelMessageSummaryRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<MessageSummary> {
    budget.node(depth)?;
    let sequence = v.sequence()?;
    ensure!(sequence > 0, "zero channel post sequence");
    let mentions = read_ids(v.mentions()?, MAX_MENTIONS, ParticipantId, budget)?;
    let reply_to = read_optional_id(v.reply_to()?, MessageId, budget)?;
    let attachments = read_ids(v.attachments()?, MAX_ATTACHMENTS, AttachmentId, budget)?;
    valid_references(&mentions, reply_to.as_ref(), &attachments)?;
    Ok(MessageSummary {
        id: MessageId(read_id(v.id()?, budget)?),
        sequence,
        author: read_participant(v.author()?, budget, depth + 1)?,
        posted_at_ms: v.posted_at_ms()?,
        intent: v.intent()?.into(),
        mentions,
        reply_to,
        attachments,
    })
}

fn write_participant(
    b: &mut Builder,
    value: &Participant,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelParticipant>> {
    budget.node(depth)?;
    value.validate()?;
    let id = write_text(b, value.id.as_ref(), budget)?;
    let name = write_text(b, &value.display_name, budget)?;
    Ok(g::ChannelParticipant::create(
        b,
        id,
        g::ParticipantKind::from(value.kind),
        name,
    ))
}

fn read_participant(
    v: g::ChannelParticipantRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Participant> {
    budget.node(depth)?;
    Ok(Participant {
        id: ParticipantId(read_id(v.id()?, budget)?),
        kind: v.kind()?.into(),
        display_name: read_name(v.display_name()?, budget)?,
    })
}

fn write_settings(
    b: &mut Builder,
    value: ChannelSettings,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelSettings>> {
    budget.node(depth)?;
    Ok(g::ChannelSettings::create(
        b,
        g::WakePolicy::from(value.user_wake),
        g::WakePolicy::from(value.agent_wake),
        value.attention_questions,
        value.attention_reports,
    ))
}

fn read_settings(
    v: g::ChannelSettingsRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelSettings> {
    budget.node(depth)?;
    Ok(ChannelSettings {
        user_wake: v.user_wake()?.into(),
        agent_wake: v.agent_wake()?.into(),
        attention_questions: v.attention_questions()?,
        attention_reports: v.attention_reports()?,
    })
}

fn write_cursor(
    b: &mut Builder,
    value: &ChannelCursor,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelCursor>> {
    budget.node(depth)?;
    valid_id(value.channel_id.as_ref())?;
    let channel = write_text(b, value.channel_id.as_ref(), budget)?;
    Ok(g::ChannelCursor::create(b, channel, value.sequence))
}

fn read_cursor(
    v: g::ChannelCursorRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelCursor> {
    budget.node(depth)?;
    Ok(ChannelCursor {
        channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
        sequence: v.sequence()?,
    })
}

fn write_change(
    b: &mut Builder,
    value: &ChannelChange,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelChange>> {
    budget.node(depth)?;
    ensure!(value.sequence > 0, "zero channel change sequence");
    Ok(g::ChannelChange::create(
        b,
        value.sequence,
        g::ChannelChangeKind::from(value.kind),
    ))
}

fn read_change(
    v: g::ChannelChangeRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelChange> {
    budget.node(depth)?;
    let sequence = v.sequence()?;
    ensure!(sequence > 0, "zero channel change sequence");
    Ok(ChannelChange {
        sequence,
        kind: v.kind()?.into(),
    })
}

fn write_descriptor(
    b: &mut Builder,
    value: &AttachmentDescriptor,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::AttachmentDescriptor>> {
    budget.node(depth)?;
    valid_id(value.id.as_ref())?;
    valid_alt_text(value.alt_text.as_deref())?;
    valid_attachment_size(value.byte_len as usize)?;
    let id = write_text(b, value.id.as_ref(), budget)?;
    let alt = write_optional_text(b, value.alt_text.as_deref(), budget)?;
    Ok(g::AttachmentDescriptor::create(
        b,
        id,
        g::AttachmentMediaType::from(value.media_type),
        alt,
        value.byte_len,
    ))
}

fn read_descriptor(
    v: g::AttachmentDescriptorRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<AttachmentDescriptor> {
    budget.node(depth)?;
    let byte_len = v.byte_len()?;
    valid_attachment_size(byte_len as usize)?;
    Ok(AttachmentDescriptor {
        id: AttachmentId(read_id(v.id()?, budget)?),
        media_type: v.media_type()?.into(),
        alt_text: read_alt(v.alt_text()?, budget)?,
        byte_len,
    })
}

fn write_optional_sequence(
    b: &mut Builder,
    value: Option<u64>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<Offset<g::OptionalU64>>> {
    value
        .map(|value| {
            ensure!(value != 0, "channel history bound cannot be zero");
            budget.node(depth)?;
            Ok(g::OptionalU64::create(b, value))
        })
        .transpose()
}

fn read_optional_sequence(
    value: Option<g::OptionalU64Ref<'_>>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<u64>> {
    value
        .map(|value| {
            budget.node(depth)?;
            let value = value.value()?;
            ensure!(value != 0, "channel history bound cannot be zero");
            Ok(value)
        })
        .transpose()
}

fn read_id(value: &str, budget: &mut Budget) -> Result<String> {
    valid_id(value)?;
    read_text(value, budget)
}

fn read_name(value: &str, budget: &mut Budget) -> Result<String> {
    valid_name(value)?;
    read_text(value, budget)
}

fn read_body(value: &str, budget: &mut Budget) -> Result<String> {
    valid_post_text(value)?;
    read_text(value, budget)
}

fn read_alt(value: Option<&str>, budget: &mut Budget) -> Result<Option<String>> {
    valid_alt_text(value)?;
    value.map(|v| read_text(v, budget)).transpose()
}

fn write_optional_id(
    b: &mut Builder,
    value: Option<&impl AsRef<str>>,
    budget: &mut Budget,
) -> Result<Option<Offset<str>>> {
    write_optional_text(b, value.map(AsRef::as_ref), budget)
}

fn write_reply_id(
    b: &mut Builder,
    value: Option<&impl AsRef<str>>,
    budget: &mut Budget,
) -> Result<Option<Offset<str>>> {
    if let Some(value) = value {
        valid_id(value.as_ref())?;
    }
    write_optional_id(b, value, budget)
}

fn read_optional_id<T>(
    value: Option<&str>,
    id: impl Fn(String) -> T,
    budget: &mut Budget,
) -> Result<Option<T>> {
    value
        .map(|value| read_id(value, budget).map(id))
        .transpose()
}

fn write_ids(
    b: &mut Builder,
    values: &[impl AsRef<str>],
    budget: &mut Budget,
) -> Result<Offset<[Offset<str>]>> {
    budget.vector(values.len(), size_of::<Offset<str>>())?;
    let offsets = values
        .iter()
        .map(|value| write_text(b, value.as_ref(), budget))
        .collect::<Result<Vec<_>>>()?;
    Ok(b.create_vector(offsets.as_slice()))
}

fn read_ids<'a, T>(
    values: Vector<'a, planus::Result<&'a str>>,
    max: usize,
    id: impl Fn(String) -> T,
    budget: &mut Budget,
) -> Result<Vec<T>> {
    read_vector(values, max, budget, |value, budget| {
        read_id(value, budget).map(&id)
    })
}

fn write_page<T, G>(
    b: &mut Builder,
    values: &[T],
    budget: &mut Budget,
    write: impl Fn(&mut Builder, &T, &mut Budget, usize) -> Result<Offset<G>>,
) -> Result<Offset<[Offset<G>]>> {
    ensure!(
        values.len() <= usize::from(MAX_PAGE_LIMIT),
        "channel page exceeds limit"
    );
    budget.vector(values.len(), size_of::<Offset<G>>())?;
    let offsets = values
        .iter()
        .map(|value| write(b, value, budget, 1))
        .collect::<Result<Vec<_>>>()?;
    Ok(b.create_vector(offsets.as_slice()))
}

fn read_vector<'a, T: 'a, U>(
    values: Vector<'a, planus::Result<T>>,
    max: usize,
    budget: &mut Budget,
    read: impl Fn(T, &mut Budget) -> Result<U>,
) -> Result<Vec<U>>
where
    planus::Result<T>: planus::VectorRead<'a>,
{
    ensure!(values.len() <= max, "channel vector exceeds limit");
    budget.vector(values.len(), size_of::<U>())?;
    values.iter().map(|value| read(value?, budget)).collect()
}
