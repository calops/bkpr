use super::ChannelAuthority;
use anyhow::{Context, Result, ensure};
use bkpr_protocol::channels::{
    AttachmentDescriptor, AttachmentId, AttachmentUpload, ChannelChange, ChannelChangeKind,
    ChannelId, ChannelPost, ChannelRequest, ChannelResponse, ChannelSettings, ChannelSummary,
    MembershipMode, MessageId, MessageSummary, Participant, ParticipantId, ParticipantKind,
    PostIntent, WakePolicy,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Transaction, params};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::ErrorKind,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const APPLICATION_ID: i64 = 0x424b4348;
const STORE_NAME: &str = "channels.sqlite3";
const MAX_PENDING: usize = 4096;

pub(super) struct Store {
    pub(super) connection: Connection,
}

struct StoredChannel {
    workspace_id: String,
    name: String,
    workspace_wide: bool,
    settings: ChannelSettings,
    last_sequence: u64,
    change_sequence: u64,
    last_attention: u64,
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .context("open channel session directory")?;
        let metadata = directory.metadata()?;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "channel session directory is not private"
        );
        for sidecar in [
            "channels.sqlite3-journal",
            "channels.sqlite3-wal",
            "channels.sqlite3-shm",
        ] {
            open_private(&root.join(sidecar), false)?;
        }
        let path = root.join(STORE_NAME);
        let file = open_private(&path, true)?.context("missing channel store")?;
        let empty = file.metadata()?.len() == 0;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::from_bits_retain(rusqlite::ffi::SQLITE_OPEN_NOFOLLOW);
        let mut connection = Connection::open_with_flags(path, flags)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let application: i64 =
            connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            (empty && application == 0 && version == 0)
                || (application == APPLICATION_ID && version == 1),
            "foreign or unsupported channel store"
        );
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF;
             PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=0;
             PRAGMA cache_size=-1024;",
        )?;
        if empty {
            let tx = connection.transaction()?;
            tx.execute_batch(include_str!("schema.sql"))?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
            directory.sync_all()?;
        } else {
            // Retain frozen work as history; a new runtime must not adopt old wakes or alerts.
            connection.execute("UPDATE notification_work SET pending=0 WHERE pending=1", [])?;
        }
        Ok(Self { connection })
    }

    pub(super) fn request(
        &mut self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<(ChannelResponse, bool)> {
        let request = match request {
            ChannelRequest::List {
                workspace_id,
                after,
                limit,
            } => {
                let authority = workspace_authority(&workspace_id, &admit)?;
                return Ok((
                    self.list(&workspace_id, after.as_ref(), limit, &authority)?,
                    false,
                ));
            }
            ChannelRequest::Create {
                workspace_id,
                name,
                membership,
                members,
                settings,
            } => {
                let authority = workspace_authority(&workspace_id, &admit)?;
                require_write(&authority)?;
                return self.create(
                    workspace_id,
                    name,
                    membership,
                    members,
                    settings,
                    &authority,
                );
            }
            request => request,
        };
        let channel_id = match &request {
            ChannelRequest::ListMembers { channel_id, .. }
            | ChannelRequest::SetSettings { channel_id, .. }
            | ChannelRequest::SetMembership { channel_id, .. }
            | ChannelRequest::Post { channel_id, .. }
            | ChannelRequest::History { channel_id, .. }
            | ChannelRequest::Read { channel_id, .. }
            | ChannelRequest::MarkRead { channel_id, .. }
            | ChannelRequest::UploadAttachment { channel_id, .. }
            | ChannelRequest::GetAttachment { channel_id, .. } => channel_id,
            _ => unreachable!("workspace and subscription routes are handled separately"),
        };
        let (channel, authority) = self.access(channel_id, &admit)?;
        match request {
            ChannelRequest::ListMembers {
                channel_id,
                after,
                limit,
            } => {
                let members = self.members_if_explicit(&channel_id, &channel)?;
                Ok((
                    list_members(authority, members.as_deref(), after.as_ref(), limit)?,
                    false,
                ))
            }
            ChannelRequest::SetSettings {
                channel_id,
                settings,
            } => {
                require_owner(&authority)?;
                self.set_settings(channel_id, channel, settings, &authority.actor)
            }
            ChannelRequest::SetMembership {
                channel_id,
                membership,
                members,
            } => {
                require_owner(&authority)?;
                self.set_membership(channel_id, channel, membership, members, &authority)
            }
            ChannelRequest::Post { channel_id, post } => {
                require_write(&authority)?;
                self.post(&channel_id, channel, post, authority)
            }
            ChannelRequest::History {
                channel_id,
                before,
                limit,
            } => Ok((
                self.history(
                    &channel_id,
                    before.unwrap_or(channel.last_sequence + 1),
                    limit,
                )?,
                false,
            )),
            ChannelRequest::Read {
                channel_id,
                message_id,
            } => Ok((self.read(&channel_id, &message_id)?, false)),
            ChannelRequest::MarkRead {
                channel_id,
                through,
            } => {
                require_write(&authority)?;
                ensure!(
                    through <= channel.last_sequence,
                    "channel read position is ahead of history"
                );
                self.mark_read(&channel_id, &authority.actor.id, through)
            }
            ChannelRequest::UploadAttachment { channel_id, upload } => {
                require_write(&authority)?;
                Ok((self.upload(&channel_id, upload)?, false))
            }
            ChannelRequest::GetAttachment {
                channel_id,
                attachment_id,
            } => Ok((self.attachment(&channel_id, &attachment_id)?, false)),
            _ => unreachable!("workspace and subscription routes are handled separately"),
        }
    }

    fn access(
        &self,
        id: &ChannelId,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<(StoredChannel, ChannelAuthority)> {
        let channel = self.connection.query_row(
            "SELECT workspace_id,name,workspace_wide,settings,last_sequence,change_sequence,last_attention
             FROM channels WHERE id=?1",
            [id.as_ref()],
            |row| StoredChannel::read(row, 0),
        ).optional()?.context("channel unavailable")?;
        let authority = workspace_authority(&channel.workspace_id, admit)?;
        if authority.actor.kind == ParticipantKind::Agent && !channel.workspace_wide {
            let member: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=?2)",
                params![id.as_ref(), authority.actor.id.as_ref()],
                |r| r.get(0),
            )?;
            ensure!(member, "channel unavailable");
        }
        Ok((channel, authority))
    }

    fn list(
        &self,
        workspace_id: &str,
        after: Option<&ChannelId>,
        limit: u16,
        authority: &ChannelAuthority,
    ) -> Result<ChannelResponse> {
        let mut statement = self.connection.prepare(
            "SELECT c.id,c.workspace_id,c.name,c.workspace_wide,c.settings,c.last_sequence,c.change_sequence,
                    c.last_attention,COALESCE(p.through,0)
             FROM channels c LEFT JOIN positions p ON p.channel_id=c.id AND p.participant_id=?2
             WHERE c.workspace_id=?1 AND c.id>?3 AND
                   (?4 OR c.workspace_wide OR EXISTS (
                    SELECT 1 FROM members m WHERE m.channel_id=c.id AND m.participant_id=?2))
             ORDER BY c.id LIMIT ?5",
        )?;
        let user = authority.actor.kind == ParticipantKind::User;
        let mut channels = statement
            .query_map(
                params![
                    workspace_id,
                    authority.actor.id.as_ref(),
                    after.map_or("", AsRef::as_ref),
                    user,
                    limit + 1
                ],
                |r| Ok(StoredChannel::read(r, 1)?.summary(ChannelId(r.get(0)?), r.get(8)?, user)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_after =
            take_extra(&mut channels, limit).then(|| channels.last().unwrap().id.clone());
        Ok(ChannelResponse::Listed {
            channels,
            next_after,
        })
    }

    fn create(
        &mut self,
        workspace_id: String,
        name: String,
        membership: MembershipMode,
        mut members: Vec<ParticipantId>,
        settings: ChannelSettings,
        authority: &ChannelAuthority,
    ) -> Result<(ChannelResponse, bool)> {
        let workspace_wide = membership == MembershipMode::WorkspaceWide;
        if !workspace_wide
            && authority.actor.kind == ParticipantKind::Agent
            && !members.contains(&authority.actor.id)
        {
            ensure!(
                members.len() < bkpr_protocol::channels::MAX_MEMBERS,
                "explicit channel must have room for its creator"
            );
            members.push(authority.actor.id.clone());
        }
        validate_members(&members, &[], authority)?;
        let id = ChannelId(new_id());
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO channels(id,workspace_id,name,workspace_wide,settings) VALUES(?1,?2,?3,?4,?5)",
            params![id.as_ref(), workspace_id, name, workspace_wide, serde_json::to_string(&settings)?],
        )?;
        insert_members(&tx, &id, &members)?;
        tx.commit()?;
        Ok((
            ChannelResponse::Channel {
                channel: ChannelSummary {
                    id,
                    workspace_id,
                    name,
                    membership,
                    settings,
                    last_sequence: 0,
                    read_through: 0,
                    unread_attention: false,
                },
            },
            false,
        ))
    }

    fn set_settings(
        &mut self,
        id: ChannelId,
        mut channel: StoredChannel,
        settings: ChannelSettings,
        actor: &Participant,
    ) -> Result<(ChannelResponse, bool)> {
        let through = read_position(&self.connection, &id, &actor.id)?;
        let changed = channel.settings != settings;
        if changed {
            let tx = self.connection.transaction()?;
            tx.execute(
                "UPDATE channels SET settings=?2 WHERE id=?1",
                params![id.as_ref(), serde_json::to_string(&settings)?],
            )?;
            add_change(&tx, &id, ChannelChangeKind::Settings, None)?;
            tx.commit()?;
            channel.settings = settings;
        }
        Ok((
            ChannelResponse::Channel {
                channel: channel.summary(id, through, true),
            },
            changed,
        ))
    }

    fn set_membership(
        &mut self,
        id: ChannelId,
        mut channel: StoredChannel,
        membership: MembershipMode,
        mut members: Vec<ParticipantId>,
        authority: &ChannelAuthority,
    ) -> Result<(ChannelResponse, bool)> {
        let previous = self.members_if_explicit(&id, &channel)?.unwrap_or_default();
        members.sort_unstable();
        validate_members(&members, &previous, authority)?;
        let through = read_position(&self.connection, &id, &authority.actor.id)?;
        let workspace_wide = membership == MembershipMode::WorkspaceWide;
        let changed = channel.workspace_wide != workspace_wide || previous != members;
        if changed {
            let tx = self.connection.transaction()?;
            tx.execute(
                "UPDATE channels SET workspace_wide=?2 WHERE id=?1",
                params![id.as_ref(), workspace_wide],
            )?;
            tx.execute("DELETE FROM members WHERE channel_id=?1", [id.as_ref()])?;
            insert_members(&tx, &id, &members)?;
            if !workspace_wide {
                tx.execute(
                    "UPDATE notification_work SET pending=0 WHERE channel_id=?1 AND pending=1 AND launch_id IS NOT NULL
                     AND NOT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=notification_work.participant_id)",
                    [id.as_ref()],
                )?;
            }
            add_change(&tx, &id, ChannelChangeKind::Membership, None)?;
            tx.commit()?;
            channel.workspace_wide = workspace_wide;
        }
        Ok((
            ChannelResponse::Channel {
                channel: channel.summary(id, through, true),
            },
            changed,
        ))
    }

    fn members_if_explicit(
        &self,
        id: &ChannelId,
        channel: &StoredChannel,
    ) -> Result<Option<Vec<ParticipantId>>> {
        if channel.workspace_wide {
            return Ok(None);
        }
        let mut statement = self.connection.prepare(
            "SELECT participant_id FROM members WHERE channel_id=?1 ORDER BY participant_id",
        )?;
        let members = statement
            .query_map([id.as_ref()], |r| Ok(ParticipantId(r.get(0)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(members))
    }

    fn post(
        &mut self,
        id: &ChannelId,
        channel: StoredChannel,
        post: ChannelPost,
        authority: ChannelAuthority,
    ) -> Result<(ChannelResponse, bool)> {
        let fingerprint = fingerprint(&post);
        let retry = self
            .connection
            .query_row(
                "SELECT channel_id,fingerprint,summary,wake_count,attention
             FROM posts WHERE author_id=?1 AND request_key=?2",
                params![authority.actor.id.as_ref(), post.idempotency_key],
                |row| {
                    if row.get_ref(0)?.as_str()? != id.as_ref()
                        || row.get_ref(1)?.as_blob()? != fingerprint
                    {
                        return Ok(None);
                    }
                    Ok(Some(ChannelResponse::Posted {
                        message: json(row, 2)?,
                        wake_count: row.get(3)?,
                        attention: row.get(4)?,
                    }))
                },
            )
            .optional()?;
        if let Some(response) = retry {
            return Ok((
                response.context("channel post conflicts with an existing retry key")?,
                false,
            ));
        }
        let (policy, attention) = match authority.actor.kind {
            ParticipantKind::User => (channel.settings.user_wake, false),
            ParticipantKind::Agent => (
                channel.settings.agent_wake,
                match post.intent {
                    PostIntent::Note => false,
                    PostIntent::Question => channel.settings.attention_questions,
                    PostIntent::Report => channel.settings.attention_reports,
                },
            ),
        };
        let may_wake = !authority.participants.is_empty()
            && policy != WakePolicy::None
            && (policy != WakePolicy::AddressedOnly || !post.mentions.is_empty());
        let explicit_members = if may_wake || !post.mentions.is_empty() {
            self.members_if_explicit(id, &channel)?
        } else {
            None
        };
        let can_mention =
            |participant: &Participant| member(participant, explicit_members.as_deref());
        let mut addressed_agent = false;
        for mention in &post.mentions {
            let target = authority
                .roster()
                .find(|actor| actor.id == *mention && can_mention(actor))
                .context("channel mention no longer names a current member")?;
            addressed_agent |= target.kind == ParticipantKind::Agent;
        }
        let mut wakes = Vec::new();
        if may_wake {
            for entry in &authority.participants {
                let Some(launch) = entry.launch_id.as_deref() else {
                    continue;
                };
                let target = &entry.actor;
                if target.kind != ParticipantKind::Agent
                    || target.id == authority.actor.id
                    || !can_mention(target)
                {
                    continue;
                }
                let addressed = post.mentions.contains(&target.id);
                if match policy {
                    WakePolicy::All => true,
                    WakePolicy::AddressedOrAll => !addressed_agent || addressed,
                    WakePolicy::AddressedOnly => addressed,
                    WakePolicy::None => false,
                } {
                    ensure!(
                        wakes.len() < MAX_PENDING,
                        "channel notification capacity exceeded"
                    );
                    wakes.push((target.id.as_ref(), launch, addressed));
                }
            }
        }
        let sequence = channel.last_sequence + 1;
        let message = MessageSummary {
            id: MessageId(new_id()),
            sequence,
            author: authority.actor,
            posted_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            intent: post.intent,
            mentions: post.mentions,
            reply_to: post.reply_to,
            attachments: post.attachments,
        };
        let tx = self.connection.transaction()?;
        if let Some(reply) = &message.reply_to {
            ensure!(
                exists(
                    &tx,
                    "SELECT EXISTS(SELECT 1 FROM posts WHERE channel_id=?1 AND id=?2)",
                    id,
                    reply.as_ref()
                )?,
                "channel reply target unavailable"
            );
        }
        for attachment in &message.attachments {
            ensure!(
                exists(
                    &tx,
                    "SELECT EXISTS(SELECT 1 FROM attachments WHERE channel_id=?1 AND id=?2)",
                    id,
                    attachment.as_ref()
                )?,
                "channel attachment unavailable"
            );
        }
        let needed = wakes.len() + usize::from(attention);
        if needed != 0 {
            let pending: usize = tx.query_row(
                "SELECT count(*) FROM notification_work WHERE pending=1",
                [],
                |r| r.get(0),
            )?;
            ensure!(
                needed <= MAX_PENDING.saturating_sub(pending),
                "channel notification capacity exceeded"
            );
        }
        let wake_count = u32::try_from(wakes.len())?;
        tx.execute(
            "INSERT INTO posts(id,channel_id,sequence,author_id,request_key,fingerprint,summary,body,wake_count,attention)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![message.id.as_ref(), id.as_ref(), sequence, message.author.id.as_ref(), post.idempotency_key,
                    fingerprint.as_slice(), serde_json::to_string(&message)?, post.text, wake_count, attention],
        )?;
        tx.execute(
            "UPDATE channels SET last_sequence=?2,last_attention=CASE WHEN ?3 THEN ?2 ELSE last_attention END WHERE id=?1",
            params![id.as_ref(), sequence, attention],
        )?;
        if needed != 0 {
            let mut insert = tx.prepare(
                "INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed) VALUES(?1,?2,?3,?4,?5)",
            )?;
            for (recipient, launch, addressed) in wakes {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    recipient,
                    launch,
                    addressed
                ])?;
            }
            if attention {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    "",
                    None::<&str>,
                    false
                ])?;
            }
        }
        add_change(&tx, id, ChannelChangeKind::Post, None)?;
        tx.commit()?;
        Ok((
            ChannelResponse::Posted {
                message,
                wake_count,
                attention,
            },
            true,
        ))
    }

    fn history(&self, id: &ChannelId, before: u64, limit: u16) -> Result<ChannelResponse> {
        let sequence = i64::try_from(before - 1).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(
            "SELECT summary FROM posts WHERE channel_id=?1 AND sequence<=?2 ORDER BY sequence DESC LIMIT ?3",
        )?;
        let mut messages = statement
            .query_map(params![id.as_ref(), sequence, limit + 1], |r| json(r, 0))?
            .collect::<rusqlite::Result<Vec<MessageSummary>>>()?;
        let next_before =
            take_extra(&mut messages, limit).then(|| messages.last().unwrap().sequence);
        messages.reverse();
        Ok(ChannelResponse::History {
            messages,
            next_before,
        })
    }

    fn read(&self, id: &ChannelId, message_id: &MessageId) -> Result<ChannelResponse> {
        self.connection
            .query_row(
                "SELECT summary,body FROM posts WHERE channel_id=?1 AND id=?2",
                params![id.as_ref(), message_id.as_ref()],
                |r| {
                    Ok(ChannelResponse::Message {
                        message: json(r, 0)?,
                        text: r.get(1)?,
                    })
                },
            )
            .optional()?
            .context("channel message unavailable")
    }

    fn mark_read(
        &mut self,
        id: &ChannelId,
        actor: &ParticipantId,
        through: u64,
    ) -> Result<(ChannelResponse, bool)> {
        let previous = read_position(&self.connection, id, actor)?;
        if through <= previous {
            return Ok((ChannelResponse::ReadPosition { through: previous }, false));
        }
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO positions VALUES(?1,?2,?3) ON CONFLICT(channel_id,participant_id)
             DO UPDATE SET through=excluded.through",
            params![id.as_ref(), actor.as_ref(), through],
        )?;
        add_change(&tx, id, ChannelChangeKind::ReadPosition, Some(actor))?;
        tx.commit()?;
        Ok((ChannelResponse::ReadPosition { through }, true))
    }

    pub(super) fn changes(
        &self,
        id: &ChannelId,
        after: Option<u64>,
        limit: u16,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<(u64, Vec<ChannelChange>, bool, bool)> {
        let (channel, authority) = self.access(id, admit)?;
        let after = after.unwrap_or(channel.change_sequence);
        ensure!(
            after <= channel.change_sequence,
            "channel cursor is ahead of the change stream"
        );
        if after == channel.change_sequence {
            return Ok((after, Vec::new(), false, authority.workspace_open));
        }
        let mut statement = self.connection.prepare(
            "SELECT sequence,kind FROM changes WHERE channel_id=?1 AND participant_id IS NULL AND sequence>?3
             UNION ALL
             SELECT sequence,kind FROM changes WHERE channel_id=?1 AND participant_id=?2 AND sequence>?3
             ORDER BY sequence LIMIT ?4",
        )?;
        let mut changes = statement
            .query_map(
                params![id.as_ref(), authority.actor.id.as_ref(), after, limit + 1],
                |r| {
                    Ok(ChannelChange {
                        sequence: r.get(0)?,
                        kind: decode_change(r.get(1)?)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = take_extra(&mut changes, limit);
        let sequence = if more {
            changes.last().unwrap().sequence
        } else {
            channel.change_sequence
        };
        Ok((sequence, changes, more, authority.workspace_open))
    }

    fn upload(&mut self, id: &ChannelId, upload: AttachmentUpload) -> Result<ChannelResponse> {
        let attachment = AttachmentDescriptor {
            id: AttachmentId(new_id()),
            media_type: upload.media_type,
            alt_text: upload.alt_text,
            byte_len: upload.bytes.len().try_into()?,
        };
        self.connection.execute(
            "INSERT INTO attachments VALUES(?1,?2,?3,?4)",
            params![
                attachment.id.as_ref(),
                id.as_ref(),
                serde_json::to_string(&attachment)?,
                upload.bytes
            ],
        )?;
        Ok(ChannelResponse::AttachmentUploaded { attachment })
    }

    fn attachment(&self, id: &ChannelId, attachment_id: &AttachmentId) -> Result<ChannelResponse> {
        self.connection
            .query_row(
                "SELECT descriptor,bytes FROM attachments WHERE channel_id=?1 AND id=?2",
                params![id.as_ref(), attachment_id.as_ref()],
                |r| {
                    Ok(ChannelResponse::Attachment {
                        attachment: json(r, 0)?,
                        bytes: r.get::<_, Vec<u8>>(1)?.into(),
                    })
                },
            )
            .optional()?
            .context("channel attachment unavailable")
    }
}

impl StoredChannel {
    fn read(row: &Row<'_>, start: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: row.get(start)?,
            name: row.get(start + 1)?,
            workspace_wide: row.get(start + 2)?,
            settings: json(row, start + 3)?,
            last_sequence: row.get(start + 4)?,
            change_sequence: row.get(start + 5)?,
            last_attention: row.get(start + 6)?,
        })
    }

    fn summary(self, id: ChannelId, read_through: u64, user: bool) -> ChannelSummary {
        ChannelSummary {
            id,
            workspace_id: self.workspace_id,
            name: self.name,
            membership: if self.workspace_wide {
                MembershipMode::WorkspaceWide
            } else {
                MembershipMode::Explicit
            },
            settings: self.settings,
            last_sequence: self.last_sequence,
            read_through,
            unread_attention: user && self.last_attention > read_through,
        }
    }
}

impl ChannelAuthority {
    fn roster(&self) -> impl Iterator<Item = &Participant> {
        std::iter::once(&self.actor).chain(
            self.participants
                .iter()
                .map(|entry| &entry.actor)
                .filter(|actor| actor.id != self.actor.id),
        )
    }
}

fn workspace_authority(
    workspace: &str,
    admit: &impl Fn(&str) -> Result<ChannelAuthority>,
) -> Result<ChannelAuthority> {
    let authority = admit(workspace)?;
    authority.actor.validate()?;
    ensure!(
        authority.workspace_open || authority.actor.kind == ParticipantKind::User,
        "channel workspace unavailable"
    );
    Ok(authority)
}

fn require_write(authority: &ChannelAuthority) -> Result<()> {
    ensure!(authority.workspace_open, "channel workspace is closed");
    ensure!(authority.writable, "channel authority is read-only");
    Ok(())
}

fn require_owner(authority: &ChannelAuthority) -> Result<()> {
    require_write(authority)?;
    ensure!(
        authority.actor.kind == ParticipantKind::User,
        "channel management requires the session user"
    );
    Ok(())
}

fn member(participant: &Participant, members: Option<&[ParticipantId]>) -> bool {
    participant.kind == ParticipantKind::User
        || members.is_none_or(|ids| ids.binary_search(&participant.id).is_ok())
}

fn validate_members(
    members: &[ParticipantId],
    previous: &[ParticipantId],
    authority: &ChannelAuthority,
) -> Result<()> {
    for id in members {
        ensure!(
            previous.binary_search(id).is_ok() || authority.roster().any(|p| p.id == *id),
            "channel member is not authorized in this workspace"
        );
    }
    Ok(())
}

fn list_members(
    authority: ChannelAuthority,
    members: Option<&[ParticipantId]>,
    after: Option<&ParticipantId>,
    limit: u16,
) -> Result<ChannelResponse> {
    let ChannelAuthority {
        actor,
        participants: mut roster,
        workspace_open,
        ..
    } = authority;
    if !workspace_open {
        return Ok(ChannelResponse::Members {
            participants: Vec::new(),
            next_after: None,
        });
    }
    let visible = |participant: &Participant| {
        member(participant, members) && after.is_none_or(|id| participant.id > *id)
    };
    roster.retain(|entry| entry.actor.id != actor.id && visible(&entry.actor));
    if visible(&actor) {
        roster.push(super::WorkspaceParticipant {
            actor,
            launch_id: None,
        });
    }
    let count = usize::from(limit) + 1;
    if roster.len() > count {
        roster.select_nth_unstable_by(count, |left, right| left.actor.id.cmp(&right.actor.id));
        roster.truncate(count);
    }
    roster.sort_unstable_by(|left, right| left.actor.id.cmp(&right.actor.id));
    let next_after =
        take_extra(&mut roster, limit).then(|| roster.last().unwrap().actor.id.clone());
    for entry in &roster {
        entry.actor.validate()?;
    }
    Ok(ChannelResponse::Members {
        participants: roster.into_iter().map(|entry| entry.actor).collect(),
        next_after,
    })
}

fn open_private(path: &Path, create: bool) -> Result<Option<File>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "unsafe channel storage file"
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => {
            crate::registry::check_file(&file)?;
            ensure!(file.metadata()?.nlink() == 1, "unsafe channel storage file");
            Ok(Some(file))
        }
        Err(error) if !create && error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn insert_members(tx: &Transaction<'_>, id: &ChannelId, members: &[ParticipantId]) -> Result<()> {
    if !members.is_empty() {
        let mut statement = tx.prepare("INSERT INTO members VALUES(?1,?2)")?;
        for member in members {
            statement.execute(params![id.as_ref(), member.as_ref()])?;
        }
    }
    Ok(())
}

fn add_change(
    tx: &Transaction<'_>,
    id: &ChannelId,
    kind: ChannelChangeKind,
    actor: Option<&ParticipantId>,
) -> Result<()> {
    let sequence: u64 = tx.query_row(
        "UPDATE channels SET change_sequence=change_sequence+1 WHERE id=?1 RETURNING change_sequence",
        [id.as_ref()], |r| r.get(0),
    )?;
    let kind = match kind {
        ChannelChangeKind::Post => 0,
        ChannelChangeKind::Settings => 1,
        ChannelChangeKind::Membership => 2,
        ChannelChangeKind::ReadPosition => 3,
    };
    tx.execute(
        "INSERT INTO changes VALUES(?1,?2,?3,?4)",
        params![id.as_ref(), sequence, kind, actor.map(AsRef::as_ref)],
    )?;
    Ok(())
}

fn decode_change(kind: i64) -> rusqlite::Result<ChannelChangeKind> {
    match kind {
        0 => Ok(ChannelChangeKind::Post),
        1 => Ok(ChannelChangeKind::Settings),
        2 => Ok(ChannelChangeKind::Membership),
        3 => Ok(ChannelChangeKind::ReadPosition),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, kind)),
    }
}

fn read_position(connection: &Connection, id: &ChannelId, actor: &ParticipantId) -> Result<u64> {
    Ok(connection
        .query_row(
            "SELECT through FROM positions WHERE channel_id=?1 AND participant_id=?2",
            params![id.as_ref(), actor.as_ref()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn exists(connection: &Connection, sql: &str, id: &ChannelId, target: &str) -> Result<bool> {
    Ok(connection
        .prepare(sql)?
        .query_row(params![id.as_ref(), target], |r| r.get(0))?)
}

fn json<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    serde_json::from_str(row.get_ref(index)?.as_str()?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn take_extra<T>(items: &mut Vec<T>, limit: u16) -> bool {
    if items.len() > usize::from(limit) {
        items.pop();
        true
    } else {
        false
    }
}

fn fingerprint(post: &ChannelPost) -> [u8; 32] {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.update(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, post).expect("typed post serialization is infallible");
    writer.0.finalize().into()
}

fn new_id() -> String {
    Uuid::new_v4().hyphenated().to_string()
}
