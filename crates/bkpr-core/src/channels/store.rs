use super::{ChannelAccessLost, ChannelAuthority, ChannelRoutes, WakeTarget};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::channels::{
    AttachmentDescriptor, AttachmentId, AttachmentUpload, ChannelChange, ChannelChangeKind,
    ChannelCursor, ChannelId, ChannelMessage, ChannelPost, ChannelRequest, ChannelResponse,
    ChannelSettings, ChannelSummary, MAX_HISTORY_BYTES, MembershipMode, MessageId, MessageSummary,
    Participant, ParticipantId, ParticipantKind, PostIntent, WakePolicy,
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

#[path = "wakes.rs"]
mod wakes;

const APPLICATION_ID: i64 = 0x424b4348;
// Versions 1–6 predate staging. Version 7 contains staging but no wakes.
const STORE_VERSION: i64 = 8;
const STORE_NAME: &str = "channels.sqlite3";
const MAX_PENDING: usize = 4096;
const READ_POST: &str = "SELECT summary,body FROM posts WHERE channel_id=?1 AND id=?2";

pub(super) const MAX_ARCHIVE_CANDIDATES: usize = 128;
pub(super) const MAX_STAGED_BYTES: u64 = 64 * 1024 * 1024;
const STAGING_TTL_MS: u64 = 24 * 60 * 60 * 1000;
pub(super) const MAX_STAGING_RECLAIM: usize = 128;
const PRESETS: [(&str, ChannelSettings); 2] = [
    (
        "#desk",
        ChannelSettings {
            user_wake: WakePolicy::AddressedOrAll,
            agent_wake: WakePolicy::None,
            attention_questions: true,
            attention_reports: true,
        },
    ),
    (
        "#team",
        ChannelSettings {
            user_wake: WakePolicy::All,
            agent_wake: WakePolicy::All,
            attention_questions: false,
            attention_reports: false,
        },
    ),
];

// Shared by fresh initialization and the one-time staging migration.
const ATTACHMENT_STAGING_SCHEMA: &str = "
    CREATE TABLE staged_attachments (
        id TEXT PRIMARY KEY REFERENCES attachments(id) ON DELETE CASCADE,
        expires_ms INTEGER NOT NULL CHECK(expires_ms >= 0),
        byte_len INTEGER NOT NULL CHECK(byte_len > 0)
    ) WITHOUT ROWID, STRICT;
    CREATE INDEX staged_attachment_expiry ON staged_attachments(expires_ms,id);
    CREATE TABLE attachment_budget (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        unposted_bytes INTEGER NOT NULL CHECK(unposted_bytes >= 0)
    ) STRICT;
    INSERT INTO attachment_budget VALUES(1,0);";

pub(crate) struct Store {
    pub(super) connection: Connection,
    pub(super) retirement_epoch: u64,
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

#[derive(serde::Serialize)]
struct MessagePage<'a> {
    message: MessageSummary,
    text: &'a str,
    body_bytes: usize,
    next_offset: Option<usize>,
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
                || (application == APPLICATION_ID && matches!(version, 1..=3 | 7..=STORE_VERSION)),
            "foreign or unsupported channel store"
        );
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF;
             PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=EXTRA;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=0;
             PRAGMA cache_size=-1024;",
        )?;
        if empty {
            let tx = connection.transaction()?;
            tx.execute_batch(include_str!("schema.sql"))?;
            tx.execute_batch(include_str!("wakes.sql"))?;
            tx.execute_batch(ATTACHMENT_STAGING_SCHEMA)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", STORE_VERSION)?;
            tx.commit()?;
            directory.sync_all()?;
        } else {
            if version < STORE_VERSION {
                let tx = connection.transaction()?;
                if version == 1 {
                    tx.execute_batch(
                        "ALTER TABLE members ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '';
                         UPDATE members SET workspace_id=(SELECT workspace_id FROM channels WHERE id=members.channel_id);
                         CREATE INDEX workspace_wide_channels ON channels(workspace_id,id) WHERE workspace_wide=1;
                         CREATE INDEX visible_explicit_channels ON members(workspace_id,participant_id,channel_id);",
                    )?;
                }
                if matches!(version, 1 | 2 | 7) {
                    tx.execute_batch(include_str!("wakes.sql"))?;
                }
                if version < 7 {
                    migrate_attachment_staging(&tx)?;
                }
                tx.pragma_update(None, "user_version", STORE_VERSION)?;
                tx.commit()?;
            }
            // A new runtime may inspect history, but never adopts another runtime's work.
            connection.execute_batch(
                "UPDATE notification_work SET pending=0 WHERE pending=1;
                 UPDATE channel_batches SET outcome='unknown' WHERE outcome IS NULL;",
            )?;
        }
        Ok(Self {
            connection,
            retirement_epoch: 0,
        })
    }

    pub(super) fn retire_closed_workspaces(
        &mut self,
        workspace_open: &impl Fn(&str) -> bool,
    ) -> Result<()> {
        let mut closed = Vec::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT c.workspace_id FROM notification_work w
                 JOIN channels c ON c.id=w.channel_id WHERE w.pending=1
                 UNION SELECT c.workspace_id FROM channel_batches b
                 JOIN channels c ON c.id=b.channel_id WHERE b.outcome IS NULL",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let workspace = row.get_ref(0)?.as_str()?;
                if !workspace_open(workspace) {
                    closed.push(workspace.to_owned());
                }
            }
        }
        if closed.is_empty() {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        {
            let mut statement = tx.prepare(
                "UPDATE notification_work SET pending=0 WHERE pending=1 AND channel_id IN
                    (SELECT id FROM channels WHERE workspace_id=?1)",
            )?;
            for id in &closed {
                statement.execute([id])?;
            }
            let mut batches = tx.prepare(
                "UPDATE channel_batches SET outcome='revoked' WHERE outcome IS NULL AND channel_id IN
                    (SELECT id FROM channels WHERE workspace_id=?1)",
            )?;
            for id in &closed {
                batches.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn retire_inactive_launches(&mut self, active: &dyn Fn(&str) -> bool) -> Result<()> {
        let mut inactive = Vec::new();
        let mut launches = std::collections::BTreeMap::<String, bool>::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT launch_id,message_id,participant_id FROM notification_work
                 WHERE pending=1 AND launch_id IS NOT NULL",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let launch = row.get_ref(0)?.as_str()?;
                let live = match launches.get(launch) {
                    Some(live) => *live,
                    None => {
                        let live = active(launch);
                        launches.insert(launch.to_owned(), live);
                        live
                    }
                };
                if !live {
                    inactive.push((row.get::<_, String>(1)?, row.get::<_, String>(2)?));
                }
            }
        }
        if !inactive.is_empty() {
            let tx = self.connection.transaction()?;
            {
                let mut update = tx.prepare(
                    "UPDATE notification_work SET pending=0 WHERE message_id=?1 AND participant_id=?2",
                )?;
                for (message, participant) in inactive {
                    update.execute([message, participant])?;
                }
            }
            tx.commit()?;
        }
        Ok(())
    }

    pub(super) fn provision<'a>(
        &mut self,
        workspaces: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        {
            let mut exists = tx.prepare(
                "SELECT EXISTS(SELECT 1 FROM channels WHERE workspace_id=?1 AND name=?2)",
            )?;
            for workspace in workspaces {
                for (name, settings) in PRESETS {
                    if !exists.query_row([workspace, name], |row| row.get::<_, bool>(0))? {
                        insert_channel(&tx, &ChannelId(new_id()), workspace, name, true, settings)?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn authorize_upload(
        &self,
        id: &ChannelId,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<()> {
        let (_, authority) = self.access(id, admit)?;
        require_write(&authority)
    }

    pub(super) fn request(
        &mut self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        routes: ChannelRoutes<'_>,
        retire: impl FnOnce(&mut Self, &str) -> Result<bool>,
    ) -> Result<(ChannelResponse, bool)> {
        let mut selector = None;
        let request = match request {
            ChannelRequest::List {
                workspace_id,
                after,
                limit,
            } => {
                let authority = workspace_authority(&workspace_id, &admit)?;
                if authority.workspace_open {
                    self.provision([workspace_id.as_str()])?;
                } else {
                    let retained: bool = self.connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM channels WHERE workspace_id=?1)",
                        [&workspace_id],
                        |row| row.get(0),
                    )?;
                    ensure!(retained, ChannelAccessLost);
                }
                return Ok((
                    self.list(&workspace_id, after.as_ref(), limit, &authority)?,
                    false,
                ));
            }
            ChannelRequest::ListArchivedWorkspaces { after, limit } => {
                return Ok((self.archives(after.as_deref(), limit, &admit)?, false));
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
                self.provision([workspace_id.as_str()])?;
                return self.create(
                    workspace_id,
                    name,
                    membership,
                    members,
                    settings,
                    &authority,
                );
            }
            ChannelRequest::Post { channel_id, post } => {
                let channel_id =
                    match self.resolve_builtin(&channel_id, routes.workspace, &admit)? {
                        Some(id) => {
                            selector = Some(channel_id);
                            id
                        }
                        None => channel_id,
                    };
                ChannelRequest::Post { channel_id, post }
            }
            request => request,
        };
        let channel_id = match &request {
            ChannelRequest::ListMembers { channel_id, .. }
            | ChannelRequest::ListExplicitMembers { channel_id, .. }
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
            ChannelRequest::ListExplicitMembers {
                channel_id,
                after,
                limit,
            } => Ok((
                self.explicit_members(&channel_id, after.as_ref(), limit)?,
                false,
            )),
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
            ChannelRequest::Post { channel_id, post } => self.post(
                channel,
                post,
                authority,
                retire,
                routes,
                (&channel_id, selector.as_ref()),
            ),
            ChannelRequest::History {
                channel_id,
                before,
                after,
                limit,
            } => Ok((
                self.history(&channel_id, &channel, before, after, limit)?,
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

    fn resolve_builtin(
        &mut self,
        selector: &ChannelId,
        workspace: Option<&str>,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<Option<ChannelId>> {
        let Some(workspace) = workspace.filter(|_| matches!(selector.as_ref(), "#desk" | "#team"))
        else {
            return Ok(None);
        };
        let lookup = |connection: &Connection| {
            connection
                .query_row(
                    "SELECT id FROM channels WHERE workspace_id=?1 AND name=?2",
                    params![workspace, selector.as_ref()],
                    |row| Ok(ChannelId(row.get(0)?)),
                )
                .optional()
        };
        if let Some(id) = lookup(&self.connection)? {
            return Ok(Some(id));
        }
        workspace_authority(workspace, admit)?;
        self.provision([workspace])?;
        Ok(Some(lookup(&self.connection)?.context(ChannelAccessLost)?))
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
        ).optional()?.context(ChannelAccessLost)?;
        let authority = workspace_authority(&channel.workspace_id, admit)?;
        if authority.actor.kind == ParticipantKind::Agent && !channel.workspace_wide {
            let member: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=?2)",
                params![id.as_ref(), authority.actor.id.as_ref()],
                |r| r.get(0),
            )?;
            ensure!(member, ChannelAccessLost);
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
            "WITH visible(id) AS (
                 SELECT id FROM channels WHERE ?4 AND workspace_id=?1 AND id>?3
                 UNION ALL
                 SELECT id FROM channels WHERE NOT ?4 AND workspace_id=?1 AND workspace_wide=1 AND id>?3
                 UNION ALL
                 SELECT channel_id FROM members
                   WHERE NOT ?4 AND workspace_id=?1 AND participant_id=?2 AND channel_id>?3
                 ORDER BY 1 LIMIT ?5
             )
             SELECT c.id,c.workspace_id,c.name,c.workspace_wide,c.settings,c.last_sequence,c.change_sequence,
                    c.last_attention,COALESCE(p.through,0)
             FROM visible v JOIN channels c ON c.id=v.id
               LEFT JOIN positions p ON p.channel_id=c.id AND p.participant_id=?2
             ORDER BY v.id",
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

    fn archives(
        &self,
        after: Option<&str>,
        limit: u16,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<ChannelResponse> {
        // Seek past every workspace on the index rather than scanning its duplicate
        // channel rows. A lookahead checks only for another unexamined candidate.
        let mut statement = self.connection.prepare(
            "SELECT workspace_id FROM channels WHERE workspace_id>?1 ORDER BY workspace_id LIMIT 1",
        )?;
        let mut cursor = after.unwrap_or("").to_owned();
        let mut workspace_ids = Vec::new();
        for _ in 0..MAX_ARCHIVE_CANDIDATES {
            let mut rows = statement.query([cursor.as_str()])?;
            let Some(row) = rows.next()? else {
                return Ok(ChannelResponse::ArchivedWorkspaces {
                    workspace_ids,
                    next_after: None,
                });
            };
            let id = row.get_ref(0)?.as_str()?;
            let authority = admit(id)?;
            authority.actor.validate()?;
            if authority.actor.kind != ParticipantKind::User {
                return Ok(ChannelResponse::ArchivedWorkspaces {
                    workspace_ids: Vec::new(),
                    next_after: None,
                });
            }
            cursor.clear();
            cursor.push_str(id);
            if !authority.workspace_open {
                workspace_ids.push(id.to_owned());
                if workspace_ids.len() == usize::from(limit) {
                    break;
                }
            }
        }
        let next_after = statement.exists([cursor.as_str()])?.then_some(cursor);
        Ok(ChannelResponse::ArchivedWorkspaces {
            workspace_ids,
            next_after,
        })
    }

    fn explicit_members(
        &self,
        id: &ChannelId,
        after: Option<&ParticipantId>,
        limit: u16,
    ) -> Result<ChannelResponse> {
        let mut statement = self.connection.prepare(
            "SELECT participant_id FROM members WHERE channel_id=?1 AND participant_id>?2
             ORDER BY participant_id LIMIT ?3",
        )?;
        let mut members = statement
            .query_map(
                params![id.as_ref(), after.map_or("", AsRef::as_ref), limit + 1],
                |row| Ok(ParticipantId(row.get(0)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_after = take_extra(&mut members, limit).then(|| members.last().unwrap().clone());
        Ok(ChannelResponse::ExplicitMembers {
            members,
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
        insert_channel(&tx, &id, &workspace_id, &name, workspace_wide, settings)?;
        insert_members(&tx, &id, &workspace_id, &members)?;
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
            insert_members(&tx, &id, &channel.workspace_id, &members)?;
            if !workspace_wide {
                tx.execute(
                    "UPDATE notification_work SET pending=0 WHERE channel_id=?1 AND pending=1 AND launch_id IS NOT NULL
                     AND NOT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=notification_work.participant_id)",
                    [id.as_ref()],
                )?;
                tx.execute(
                    "UPDATE channel_batches SET outcome='revoked' WHERE channel_id=?1 AND outcome IS NULL
                     AND NOT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=channel_batches.participant_id)",
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
        channel: StoredChannel,
        mut post: ChannelPost,
        authority: ChannelAuthority,
        retire: impl FnOnce(&mut Self, &str) -> Result<bool>,
        routes: ChannelRoutes<'_>,
        (id, selector): (&ChannelId, Option<&ChannelId>),
    ) -> Result<(ChannelResponse, bool)> {
        let fingerprint = fingerprint_for(&post, selector);
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
        require_write(&authority)?;
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
        for mention in &mut post.mentions {
            let target = if let Some(caller) = routes.exact_mentions {
                exact_mention(&authority, mention, caller)
            } else {
                authority.roster().find(|actor| actor.id == *mention)
            }
            .filter(|actor| can_mention(actor))
            .context("channel mention no longer names a current member; refresh the roster")?;
            addressed_agent |= target.kind == ParticipantKind::Agent;
            if *mention != target.id {
                mention.clone_from(&target.id);
            }
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
                    wakes.push(WakeTarget {
                        launch,
                        binding: entry.binding_revision,
                        participant: target.id.as_ref(),
                        addressed,
                    });
                }
            }
        }
        let sequence = channel.last_sequence + 1;
        let message = MessageSummary {
            id: MessageId(new_id()),
            sequence,
            author: authority.actor,
            posted_at_ms: unix_time_ms()?,
            intent: post.intent,
            mentions: post.mentions,
            reply_to: post.reply_to,
            attachments: post.attachments,
            source: post.source,
        };
        let needed = wakes.len() + usize::from(attention);
        let pending_work = needed != 0 && !retire(self, &channel.workspace_id)?;
        if pending_work {
            self.reclaim_wake_capacity(needed, routes.binding_live)?;
        }
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
        if !message.attachments.is_empty() {
            promote_attachments(&tx, id, &message.attachments, message.posted_at_ms)?;
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
                "INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed,pending,binding_revision)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for wake in &wakes {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    wake.participant,
                    wake.launch,
                    wake.addressed,
                    pending_work,
                    wake.binding,
                ])?;
            }
            if attention {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    "",
                    None::<&str>,
                    false,
                    pending_work,
                    0,
                ])?;
            }
        }
        add_change(&tx, id, ChannelChangeKind::Post, None)?;
        tx.commit()?;
        if pending_work && !wakes.is_empty() {
            (routes.publish)(&wakes);
        }
        Ok((
            ChannelResponse::Posted {
                message,
                wake_count,
                attention,
            },
            true,
        ))
    }

    pub(crate) fn compact_members(
        &mut self,
        request: ChannelRequest,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        caller: (&str, u64),
        workspace: Option<&str>,
    ) -> Result<serde_json::Value> {
        let ChannelRequest::ListMembers {
            channel_id,
            after,
            limit,
        } = request
        else {
            unreachable!("compact roster requires a members request");
        };
        let channel_id = self
            .resolve_builtin(&channel_id, workspace, admit)?
            .unwrap_or(channel_id);
        let (channel, authority) = self.access(&channel_id, admit)?;
        let members = self.members_if_explicit(&channel_id, &channel)?;
        let (entries, next_after) =
            members_page(authority, members.as_deref(), after.as_ref(), limit)?;
        let participants: Vec<_> = entries.into_iter().map(|entry| {
            let launch = (entry.actor.kind == ParticipantKind::Agent).then(|| {
                entry.launch_id.as_deref().map_or(caller, |id| (id, entry.binding_revision))
            });
            let mention_id = launch.map_or_else(|| entry.actor.id.0.clone(), |(id, generation)| {
                format!("{id}:{generation}")
            });
            let mut member = serde_json::json!({
                "id":entry.actor.id, "kind":entry.actor.kind, "display_name":entry.actor.display_name,
                "mention_id":mention_id,
            });
            if let Some((id, _)) = launch {
                member["launch_id"] = serde_json::Value::String(id.to_owned());
            }
            member
        }).collect();
        Ok(
            serde_json::json!({"kind":"members", "participants":participants, "next_after":next_after}),
        )
    }

    pub(crate) fn compact_history(
        &mut self,
        request: ChannelRequest,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        workspace: Option<&str>,
    ) -> Result<serde_json::Value> {
        let ChannelRequest::History {
            channel_id,
            before,
            after,
            limit,
        } = request
        else {
            unreachable!("compact history requires a history request");
        };
        let channel_id = self
            .resolve_builtin(&channel_id, workspace, admit)?
            .unwrap_or(channel_id);
        let (channel, _) = self.access(&channel_id, admit)?;
        let bound = history_bound(&channel, before, after);
        let sql = if after.is_some() {
            "SELECT summary FROM posts WHERE channel_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"
        } else {
            "SELECT summary FROM posts WHERE channel_id=?1 AND sequence<=?2 ORDER BY sequence DESC LIMIT ?3"
        };
        let mut statement = self.connection.prepare(sql)?;
        let mut messages = statement
            .query_map(params![channel_id.as_ref(), bound, limit], |row| {
                json::<MessageSummary>(row, 0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if after.is_none() {
            messages.reverse();
        }
        Ok(serde_json::json!({
            "kind": "history",
            "messages": messages,
            "next_before": messages.first().map(|post| post.sequence).filter(|seq| *seq > 1),
            "next_after": messages.last().map(|post| post.sequence).filter(|seq| *seq < channel.last_sequence),
            "cursor": ChannelCursor { channel_id, sequence: channel.change_sequence },
        }))
    }

    fn history(
        &self,
        id: &ChannelId,
        channel: &StoredChannel,
        before: Option<u64>,
        after: Option<u64>,
        limit: u16,
    ) -> Result<ChannelResponse> {
        let sql = if after.is_some() {
            "SELECT summary,body FROM posts WHERE channel_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"
        } else {
            "SELECT summary,body FROM posts WHERE channel_id=?1 AND sequence<=?2 ORDER BY sequence DESC LIMIT ?3"
        };
        let bound = history_bound(channel, before, after);
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(params![id.as_ref(), bound, limit])?;
        let mut messages = Vec::new();
        let mut content_bytes = 0;
        while let Some(row) = rows.next()? {
            let message = self.message(id, row)?;
            content_bytes += message.content_bytes();
            ensure!(
                !messages.is_empty() || content_bytes <= MAX_HISTORY_BYTES,
                "channel message exceeds the history page budget"
            );
            if content_bytes > MAX_HISTORY_BYTES {
                break;
            }
            messages.push(message);
        }
        if after.is_none() {
            messages.reverse();
        }
        let next_before = messages
            .first()
            .map(|entry| entry.message.sequence)
            .filter(|seq| *seq > 1);
        let next_after = messages
            .last()
            .map(|entry| entry.message.sequence)
            .filter(|seq| *seq < channel.last_sequence);
        Ok(ChannelResponse::History {
            messages,
            next_before,
            next_after,
            cursor: ChannelCursor {
                channel_id: id.clone(),
                sequence: channel.change_sequence,
            },
        })
    }

    fn read(&self, id: &ChannelId, message_id: &MessageId) -> Result<ChannelResponse> {
        let mut statement = self.connection.prepare(READ_POST)?;
        let mut rows = statement.query(params![id.as_ref(), message_id.as_ref()])?;
        let message = self.message(id, rows.next()?.context("channel message unavailable")?)?;
        Ok(ChannelResponse::Message { message })
    }

    pub(crate) fn compact_message(
        &mut self,
        request: ChannelRequest,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        offset: usize,
        maximum: usize,
        workspace: Option<&str>,
    ) -> Result<serde_json::Value> {
        let ChannelRequest::Read {
            channel_id,
            message_id,
        } = request
        else {
            unreachable!("compact message requires a read request");
        };
        let channel_id = self
            .resolve_builtin(&channel_id, workspace, admit)?
            .unwrap_or(channel_id);
        self.access(&channel_id, admit)?;
        self.with_message_page(&channel_id, &message_id, offset, maximum, |page| {
            #[derive(serde::Serialize)]
            struct Reply<'a> {
                kind: &'static str,
                #[serde(flatten)]
                page: MessagePage<'a>,
                attachments: Vec<AttachmentDescriptor>,
            }
            let attachments = self.message_attachments(&channel_id, &page.message)?;
            Ok(serde_json::to_value(Reply {
                kind: "message_page",
                page,
                attachments,
            })?)
        })
    }

    fn with_message_page<T>(
        &self,
        channel_id: &ChannelId,
        message_id: &MessageId,
        offset: usize,
        maximum: usize,
        project: impl FnOnce(MessagePage<'_>) -> Result<T>,
    ) -> Result<T> {
        self.optional_message_page(channel_id, message_id, offset, maximum, project)?
            .context("channel message unavailable")
    }

    fn optional_message_page<T>(
        &self,
        channel_id: &ChannelId,
        message_id: &MessageId,
        offset: usize,
        maximum: usize,
        project: impl FnOnce(MessagePage<'_>) -> Result<T>,
    ) -> Result<Option<T>> {
        let mut statement = self.connection.prepare(READ_POST)?;
        let mut rows = statement.query(params![channel_id.as_ref(), message_id.as_ref()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let text = row.get_ref(1)?.as_str()?;
        ensure!(text.is_char_boundary(offset), "invalid channel body offset");
        let mut end = offset.saturating_add(maximum).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        project(MessagePage {
            message: json(row, 0)?,
            text: &text[offset..end],
            body_bytes: text.len(),
            next_offset: (end < text.len()).then_some(end),
        })
        .map(Some)
    }

    fn message(&self, id: &ChannelId, row: &Row<'_>) -> Result<ChannelMessage> {
        let message: MessageSummary = json(row, 0)?;
        let attachments = self.message_attachments(id, &message)?;
        Ok(ChannelMessage {
            message,
            text: row.get(1)?,
            attachments,
        })
    }

    fn message_attachments(
        &self,
        id: &ChannelId,
        message: &MessageSummary,
    ) -> Result<Vec<AttachmentDescriptor>> {
        let mut attachments = Vec::with_capacity(message.attachments.len());
        if !message.attachments.is_empty() {
            let mut statement = self
                .connection
                .prepare("SELECT descriptor FROM attachments WHERE channel_id=?1 AND id=?2")?;
            for attachment in &message.attachments {
                attachments.push(
                    statement.query_row(params![id.as_ref(), attachment.as_ref()], |row| {
                        json(row, 0)
                    })?,
                );
            }
        }
        Ok(attachments)
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
        let now = unix_time_ms()?;
        let attachment = AttachmentDescriptor {
            id: AttachmentId(new_id()),
            media_type: upload.media_type,
            alt_text: upload.alt_text,
            byte_len: upload.bytes.len().try_into()?,
        };
        let tx = self.connection.transaction()?;
        let staged: u64 = tx.query_row(
            "SELECT unposted_bytes FROM attachment_budget WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let reclaimed = reclaim_staged_attachments(&tx, now, u64::MAX)?;
        let remaining = staged
            .checked_sub(reclaimed)
            .context("inconsistent channel attachment budget")?;
        let total = remaining
            .checked_add(u64::from(attachment.byte_len))
            .context("inconsistent channel attachment budget")?;
        if total > MAX_STAGED_BYTES {
            // Leave progress durable even if a bounded sweep could not free enough space.
            if reclaimed != 0 {
                set_staged_bytes(&tx, remaining)?;
                tx.commit()?;
            }
            bail!("channel unattached upload capacity exceeded");
        }
        tx.execute(
            "INSERT INTO attachments VALUES(?1,?2,?3,?4)",
            params![
                attachment.id.as_ref(),
                id.as_ref(),
                serde_json::to_string(&attachment)?,
                upload.bytes
            ],
        )?;
        tx.execute(
            "INSERT INTO staged_attachments(id,expires_ms,byte_len) VALUES(?1,?2,?3)",
            params![
                attachment.id.as_ref(),
                staging_expiry(now)?,
                attachment.byte_len
            ],
        )?;
        set_staged_bytes(&tx, total)?;
        tx.commit()?;
        Ok(ChannelResponse::AttachmentUploaded { attachment })
    }

    fn attachment(&self, id: &ChannelId, attachment_id: &AttachmentId) -> Result<ChannelResponse> {
        self.connection
            .query_row(
                "SELECT a.descriptor,a.bytes FROM attachments a
                 LEFT JOIN staged_attachments s ON s.id=a.id
                 WHERE a.channel_id=?1 AND a.id=?2 AND (s.expires_ms IS NULL OR s.expires_ms>?3)",
                params![id.as_ref(), attachment_id.as_ref(), unix_time_ms()?],
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

fn unix_time_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

fn staging_expiry(now: u64) -> Result<u64> {
    now.checked_add(STAGING_TTL_MS)
        .context("channel attachment expiry overflow")
}

fn set_staged_bytes(tx: &Transaction<'_>, bytes: u64) -> Result<()> {
    tx.execute(
        "UPDATE attachment_budget SET unposted_bytes=?1 WHERE singleton=1",
        [bytes],
    )?;
    Ok(())
}

fn reclaim_staged_attachments(tx: &Transaction<'_>, through: u64, target: u64) -> Result<u64> {
    // Snapshot a bounded batch before deleting from the expiry index via the FK cascade.
    let mut due = tx.prepare(
        "SELECT id,byte_len FROM staged_attachments WHERE expires_ms<=?1
         ORDER BY expires_ms,id LIMIT ?2",
    )?;
    let expired = due
        .query_map(params![through, MAX_STAGING_RECLAIM], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if expired.is_empty() {
        return Ok(0);
    }
    let mut delete = tx.prepare("DELETE FROM attachments WHERE id=?1")?;
    let mut bytes = 0;
    for (id, byte_len) in expired {
        delete.execute([id])?;
        bytes += byte_len;
        if bytes >= target {
            break;
        }
    }
    Ok(bytes)
}

fn promote_attachments(
    tx: &Transaction<'_>,
    channel: &ChannelId,
    ids: &[AttachmentId],
    now: u64,
) -> Result<()> {
    let mut lookup = tx.prepare(
        "SELECT s.byte_len FROM attachments a LEFT JOIN staged_attachments s ON s.id=a.id
         WHERE a.channel_id=?1 AND a.id=?2 AND (s.expires_ms IS NULL OR s.expires_ms>?3)",
    )?;
    let mut promote = tx.prepare("DELETE FROM staged_attachments WHERE id=?1")?;
    let mut credited = 0;
    for id in ids {
        let bytes: Option<u64> = lookup
            .query_row(params![channel.as_ref(), id.as_ref(), now], |row| {
                row.get(0)
            })
            .optional()?
            .context("channel attachment unavailable")?;
        if let Some(bytes) = bytes {
            promote.execute([id.as_ref()])?;
            credited += bytes;
        }
    }
    if credited != 0 {
        tx.execute(
            "UPDATE attachment_budget SET unposted_bytes=unposted_bytes-?1 WHERE singleton=1",
            [credited],
        )?;
    }
    Ok(())
}

fn migrate_attachment_staging(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(ATTACHMENT_STAGING_SCHEMA)?;
    let expiry = staging_expiry(unix_time_ms()?)?;
    let migrated = tx.execute(
        "INSERT INTO staged_attachments(id,expires_ms,byte_len)
         SELECT id,?1,length(bytes) FROM attachments",
        [expiry],
    )?;
    if migrated == 0 {
        return Ok(());
    }
    {
        let mut refs =
            tx.prepare("SELECT a.value FROM posts p, json_each(p.summary,'$.attachments') a")?;
        let mut rows = refs.query([])?;
        let mut retain = tx.prepare("DELETE FROM staged_attachments WHERE id=?1")?;
        while let Some(row) = rows.next()? {
            retain.execute([row.get_ref(0)?.as_str()?])?;
        }
    }
    let mut staged: u64 = tx.query_row(
        "SELECT COALESCE(sum(byte_len),0) FROM staged_attachments",
        [],
        |row| row.get(0),
    )?;
    // Legacy uploads had no timestamp. Give retained uploads a fresh grace period
    // and use their opaque IDs to break ties if they already exceed the budget.
    while staged > MAX_STAGED_BYTES {
        let reclaimed = reclaim_staged_attachments(tx, expiry, staged - MAX_STAGED_BYTES)?;
        ensure!(reclaimed != 0, "inconsistent channel attachment budget");
        staged -= reclaimed;
    }
    set_staged_bytes(tx, staged)
}

fn insert_channel(
    tx: &Transaction<'_>,
    id: &ChannelId,
    workspace: &str,
    name: &str,
    workspace_wide: bool,
    settings: ChannelSettings,
) -> Result<()> {
    tx.execute(
        "INSERT INTO channels(id,workspace_id,name,workspace_wide,settings) VALUES(?1,?2,?3,?4,?5)",
        params![
            id.as_ref(),
            workspace,
            name,
            workspace_wide,
            serde_json::to_string(&settings)?
        ],
    )?;
    Ok(())
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
        ChannelAccessLost
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
    let (roster, next_after) = members_page(authority, members, after, limit)?;
    Ok(ChannelResponse::Members {
        participants: roster.into_iter().map(|entry| entry.actor).collect(),
        next_after,
    })
}

fn members_page(
    authority: ChannelAuthority,
    members: Option<&[ParticipantId]>,
    after: Option<&ParticipantId>,
    limit: u16,
) -> Result<(Vec<super::WorkspaceParticipant>, Option<ParticipantId>)> {
    let ChannelAuthority {
        actor,
        participants: mut roster,
        workspace_open,
        ..
    } = authority;
    if !workspace_open {
        return Ok((Vec::new(), None));
    }
    let visible = |participant: &Participant| {
        member(participant, members) && after.is_none_or(|id| participant.id > *id)
    };
    roster.retain(|entry| entry.actor.id != actor.id && visible(&entry.actor));
    if visible(&actor) {
        roster.push(super::WorkspaceParticipant {
            actor,
            launch_id: None,
            binding_revision: 0,
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
    Ok((roster, next_after))
}

fn exact_mention<'a>(
    authority: &'a ChannelAuthority,
    mention: &ParticipantId,
    caller: (&str, u64),
) -> Option<&'a Participant> {
    let reference = mention.as_ref();
    if let Some((launch, binding)) = reference.split_once(':') {
        if !binding
            .as_bytes()
            .first()
            .is_some_and(|digit| matches!(digit, b'1'..=b'9'))
            || !binding.bytes().all(|digit| digit.is_ascii_digit())
        {
            return None;
        }
        let binding = binding.parse::<u64>().ok()?;
        if caller == (launch, binding) {
            return Some(&authority.actor);
        }
        return authority
            .participants
            .iter()
            .find(|entry| {
                entry.launch_id.as_deref() == Some(launch) && entry.binding_revision == binding
            })
            .map(|entry| &entry.actor);
    }
    authority
        .roster()
        .find(|actor| actor.kind == ParticipantKind::User && actor.id == *mention)
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

fn insert_members(
    tx: &Transaction<'_>,
    id: &ChannelId,
    workspace_id: &str,
    members: &[ParticipantId],
) -> Result<()> {
    if !members.is_empty() {
        let mut statement = tx.prepare(
            "INSERT INTO members(channel_id,participant_id,workspace_id) VALUES(?1,?2,?3)",
        )?;
        for member in members {
            statement.execute(params![id.as_ref(), member.as_ref(), workspace_id])?;
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

fn fingerprint_for(post: &ChannelPost, selector: Option<&ChannelId>) -> [u8; 32] {
    let mut writer = Sha256::new();
    if let Some(selector) = selector {
        writer.update(selector.as_ref().as_bytes());
        writer.update([0]);
    }
    serde_json::to_writer(&mut writer, post).expect("typed post serialization is infallible");
    writer.finalize().into()
}

fn history_bound(channel: &StoredChannel, before: Option<u64>, after: Option<u64>) -> i64 {
    let bound = after.unwrap_or_else(|| before.map_or(channel.last_sequence, |value| value - 1));
    i64::try_from(bound).unwrap_or(i64::MAX)
}

fn new_id() -> String {
    Uuid::new_v4().hyphenated().to_string()
}
