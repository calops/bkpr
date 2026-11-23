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
        ChannelRequest::ListArchivedWorkspaces { after, limit } => {
            let after = write_optional_id(b, after.as_ref(), budget)?;
            let offset = g::ChannelRequestListArchivedWorkspaces::create(b, after, *limit);
            G::create_channel_request_list_archived_workspaces(b, offset)
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
        ChannelRequest::ListExplicitMembers {
            channel_id,
            after,
            limit,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let after = write_optional_id(b, after.as_ref(), budget)?;
            let offset = g::ChannelRequestListExplicitMembers::create(b, channel, after, *limit);
            G::create_channel_request_list_explicit_members(b, offset)
        }
        ChannelRequest::UserRoster {
            workspace_id,
            channel_id,
            after,
            limit,
        } => {
            let workspace = write_text(b, workspace_id, budget)?;
            let channel = write_optional_id(b, channel_id.as_ref(), budget)?;
            let after = write_optional_id(b, after.as_ref(), budget)?;
            let offset = g::ChannelRequestUserRoster::create(b, workspace, channel, after, *limit);
            G::create_channel_request_user_roster(b, offset)
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
        ChannelRequest::PostUser {
            channel_id,
            post,
            bindings,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let post = write_post(b, post, budget, depth)?;
            let bindings = write_vector(b, bindings, budget, |b, binding, budget| {
                write_mention_binding(b, binding, budget, depth)
            })?;
            let offset = g::ChannelRequestPostUser::create(b, channel, post, bindings);
            G::create_channel_request_post_user(b, offset)
        }
        ChannelRequest::History {
            channel_id,
            before,
            after,
            limit,
        } => {
            let channel = write_text(b, channel_id.as_ref(), budget)?;
            let before = write_optional_sequence(b, *before, budget, depth)?;
            let after = write_optional_sequence(b, *after, budget, depth)?;
            let offset = g::ChannelRequestHistory::create(b, channel, before, *limit, after);
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
        ChannelRequest::CatalogSnapshot { after, limit } => {
            let after = after
                .as_ref()
                .map(|page| write_catalog_page(b, page, budget, depth))
                .transpose()?;
            let offset = g::ChannelRequestCatalogSnapshot::create(b, after, *limit);
            G::create_channel_request_catalog_snapshot(b, offset)
        }
        ChannelRequest::CatalogSubscribe {
            after,
            limit,
            wait_ms,
        } => {
            let after = write_catalog_cursor(b, after, budget, depth)?;
            let offset = g::ChannelRequestCatalogSubscribe::create(b, after, *limit, *wait_ms);
            G::create_channel_request_catalog_subscribe(b, offset)
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
        R::ChannelRequestListArchivedWorkspaces(v) => ChannelRequest::ListArchivedWorkspaces {
            after: read_optional_id(v.after()?, std::convert::identity, budget)?,
            limit: v.limit()?,
        },
        R::ChannelRequestListMembers(v) => ChannelRequest::ListMembers {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            after: read_optional_id(v.after()?, ParticipantId, budget)?,
            limit: v.limit()?,
        },
        R::ChannelRequestListExplicitMembers(v) => ChannelRequest::ListExplicitMembers {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            after: read_optional_id(v.after()?, ParticipantId, budget)?,
            limit: v.limit()?,
        },
        R::ChannelRequestUserRoster(v) => ChannelRequest::UserRoster {
            workspace_id: read_id(v.workspace_id()?, budget)?,
            channel_id: read_optional_id(v.channel_id()?, ChannelId, budget)?,
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
        R::ChannelRequestPostUser(v) => ChannelRequest::PostUser {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            post: read_post(v.post()?, budget, depth)?,
            bindings: read_vector(v.bindings()?, MAX_MENTIONS, budget, |value, budget| {
                read_mention_binding(value, budget, depth)
            })?,
        },
        R::ChannelRequestHistory(v) => ChannelRequest::History {
            channel_id: ChannelId(read_id(v.channel_id()?, budget)?),
            before: read_optional_sequence(v.before()?, budget, depth)?,
            after: read_optional_sequence(v.after()?, budget, depth)?,
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
        R::ChannelRequestCatalogSnapshot(v) => ChannelRequest::CatalogSnapshot {
            after: v
                .after()?
                .map(|page| read_catalog_page(page, budget, depth))
                .transpose()?,
            limit: v.limit()?,
        },
        R::ChannelRequestCatalogSubscribe(v) => ChannelRequest::CatalogSubscribe {
            after: read_catalog_cursor(v.after()?, budget, depth)?,
            limit: v.limit()?,
            wait_ms: v.wait_ms()?,
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
        ChannelResponse::ArchivedWorkspaces {
            workspace_ids,
            next_after,
        } => {
            let workspace_ids = write_id_page(b, workspace_ids, budget)?;
            let next_after = write_reply_id(b, next_after.as_ref(), budget)?;
            let offset = g::ChannelResponseArchivedWorkspaces::create(b, workspace_ids, next_after);
            G::create_channel_response_archived_workspaces(b, offset)
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
        ChannelResponse::UserRoster {
            participants,
            next_after,
        } => {
            let participants = write_page(b, participants, budget, write_roster_entry)?;
            let next_after = write_reply_id(b, next_after.as_ref(), budget)?;
            let offset = g::ChannelResponseUserRoster::create(b, participants, next_after);
            G::create_channel_response_user_roster(b, offset)
        }
        ChannelResponse::ExplicitMembers {
            members,
            next_after,
        } => {
            let members = write_id_page(b, members, budget)?;
            let next_after = write_reply_id(b, next_after.as_ref(), budget)?;
            let offset = g::ChannelResponseExplicitMembers::create(b, members, next_after);
            G::create_channel_response_explicit_members(b, offset)
        }
        ChannelResponse::Channel { channel } => {
            let channel = write_summary(b, channel, budget, 1)?;
            let offset = g::ChannelResponseChannel::create(b, channel);
            G::create_channel_response_channel(b, offset)
        }
        ChannelResponse::Posted {
            channel_id,
            message,
            wake_count,
            attention,
        } => {
            let message = write_message(b, message, budget, 1)?;
            let channel_id = write_reply_id(b, channel_id.as_ref(), budget)?;
            let offset =
                g::ChannelResponsePosted::create(b, message, *wake_count, *attention, channel_id);
            G::create_channel_response_posted(b, offset)
        }
        ChannelResponse::History {
            messages,
            next_before,
            next_after,
            cursor,
        } => {
            let mut remaining = MAX_HISTORY_BYTES;
            let messages = write_page(b, messages, budget, |b, message, budget, depth| {
                charge_history(&mut remaining, message)?;
                write_renderable_message(b, message, budget, depth)
            })?;
            validate_history_continuations(*next_before, *next_after)?;
            let next_before = write_optional_sequence(b, *next_before, budget, 1)?;
            let next_after = write_optional_sequence(b, *next_after, budget, 1)?;
            let cursor = write_cursor(b, cursor, budget, 1)?;
            let offset =
                g::ChannelResponseHistory::create(b, messages, next_before, next_after, cursor);
            G::create_channel_response_history(b, offset)
        }
        ChannelResponse::Message { message } => {
            let message = write_renderable_message(b, message, budget, 1)?;
            let offset = g::ChannelResponseMessage::create(b, message);
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
            status,
        } => {
            let cursor = write_cursor(b, cursor, budget, 1)?;
            let changes = write_page(b, changes, budget, write_change)?;
            let status = match status {
                ChannelSubscriptionStatus::Active => g::ChannelSubscriptionStatus::Active,
                ChannelSubscriptionStatus::Closed => {
                    ensure!(!more, "a closed subscription cannot have more changes");
                    g::ChannelSubscriptionStatus::Closed
                }
            };
            let offset = g::ChannelResponseChanges::create(b, cursor, changes, *more, status);
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
        ChannelResponse::CatalogSnapshot {
            channels,
            cursor,
            next_after,
        } => {
            let channels = write_page(b, channels, budget, write_summary)?;
            let cursor = write_catalog_cursor(b, cursor, budget, 1)?;
            let next_after = next_after
                .as_ref()
                .map(|page| write_catalog_page(b, page, budget, 1))
                .transpose()?;
            let offset = g::ChannelResponseCatalogSnapshot::create(b, channels, cursor, next_after);
            G::create_channel_response_catalog_snapshot(b, offset)
        }
        ChannelResponse::CatalogChanges {
            cursor,
            changes,
            more,
        } => {
            let cursor = write_catalog_cursor(b, cursor, budget, 1)?;
            let changes = write_page(b, changes, budget, write_catalog_change)?;
            let offset = g::ChannelResponseCatalogChanges::create(b, cursor, changes, *more);
            G::create_channel_response_catalog_changes(b, offset)
        }
        ChannelResponse::CatalogReset {} => {
            budget.node(1)?;
            let offset = g::ChannelResponseCatalogReset::create(b);
            G::create_channel_response_catalog_reset(b, offset)
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
        R::ChannelResponseArchivedWorkspaces(v) => ChannelResponse::ArchivedWorkspaces {
            workspace_ids: read_ids(
                v.workspace_ids()?,
                MAX_PAGE_LIMIT.into(),
                std::convert::identity,
                budget,
            )?,
            next_after: read_optional_id(v.next_after()?, std::convert::identity, budget)?,
        },
        R::ChannelResponseMembers(v) => ChannelResponse::Members {
            participants: read_vector(v.participants()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_participant(v, b, 1)
            })?,
            next_after: read_optional_id(v.next_after()?, ParticipantId, budget)?,
        },
        R::ChannelResponseUserRoster(v) => ChannelResponse::UserRoster {
            participants: read_vector(v.participants()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_roster_entry(v, b, 1)
            })?,
            next_after: read_optional_id(v.next_after()?, ParticipantId, budget)?,
        },
        R::ChannelResponseExplicitMembers(v) => ChannelResponse::ExplicitMembers {
            members: read_ids(v.members()?, MAX_PAGE_LIMIT.into(), ParticipantId, budget)?,
            next_after: read_optional_id(v.next_after()?, ParticipantId, budget)?,
        },
        R::ChannelResponseChannel(v) => ChannelResponse::Channel {
            channel: read_summary(v.channel()?, budget, 1)?,
        },
        R::ChannelResponsePosted(v) => ChannelResponse::Posted {
            channel_id: read_optional_id(v.channel_id()?, ChannelId, budget)?,
            message: read_message(v.message()?, budget, 1)?,
            wake_count: v.wake_count()?,
            attention: v.attention()?,
        },
        R::ChannelResponseHistory(v) => {
            let mut remaining = MAX_HISTORY_BYTES;
            let messages = read_vector(v.messages()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                let message = read_renderable_message(v, b, 1)?;
                charge_history(&mut remaining, &message)?;
                Ok(message)
            })?;
            let next_before = read_optional_sequence(v.next_before()?, budget, 1)?;
            let next_after = read_optional_sequence(v.next_after()?, budget, 1)?;
            validate_history_continuations(next_before, next_after)?;
            ChannelResponse::History {
                messages,
                next_before,
                next_after,
                cursor: read_cursor(v.cursor()?, budget, 1)?,
            }
        }
        R::ChannelResponseMessage(v) => ChannelResponse::Message {
            message: read_renderable_message(v.message()?, budget, 1)?,
        },
        R::ChannelResponseReadPosition(v) => ChannelResponse::ReadPosition {
            through: v.through()?,
        },
        R::ChannelResponseChanges(v) => {
            let more = v.more()?;
            let status = match v.status()? {
                g::ChannelSubscriptionStatus::Active => ChannelSubscriptionStatus::Active,
                g::ChannelSubscriptionStatus::Closed => {
                    ensure!(!more, "a closed subscription cannot have more changes");
                    ChannelSubscriptionStatus::Closed
                }
                g::ChannelSubscriptionStatus::Unspecified => {
                    anyhow::bail!("missing channel subscription status");
                }
            };
            ChannelResponse::Changes {
                cursor: read_cursor(v.cursor()?, budget, 1)?,
                changes: read_vector(v.changes()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                    read_change(v, b, 1)
                })?,
                more,
                status,
            }
        }
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
        R::ChannelResponseCatalogSnapshot(v) => ChannelResponse::CatalogSnapshot {
            channels: read_vector(v.channels()?, MAX_PAGE_LIMIT.into(), budget, |v, b| {
                read_summary(v, b, 1)
            })?,
            cursor: read_catalog_cursor(v.cursor()?, budget, 1)?,
            next_after: v
                .next_after()?
                .map(|page| read_catalog_page(page, budget, 1))
                .transpose()?,
        },
        R::ChannelResponseCatalogChanges(v) => ChannelResponse::CatalogChanges {
            cursor: read_catalog_cursor(v.cursor()?, budget, 1)?,
            changes: read_vector(
                v.changes()?,
                MAX_PAGE_LIMIT.into(),
                budget,
                read_catalog_change,
            )?,
            more: v.more()?,
        },
        R::ChannelResponseCatalogReset(_) => {
            budget.node(1)?;
            ChannelResponse::CatalogReset {}
        }
    })
}

pub(super) fn write_post(
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
    let source = write_source(b, value.source.as_deref(), budget, depth + 1)?;
    Ok(g::ChannelPost::create(
        b,
        key,
        text,
        g::PostIntent::from(value.intent),
        mentions,
        reply_to,
        attachments,
        source,
    ))
}

pub(super) fn read_post(
    v: g::ChannelPostRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelPost> {
    budget.node(depth)?;
    Ok(ChannelPost {
        idempotency_key: read_id(v.idempotency_key()?, budget)?,
        text: read_body(v.text()?, budget)?,
        intent: v.intent()?.into(),
        mentions: read_ids(v.mentions()?, MAX_MENTIONS, ParticipantId, budget)?,
        reply_to: read_optional_id(v.reply_to()?, MessageId, budget)?,
        attachments: read_ids(v.attachments()?, MAX_ATTACHMENTS, AttachmentId, budget)?,
        source: read_source(v.source()?, budget, depth + 1)?,
    })
}

fn write_source(
    b: &mut Builder,
    value: Option<&ChannelSource>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<Offset<g::ChannelSource>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    budget.node(depth)?;
    let channel_id = write_text(b, value.channel_id.as_ref(), budget)?;
    let message_id = write_text(b, value.message_id.as_ref(), budget)?;
    Ok(Some(g::ChannelSource::create(b, channel_id, message_id)))
}

fn read_source(
    value: Option<g::ChannelSourceRef<'_>>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Option<Box<ChannelSource>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    budget.node(depth)?;
    let channel_id = value.channel_id()?;
    let message_id = value.message_id()?;
    valid_source_ids(channel_id, message_id)?;
    Ok(Some(Box::new(ChannelSource {
        channel_id: ChannelId(read_text(channel_id, budget)?),
        message_id: MessageId(read_text(message_id, budget)?),
    })))
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
        value.direct,
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
        direct: v.direct()?,
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
    if let Some(source) = &value.source {
        valid_source_ids(source.channel_id.as_ref(), source.message_id.as_ref())?;
    }
    let id = write_text(b, value.id.as_ref(), budget)?;
    let author = write_participant(b, &value.author, budget, depth + 1)?;
    let mentions = write_ids(b, &value.mentions, budget)?;
    let reply = write_optional_id(b, value.reply_to.as_ref(), budget)?;
    let attachments = write_ids(b, &value.attachments, budget)?;
    let source = write_source(b, value.source.as_deref(), budget, depth + 1)?;
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
        source,
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
        source: read_source(v.source()?, budget, depth + 1)?,
    })
}

fn write_renderable_message(
    b: &mut Builder,
    value: &ChannelMessage,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelMessage>> {
    budget.node(depth)?;
    value.validate_attachments()?;
    validate_retained_body(&value.text)?;
    let message = write_message(b, &value.message, budget, depth + 1)?;
    let text = write_text(b, &value.text, budget)?;
    let attachments = write_vector(b, &value.attachments, budget, |b, value, budget| {
        write_descriptor(b, value, budget, depth + 1)
    })?;
    Ok(g::ChannelMessage::create(b, message, text, attachments))
}

fn read_renderable_message(
    v: g::ChannelMessageRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelMessage> {
    budget.node(depth)?;
    let text = v.text()?;
    validate_retained_body(text)?;
    let message = ChannelMessage {
        message: read_message(v.message()?, budget, depth + 1)?,
        text: read_text(text, budget)?,
        attachments: read_vector(v.attachments()?, MAX_ATTACHMENTS, budget, |v, b| {
            read_descriptor(v, b, depth + 1)
        })?,
    };
    message.validate_attachments()?;
    Ok(message)
}

fn validate_retained_body(text: &str) -> Result<()> {
    // Direct send_message preserves the original UTF-8 and has a larger bound
    // than the shared composer, whose separate request limits are unchanged.
    ensure!(
        text.len() <= MAX_DIRECT_MESSAGE_BYTES,
        "retained channel message exceeds byte limit"
    );
    Ok(())
}

fn charge_history(remaining: &mut usize, message: &ChannelMessage) -> Result<()> {
    *remaining = remaining
        .checked_sub(message.content_bytes())
        .ok_or_else(|| anyhow::anyhow!("channel history page exceeds byte limit"))?;
    Ok(())
}

fn validate_history_continuations(before: Option<u64>, after: Option<u64>) -> Result<()> {
    ensure!(
        before != Some(0) && after != Some(0),
        "channel history continuation cannot be zero"
    );
    Ok(())
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

fn write_mention_binding(
    b: &mut Builder,
    value: &ChannelMentionBinding,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelMentionBinding>> {
    budget.node(depth)?;
    let participant_id = write_text(b, value.participant_id.as_ref(), budget)?;
    let launch_id = write_text(b, &value.launch_id, budget)?;
    Ok(g::ChannelMentionBinding::create(
        b,
        participant_id,
        launch_id,
        value.binding_generation,
    ))
}

fn read_mention_binding(
    value: g::ChannelMentionBindingRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelMentionBinding> {
    budget.node(depth)?;
    let binding_generation = value.binding_generation()?;
    ensure!(
        binding_generation != 0,
        "missing channel mention binding generation"
    );
    Ok(ChannelMentionBinding {
        participant_id: ParticipantId(read_id(value.participant_id()?, budget)?),
        launch_id: read_id(value.launch_id()?, budget)?,
        binding_generation,
    })
}

fn write_roster_entry(
    b: &mut Builder,
    entry: &ChannelRosterEntry,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelRosterEntry>> {
    budget.node(depth)?;
    entry.validate_binding_relationship()?;
    let participant = write_participant(b, &entry.participant, budget, depth + 1)?;
    let binding = entry
        .binding
        .as_ref()
        .map(|value| {
            value.validate()?;
            write_mention_binding(b, value, budget, depth + 1)
        })
        .transpose()?;
    Ok(g::ChannelRosterEntry::create(b, participant, binding))
}

fn read_roster_entry(
    value: g::ChannelRosterEntryRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelRosterEntry> {
    budget.node(depth)?;
    let entry = ChannelRosterEntry {
        participant: read_participant(value.participant()?, budget, depth + 1)?,
        binding: value
            .binding()?
            .map(|value| read_mention_binding(value, budget, depth + 1))
            .transpose()?,
    };
    entry.validate_binding_relationship()?;
    Ok(entry)
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

fn write_catalog_cursor(
    b: &mut Builder,
    cursor: &ChannelCatalogCursor,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelCatalogCursor>> {
    budget.node(depth)?;
    valid_id(&cursor.store_id)?;
    let store = write_text(b, &cursor.store_id, budget)?;
    Ok(g::ChannelCatalogCursor::create(b, store, cursor.sequence))
}

fn read_catalog_cursor(
    cursor: g::ChannelCatalogCursorRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelCatalogCursor> {
    budget.node(depth)?;
    Ok(ChannelCatalogCursor {
        store_id: read_id(cursor.store_id()?, budget)?,
        sequence: cursor.sequence()?,
    })
}

fn write_catalog_page(
    b: &mut Builder,
    page: &ChannelCatalogPage,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelCatalogPage>> {
    budget.node(depth)?;
    valid_id(&page.workspace_id)?;
    valid_id(page.channel_id.as_ref())?;
    let cursor = write_catalog_cursor(b, &page.cursor, budget, depth + 1)?;
    let workspace = write_text(b, &page.workspace_id, budget)?;
    let channel = write_text(b, page.channel_id.as_ref(), budget)?;
    Ok(g::ChannelCatalogPage::create(b, cursor, workspace, channel))
}

fn read_catalog_page(
    page: g::ChannelCatalogPageRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelCatalogPage> {
    budget.node(depth)?;
    Ok(ChannelCatalogPage {
        cursor: read_catalog_cursor(page.cursor()?, budget, depth + 1)?,
        workspace_id: read_id(page.workspace_id()?, budget)?,
        channel_id: ChannelId(read_id(page.channel_id()?, budget)?),
    })
}

fn write_catalog_change(
    b: &mut Builder,
    change: &ChannelCatalogChange,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelCatalogChange>> {
    use g::ChannelCatalogChangeBody as Body;
    budget.node(depth)?;
    budget.node(depth + 1)?;
    let body = match change {
        ChannelCatalogChange::Upsert { channel, attention } => {
            let channel = write_summary(b, channel, budget, depth + 2)?;
            let attention = attention
                .as_ref()
                .map(|attention| write_catalog_attention(b, attention, budget, depth + 2))
                .transpose()?;
            let offset = g::ChannelCatalogUpsert::create(b, channel, attention);
            Body::create_channel_catalog_upsert(b, offset)
        }
        ChannelCatalogChange::WorkspaceClosed { workspace_id } => {
            valid_id(workspace_id)?;
            let workspace = write_text(b, workspace_id, budget)?;
            let offset = g::ChannelCatalogWorkspaceClosed::create(b, workspace);
            Body::create_channel_catalog_workspace_closed(b, offset)
        }
    };
    Ok(g::ChannelCatalogChange::create(b, body))
}

fn read_catalog_change(
    change: g::ChannelCatalogChangeRef<'_>,
    budget: &mut Budget,
) -> Result<ChannelCatalogChange> {
    use g::ChannelCatalogChangeBodyRef as Body;
    budget.node(1)?;
    budget.node(2)?;
    Ok(match change.body()? {
        Body::ChannelCatalogUpsert(v) => ChannelCatalogChange::Upsert {
            channel: read_summary(v.channel()?, budget, 3)?,
            attention: v
                .attention()?
                .map(|attention| read_catalog_attention(attention, budget, 3))
                .transpose()?,
        },
        Body::ChannelCatalogWorkspaceClosed(v) => ChannelCatalogChange::WorkspaceClosed {
            workspace_id: read_id(v.workspace_id()?, budget)?,
        },
    })
}

fn write_catalog_attention(
    b: &mut Builder,
    value: &ChannelAttention,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::ChannelAttention>> {
    budget.node(depth)?;
    valid_id(value.message_id.as_ref())?;
    validate_catalog_attention(value.sequence, value.intent)?;
    let message_id = write_text(b, value.message_id.as_ref(), budget)?;
    Ok(g::ChannelAttention::create(
        b,
        message_id,
        value.sequence,
        g::PostIntent::from(value.intent),
        value.posted_at_ms,
    ))
}

fn read_catalog_attention(
    value: g::ChannelAttentionRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<ChannelAttention> {
    budget.node(depth)?;
    let sequence = value.sequence()?;
    let intent = value.intent()?.into();
    validate_catalog_attention(sequence, intent)?;
    Ok(ChannelAttention {
        message_id: MessageId(read_id(value.message_id()?, budget)?),
        sequence,
        intent,
        posted_at_ms: value.posted_at_ms()?,
    })
}

fn validate_catalog_attention(sequence: u64, intent: PostIntent) -> Result<()> {
    ensure!(sequence > 0, "zero catalog attention post sequence");
    ensure!(
        intent != PostIntent::Note,
        "ordinary post cannot carry catalog attention"
    );
    Ok(())
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
            Ok(value.value()?)
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
    write_vector(b, values, budget, |b, value, budget| {
        write_text(b, value.as_ref(), budget)
    })
}

fn write_id_page(
    b: &mut Builder,
    values: &[impl AsRef<str>],
    budget: &mut Budget,
) -> Result<Offset<[Offset<str>]>> {
    write_page(b, values, budget, |b, value, budget, _| {
        valid_id(value.as_ref())?;
        write_text(b, value.as_ref(), budget)
    })
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

fn write_page<T, G: ?Sized>(
    b: &mut Builder,
    values: &[T],
    budget: &mut Budget,
    mut write: impl FnMut(&mut Builder, &T, &mut Budget, usize) -> Result<Offset<G>>,
) -> Result<Offset<[Offset<G>]>> {
    ensure!(
        values.len() <= usize::from(MAX_PAGE_LIMIT),
        "channel page exceeds limit"
    );
    write_vector(b, values, budget, |b, value, budget| {
        write(b, value, budget, 1)
    })
}

fn write_vector<T, G: ?Sized>(
    b: &mut Builder,
    values: &[T],
    budget: &mut Budget,
    mut write: impl FnMut(&mut Builder, &T, &mut Budget) -> Result<Offset<G>>,
) -> Result<Offset<[Offset<G>]>> {
    budget.vector(values.len(), size_of::<Offset<G>>())?;
    let offsets = values
        .iter()
        .map(|value| write(b, value, budget))
        .collect::<Result<Vec<_>>>()?;
    Ok(b.create_vector(offsets.as_slice()))
}

fn read_vector<'a, T: 'a, U>(
    values: Vector<'a, planus::Result<T>>,
    max: usize,
    budget: &mut Budget,
    mut read: impl FnMut(T, &mut Budget) -> Result<U>,
) -> Result<Vec<U>>
where
    planus::Result<T>: planus::VectorRead<'a>,
{
    ensure!(values.len() <= max, "channel vector exceeds limit");
    budget.vector(values.len(), size_of::<U>())?;
    values.iter().map(|value| read(value?, budget)).collect()
}
