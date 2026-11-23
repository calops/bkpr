//! Session-local channel requests, durable records and bounded read models.
//!
//! The receiving session supplies the authenticated actor and determines workspace
//! access. No request carries an author, session selector or live launch authority.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum records in a list, roster, history or subscription page.
pub const MAX_PAGE_LIMIT: u16 = 64;
/// Maximum summed [`ChannelMessage::content_bytes`] in one history page.
pub const MAX_HISTORY_BYTES: usize = 256 * 1024;
/// Maximum subscription wait in milliseconds; zero performs a nonblocking read.
pub const MAX_WAIT_MS: u32 = 5_000;
/// Maximum participants in an explicit membership set.
pub const MAX_MEMBERS: usize = 256;
/// Maximum resolved mentions in one post.
pub const MAX_MENTIONS: usize = 32;
/// Maximum attachment references in one post.
pub const MAX_ATTACHMENTS: usize = 8;
/// Maximum UTF-8 bytes in one post body.
pub const MAX_POST_BYTES: usize = 16 * 1024;
/// Maximum encoded bytes in one raster attachment.
pub const MAX_ATTACHMENT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum UTF-8 bytes in an opaque identifier or idempotency key.
pub const MAX_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes in a channel name or participant display label.
pub const MAX_NAME_BYTES: usize = 160;
/// Maximum UTF-8 bytes in an attachment description.
pub const MAX_ALT_TEXT_BYTES: usize = 512;

macro_rules! identity {
    ($(#[$doc:meta] $name:ident),+ $(,)?) => {$(
        #[$doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(
            /// Opaque backend-assigned value; not a display name or credential.
            pub String,
        );

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    )+};
}

identity! {
    /// Stable channel identity in the owning recoverable session.
    ChannelId,
    /// Immutable identity of one committed message.
    MessageId,
    /// Stable workspace participant identity, distinct from a live launch.
    ParticipantId,
    /// Immutable identity of a session-owned channel attachment.
    AttachmentId,
}

/// Which authorized workspace participants are channel members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipMode {
    /// Use the explicit participant set.
    Explicit,
    /// Include authorized workspace participants, including future arrivals.
    WorkspaceWide,
}

/// Select eligible agent recipients at commit time. The author is never a target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakePolicy {
    /// Request wakes for all eligible agent members.
    #[default]
    All,
    /// Request wakes for addressed eligible agents. Fall back to all eligible agents
    /// only if no agent was mentioned; a user mention alone does not suppress that fallback.
    AddressedOrAll,
    /// Request wakes only for explicitly addressed eligible agents.
    AddressedOnly,
    /// Retain the post without requesting agent wakes.
    None,
}

/// Per-author routing and user attention policy for an ordinary channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSettings {
    /// Routing for user-authored posts.
    pub user_wake: WakePolicy,
    /// Routing for agent-authored posts.
    pub agent_wake: WakePolicy,
    /// Whether an agent's question requests user attention.
    pub attention_questions: bool,
    /// Whether an agent's report requests user attention.
    pub attention_reports: bool,
}

/// Backend-established participant category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    /// Authenticated session user.
    User,
    /// Authenticated workspace agent participant.
    Agent,
}

/// Participant reference and display label, snapshotted when used as a post author.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Participant {
    /// Stable identity used by explicit membership and mentions.
    pub id: ParticipantId,
    /// Authoritative category, not inferred from the display name.
    pub kind: ParticipantKind,
    /// Display label; nonunique and not an authorization selector.
    pub display_name: String,
}

impl Participant {
    /// Validate the stable identity and display label before persisting this participant.
    /// This does not establish the caller's authority to act as the participant.
    pub fn validate(&self) -> Result<()> {
        valid_id(self.id.as_ref())?;
        valid_name(&self.display_name)
    }
}

/// User-attention intent selected by the author, distinct from wake routing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostIntent {
    /// Ordinary conversation, without an attention signal.
    #[default]
    Note,
    /// A question for the user.
    Question,
    /// A result or finding for the user.
    Report,
}

/// A post retry is equivalent only when its authenticated author, channel and every field match.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPost {
    /// Caller-chosen key; repeat only for an identical request.
    pub idempotency_key: String,
    /// Plain UTF-8 source. Routing never parses mentions from the body.
    pub text: String,
    /// Optional semantic attention signal; ordinary posts use [`PostIntent::Note`].
    #[serde(default)]
    pub intent: PostIntent,
    /// Explicit stable participant references resolved by the backend at admission.
    #[serde(default)]
    pub mentions: Vec<ParticipantId>,
    /// One existing message in the same channel, not a separate thread.
    pub reply_to: Option<MessageId>,
    /// Immutable resources uploaded into the same channel.
    #[serde(default)]
    pub attachments: Vec<AttachmentId>,
}

impl fmt::Debug for ChannelPost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelPost").finish_non_exhaustive()
    }
}

/// Supported raster attachment media types; never inferred from a path or URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentMediaType {
    /// A bounded PNG raster image.
    #[serde(rename = "image/png")]
    Png,
}

/// Upload bytes for a channel-owned immutable attachment.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentUpload {
    /// Format the uploaded bytes must decode as.
    pub media_type: AttachmentMediaType,
    /// Optional display or fallback description.
    pub alt_text: Option<String>,
    /// Encoded raster bytes; diagnostic JSON uses padded standard Base64.
    #[serde(with = "attachment_bytes")]
    pub bytes: Vec<u8>,
}

impl fmt::Debug for AttachmentUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentUpload")
            .field("media_type", &self.media_type)
            .field("byte_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Immutable attachment metadata. Only a fetch returns the raster bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentDescriptor {
    /// Resource reference usable only in its owning channel.
    pub id: AttachmentId,
    /// Validated format of the stored raster.
    pub media_type: AttachmentMediaType,
    /// Optional display or fallback description.
    pub alt_text: Option<String>,
    /// Exact size of the encoded raster in bytes.
    pub byte_len: u32,
}

/// Position in one durable channel's change stream, separate from post sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelCursor {
    /// Channel whose change stream issued this cursor.
    pub channel_id: ChannelId,
    /// Last examined position, including any filtered changes; zero is the beginning.
    pub sequence: u64,
}

/// Type of change for a subscriber to reconcile with the typed read operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelChangeKind {
    /// A new message was committed.
    Post,
    /// Channel policy changed.
    Settings,
    /// The explicit membership or membership mode changed.
    Membership,
    /// The authenticated caller's read position advanced.
    ReadPosition,
}

/// Durable change metadata; contains no message body or notification delivery request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelChange {
    /// Monotonic position in the channel's change stream.
    pub sequence: u64,
    /// Kind of record to refresh.
    pub kind: ChannelChangeKind,
}

/// Channel discovery metadata and the requesting participant's read position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSummary {
    /// Stable identity, independent of the display name.
    pub id: ChannelId,
    /// Owning workspace, independent of its display name.
    pub workspace_id: String,
    /// User-visible channel name.
    pub name: String,
    /// Effective membership mode.
    pub membership: MembershipMode,
    /// Effective wake and attention policy.
    pub settings: ChannelSettings,
    /// Newest committed post sequence, or zero for an empty channel.
    pub last_sequence: u64,
    /// Last post sequence marked read by the authenticated participant, or zero.
    pub read_through: u64,
    /// An unread post qualified for user attention at commit time; false for agents.
    pub unread_attention: bool,
}

/// Ordered message metadata, without the body or attachment bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSummary {
    /// Immutable message identity.
    pub id: MessageId,
    /// Monotonic post sequence within the channel, starting at one.
    pub sequence: u64,
    /// Backend-established author and display label at commit time.
    pub author: Participant,
    /// Backend commit timestamp in Unix milliseconds.
    pub posted_at_ms: u64,
    /// Semantic attention signal on the post.
    pub intent: PostIntent,
    /// Resolved stable workspace participant references.
    pub mentions: Vec<ParticipantId>,
    /// Optional reply target in the same channel.
    pub reply_to: Option<MessageId>,
    /// Channel-scoped immutable attachment references.
    pub attachments: Vec<AttachmentId>,
}

/// One retained post ready to render without fetching its body or attachment metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelMessage {
    /// Committed message metadata.
    pub message: MessageSummary,
    /// Original UTF-8 body.
    pub text: String,
    /// Metadata for the attachment references in `message`, in the same order.
    pub attachments: Vec<AttachmentDescriptor>,
}

impl ChannelMessage {
    /// Count UTF-8 bytes of the body and variable-size metadata, including reference
    /// identities. Raster bytes are fetched separately and do not count toward this budget.
    pub fn content_bytes(&self) -> usize {
        let message = &self.message;
        self.text.len()
            + message.id.as_ref().len()
            + message.author.id.as_ref().len()
            + message.author.display_name.len()
            + message
                .mentions
                .iter()
                .map(|id| id.as_ref().len())
                .sum::<usize>()
            + message.reply_to.as_ref().map_or(0, |id| id.as_ref().len())
            + message
                .attachments
                .iter()
                .map(|id| id.as_ref().len())
                .sum::<usize>()
            + self
                .attachments
                .iter()
                .map(|attachment| {
                    attachment.id.as_ref().len()
                        + attachment.alt_text.as_ref().map_or(0, String::len)
                })
                .sum::<usize>()
    }

    pub(crate) fn validate_attachments(&self) -> Result<()> {
        ensure!(
            self.attachments.len() == self.message.attachments.len()
                && self
                    .attachments
                    .iter()
                    .zip(&self.message.attachments)
                    .all(|(descriptor, id)| descriptor.id == *id),
            "channel attachment descriptors do not match post references"
        );
        Ok(())
    }
}

/// Whether a bounded subscription can be resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSubscriptionStatus {
    /// Continue from the returned cursor, including after an idle timeout.
    Active,
    /// No later changes can be obtained through this subscription. Render any
    /// changes included in this final page, but do not poll again.
    Closed,
}

/// Session-local channel operations. The transport supplies the authenticated caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelRequest {
    /// List accessible channels in one workspace, ordered by opaque identity.
    List {
        /// Exact workspace identity, never a display name.
        workspace_id: String,
        /// Exclude this identity and earlier identities, or begin at the first channel.
        after: Option<ChannelId>,
        /// Maximum number of channels to return.
        limit: u16,
    },
    /// List closed workspaces in this session with retained channels, ordered by opaque
    /// identity. Archived workspaces need not have a recoverable display name.
    ListArchivedWorkspaces {
        /// Exclude this identity and earlier identities, or start at the first archive.
        after: Option<String>,
        /// Maximum number of workspace identities to return.
        limit: u16,
    },
    /// List currently authorized members for mention selection, ordered by opaque identity.
    ListMembers {
        /// Channel to inspect.
        channel_id: ChannelId,
        /// Exclude this identity and earlier identities, or begin at the first member.
        after: Option<ParticipantId>,
        /// Maximum number of members to return.
        limit: u16,
    },
    /// List saved explicit member IDs, including inactive members. This is not the
    /// current mention roster; workspace-wide channels return an empty set.
    ListExplicitMembers {
        /// Channel to inspect.
        channel_id: ChannelId,
        /// Exclude this identity and earlier identities, or begin at the first saved member.
        after: Option<ParticipantId>,
        /// Maximum number of saved member IDs to return.
        limit: u16,
    },
    /// Create an ordinary channel in one accessible workspace.
    Create {
        /// Exact workspace identity.
        workspace_id: String,
        /// Display name; no name changes behavior or grants authority.
        name: String,
        /// Explicit or dynamically derived workspace membership.
        membership: MembershipMode,
        /// Initial explicit members; must be empty for workspace-wide membership.
        members: Vec<ParticipantId>,
        /// Initial wake and attention policy.
        settings: ChannelSettings,
    },
    /// Replace policy for future posts without changing already committed notification work.
    SetSettings {
        /// Channel to manage.
        channel_id: ChannelId,
        /// Replacement policy.
        settings: ChannelSettings,
    },
    /// Replace the membership mode and explicit member set.
    SetMembership {
        /// Channel to manage.
        channel_id: ChannelId,
        /// Replacement membership mode.
        membership: MembershipMode,
        /// Replacement explicit set; must be empty for workspace-wide membership.
        members: Vec<ParticipantId>,
    },
    /// Commit one post and its notification work atomically.
    Post {
        /// Channel to post in.
        channel_id: ChannelId,
        /// Body, retry key and structured references.
        post: ChannelPost,
    },
    /// Read a byte-bounded page of renderable posts without advancing read position.
    /// Supply either `before` or `after`, not both.
    History {
        /// Channel to read.
        channel_id: ChannelId,
        /// Exclude this sequence and newer posts when reading backward.
        before: Option<u64>,
        /// Exclude this sequence and older posts when reading forward; zero starts
        /// at the oldest post. If neither bound is supplied, read from the newest post.
        after: Option<u64>,
        /// Maximum number of posts, which may be reduced by [`MAX_HISTORY_BYTES`].
        limit: u16,
    },
    /// Read one message, without moving the caller's read position.
    Read {
        /// Owning channel.
        channel_id: ChannelId,
        /// Exact message to read.
        message_id: MessageId,
    },
    /// Advance this participant's read position, independent of notification delivery.
    MarkRead {
        /// Channel the participant attended.
        channel_id: ChannelId,
        /// Newest rendered post sequence, inclusive; never rewinds the read position.
        through: u64,
    },
    /// Read bounded changes after a durable cursor, optionally waiting for new changes.
    Subscribe {
        /// Exact channel whose membership the backend must recheck.
        channel_id: ChannelId,
        /// Resume after this cursor; absence establishes a fresh current baseline.
        after: Option<ChannelCursor>,
        /// Maximum number of changes to return.
        limit: u16,
        /// At most [`MAX_WAIT_MS`], with zero for an immediate read.
        wait_ms: u32,
    },
    /// Validate and store one immutable raster attachment.
    UploadAttachment {
        /// Channel which will own the resource.
        channel_id: ChannelId,
        /// Bytes supplied by the caller, not a path to read on the server.
        upload: AttachmentUpload,
    },
    /// Fetch one raster attachment under the owning channel's current access policy.
    GetAttachment {
        /// Owning channel.
        channel_id: ChannelId,
        /// Exact immutable resource to fetch.
        attachment_id: AttachmentId,
    },
}

/// Typed channel replies. Only an attachment fetch includes encoded raster bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelResponse {
    /// Page of channels accessible to the caller.
    Listed {
        /// Channels ordered by identity.
        channels: Vec<ChannelSummary>,
        /// Exclusive continuation when more records are available.
        next_after: Option<ChannelId>,
    },
    /// Page of closed workspaces with retained channels.
    ArchivedWorkspaces {
        /// Exact workspace IDs, ordered by identity. No display names are retained here.
        workspace_ids: Vec<String>,
        /// Exclusive continuation when more archives are available.
        next_after: Option<String>,
    },
    /// Page of current channel participants.
    Members {
        /// Participants ordered by identity.
        participants: Vec<Participant>,
        /// Exclusive continuation when more records are available.
        next_after: Option<ParticipantId>,
    },
    /// Page of saved explicit member IDs, independent of the current roster.
    ExplicitMembers {
        /// Saved member IDs ordered by identity, including members not currently present.
        members: Vec<ParticipantId>,
        /// Exclusive continuation when more saved members are available.
        next_after: Option<ParticipantId>,
    },
    /// Created or updated channel state.
    Channel {
        /// State at the successful mutation.
        channel: ChannelSummary,
    },
    /// Durable acceptance of a post, not delivery or processing acknowledgement.
    Posted {
        /// Original committed post metadata, including on an identical retry.
        message: MessageSummary,
        /// Number of frozen wake targets, not the number reached.
        wake_count: u32,
        /// Whether the commit created user-attention work.
        attention: bool,
    },
    /// Bounded retained history and a change-stream baseline from the same snapshot.
    History {
        /// Renderable posts in ascending sequence order, regardless of paging direction.
        messages: Vec<ChannelMessage>,
        /// Exclusive bound for the next older page, when available.
        next_before: Option<u64>,
        /// Exclusive bound for the next newer page, when available.
        next_after: Option<u64>,
        /// Subscribe after this position to reconcile changes since this snapshot.
        cursor: ChannelCursor,
    },
    /// One retained post ready to render.
    Message {
        /// Original body, committed metadata and lightweight attachment descriptors.
        message: ChannelMessage,
    },
    /// Confirm the effective read position for the authenticated participant.
    ReadPosition {
        /// Last post sequence marked read, inclusive.
        through: u64,
    },
    /// Subscription page; retention does not depend on subscriber speed.
    Changes {
        /// Position to use when resuming, even if no visible changes were returned.
        cursor: ChannelCursor,
        /// Visible changes in ascending stream order.
        changes: Vec<ChannelChange>,
        /// More changes are immediately available after the cursor.
        more: bool,
        /// Whether to continue polling after consuming this page; a closed page never has `more`.
        status: ChannelSubscriptionStatus,
    },
    /// Durable acceptance of an upload.
    AttachmentUploaded {
        /// Reference and metadata of the stored resource.
        attachment: AttachmentDescriptor,
    },
    /// Fetched immutable attachment.
    Attachment {
        /// Metadata of the resource.
        attachment: AttachmentDescriptor,
        /// Encoded raster; native decoding shares the receive allocation, and
        /// diagnostic JSON uses padded standard Base64.
        #[serde(with = "attachment_bytes")]
        bytes: bytes::Bytes,
    },
}

impl fmt::Debug for ChannelResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = match self {
            Self::Listed { .. } => "Listed",
            Self::ArchivedWorkspaces { .. } => "ArchivedWorkspaces",
            Self::Members { .. } => "Members",
            Self::ExplicitMembers { .. } => "ExplicitMembers",
            Self::Channel { .. } => "Channel",
            Self::Posted { .. } => "Posted",
            Self::History { .. } => "History",
            Self::Message { .. } => "Message",
            Self::ReadPosition { .. } => "ReadPosition",
            Self::Changes { .. } => "Changes",
            Self::AttachmentUploaded { .. } => "AttachmentUploaded",
            Self::Attachment { .. } => "Attachment",
        };
        f.debug_struct(variant).finish_non_exhaustive()
    }
}

mod attachment_bytes {
    use super::{MAX_ATTACHMENT_BYTES, valid_attachment_size};
    use base64::{Engine, display::Base64Display, engine::general_purpose::STANDARD};
    use serde::{Deserializer, Serializer, de, ser};
    use std::fmt;

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        valid_attachment_size(bytes.len()).map_err(<S::Error as ser::Error>::custom)?;
        serializer.collect_str(&Base64Display::new(bytes, &STANDARD))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>, T: From<Vec<u8>>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        struct AttachmentBytes;

        impl de::Visitor<'_> for AttachmentBytes {
            type Value = Vec<u8>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded padded standard Base64 attachment bytes")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Vec<u8>, E> {
                if text.len() > MAX_ATTACHMENT_BYTES.div_ceil(3) * 4
                    || !text.len().is_multiple_of(4)
                {
                    return Err(E::custom("invalid Base64 attachment length"));
                }
                let padding = if text.ends_with("==") {
                    2
                } else {
                    usize::from(text.ends_with('='))
                };
                valid_attachment_size(text.len() / 4 * 3 - padding).map_err(E::custom)?;
                STANDARD.decode(text).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(AttachmentBytes).map(T::from)
    }
}

impl ChannelRequest {
    /// Whether this request only observes channel state and may use a read-only attachment.
    pub fn is_observational(&self) -> bool {
        matches!(
            self,
            Self::List { .. }
                | Self::ListArchivedWorkspaces { .. }
                | Self::ListMembers { .. }
                | Self::ListExplicitMembers { .. }
                | Self::History { .. }
                | Self::Read { .. }
                | Self::Subscribe { .. }
                | Self::GetAttachment { .. }
        )
    }

    /// Validate bounded fields before queueing or dispatch. Access, reference existence,
    /// idempotency, cursor range and raster validity are receiving-service responsibilities.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::List {
                workspace_id,
                after,
                limit,
            } => {
                valid_id(workspace_id)?;
                valid_optional_id(after.as_ref())?;
                valid_limit(*limit)
            }
            Self::ListArchivedWorkspaces { after, limit } => {
                valid_optional_id(after.as_ref())?;
                valid_limit(*limit)
            }
            Self::ListMembers {
                channel_id,
                after,
                limit,
            }
            | Self::ListExplicitMembers {
                channel_id,
                after,
                limit,
            } => {
                valid_id(channel_id.as_ref())?;
                valid_optional_id(after.as_ref())?;
                valid_limit(*limit)
            }
            Self::Create {
                workspace_id,
                name,
                membership,
                members,
                ..
            } => {
                valid_id(workspace_id)?;
                valid_name(name)?;
                valid_members(*membership, members)
            }
            Self::SetSettings { channel_id, .. } | Self::MarkRead { channel_id, .. } => {
                valid_id(channel_id.as_ref())
            }
            Self::SetMembership {
                channel_id,
                membership,
                members,
            } => {
                valid_id(channel_id.as_ref())?;
                valid_members(*membership, members)
            }
            Self::Post { channel_id, post } => {
                valid_id(channel_id.as_ref())?;
                valid_id(&post.idempotency_key)?;
                valid_post_text(&post.text)?;
                ensure!(
                    !post.text.trim().is_empty() || !post.attachments.is_empty(),
                    "channel post requires text or an attachment"
                );
                valid_references(&post.mentions, post.reply_to.as_ref(), &post.attachments)
            }
            Self::History {
                channel_id,
                before,
                after,
                limit,
            } => {
                valid_id(channel_id.as_ref())?;
                ensure!(*before != Some(0), "channel history bound cannot be zero");
                ensure!(
                    before.is_none() || after.is_none(),
                    "channel history cannot use both before and after bounds"
                );
                valid_limit(*limit)
            }
            Self::Read {
                channel_id,
                message_id,
            } => {
                valid_id(channel_id.as_ref())?;
                valid_id(message_id.as_ref())
            }
            Self::Subscribe {
                channel_id,
                after,
                limit,
                wait_ms,
            } => {
                valid_id(channel_id.as_ref())?;
                ensure!(
                    after
                        .as_ref()
                        .is_none_or(|cursor| cursor.channel_id == *channel_id),
                    "channel cursor belongs to a different channel"
                );
                ensure!(*wait_ms <= MAX_WAIT_MS, "channel wait exceeds limit");
                valid_limit(*limit)
            }
            Self::UploadAttachment { channel_id, upload } => {
                valid_id(channel_id.as_ref())?;
                valid_alt_text(upload.alt_text.as_deref())?;
                valid_attachment_size(upload.bytes.len())
            }
            Self::GetAttachment {
                channel_id,
                attachment_id,
            } => {
                valid_id(channel_id.as_ref())?;
                valid_id(attachment_id.as_ref())
            }
        }
    }
}

pub(crate) fn valid_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control),
        "invalid channel identity or idempotency key"
    );
    Ok(())
}

pub(crate) fn valid_name(value: &str) -> Result<()> {
    ensure!(
        value.len() <= MAX_NAME_BYTES
            && !value.trim().is_empty()
            && !value.chars().any(char::is_control),
        "invalid channel name or display label"
    );
    Ok(())
}

pub(crate) fn valid_post_text(value: &str) -> Result<()> {
    ensure!(
        value.len() <= MAX_POST_BYTES
            && !value
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t')),
        "invalid channel post body"
    );
    Ok(())
}

pub(crate) fn valid_alt_text(value: Option<&str>) -> Result<()> {
    ensure!(
        value.is_none_or(
            |text| text.len() <= MAX_ALT_TEXT_BYTES && !text.chars().any(char::is_control)
        ),
        "invalid attachment alt text"
    );
    Ok(())
}

pub(crate) fn valid_attachment_size(size: usize) -> Result<()> {
    ensure!(
        (1..=MAX_ATTACHMENT_BYTES).contains(&size),
        "channel attachment size outside bounds"
    );
    Ok(())
}

pub(crate) fn valid_references(
    mentions: &[ParticipantId],
    reply_to: Option<&MessageId>,
    attachments: &[AttachmentId],
) -> Result<()> {
    valid_ids(mentions, MAX_MENTIONS)?;
    valid_optional_id(reply_to)?;
    valid_ids(attachments, MAX_ATTACHMENTS)
}

fn valid_members(mode: MembershipMode, members: &[ParticipantId]) -> Result<()> {
    ensure!(
        mode == MembershipMode::Explicit || members.is_empty(),
        "workspace-wide channel cannot have explicit members"
    );
    valid_ids(members, MAX_MEMBERS)
}

fn valid_ids<T: AsRef<str>>(values: &[T], max: usize) -> Result<()> {
    ensure!(values.len() <= max, "channel reference count exceeds limit");
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    for value in values {
        valid_id(value.as_ref())?;
        ensure!(seen.insert(value.as_ref()), "duplicate channel reference");
    }
    Ok(())
}

fn valid_optional_id(value: Option<&impl AsRef<str>>) -> Result<()> {
    value.map_or(Ok(()), |id| valid_id(id.as_ref()))
}

fn valid_limit(limit: u16) -> Result<()> {
    ensure!(
        (1..=MAX_PAGE_LIMIT).contains(&limit),
        "channel page limit outside bounds"
    );
    Ok(())
}
