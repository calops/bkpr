use super::{
    ChannelAccessLost, ChannelAuthority, ChannelRoutes, STALE_USER_MENTION, WakeTarget,
    WorkspaceParticipant,
};
use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::ChannelAttentionKind;
use bkpr_protocol::channels::{
    AttachmentDescriptor, AttachmentId, AttachmentUpload, ChannelChange, ChannelChangeKind,
    ChannelCursor, ChannelId, ChannelMentionBinding, ChannelMessage, ChannelPost, ChannelRequest,
    ChannelResponse, ChannelRosterEntry, ChannelSettings, ChannelSummary, MAX_HISTORY_BYTES,
    MembershipMode, MessageId, MessageSummary, Participant, ParticipantId, ParticipantKind,
    PostIntent, WakePolicy,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};
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

#[path = "catalog.rs"]
pub(super) mod catalog;
#[path = "legacy.rs"]
mod legacy;
#[path = "wakes.rs"]
mod wakes;

const APPLICATION_ID: i64 = 0x424b4348;
// Versions 1–6 predate staging. Version 7 predates wakes.
const STORE_VERSION: i64 = 16;
const ATTENTION_ORIGIN_VERSION: i64 = 16;
const FIRST_STAGING_VERSION: i64 = 7;
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
            user_wake: WakePolicy::AddressedOrAll,
            agent_wake: WakePolicy::AddressedOrAll,
            attention_questions: false,
            attention_reports: false,
        },
    ),
];

const DIRECT_SETTINGS: ChannelSettings = ChannelSettings {
    user_wake: WakePolicy::None,
    agent_wake: WakePolicy::All,
    attention_questions: false,
    attention_reports: false,
};

fn insert_direct_channel(
    tx: &Transaction<'_>,
    id: &ChannelId,
    workspace: &str,
    from: &Participant,
    to: &Participant,
) -> Result<()> {
    let mut pair = [from.id.as_ref(), to.id.as_ref()];
    pair.sort_unstable();
    let short = |text: &str| text.floor_char_boundary(72);
    let display = format!(
        "DM: {} / {}",
        &from.display_name[..short(&from.display_name)],
        &to.display_name[..short(&to.display_name)]
    );
    insert_channel(
        tx,
        id,
        workspace,
        &format!("\u{1}{}", id.as_ref()),
        false,
        DIRECT_SETTINGS,
    )?;
    tx.execute(
        "INSERT INTO direct_channels VALUES(?1,?2,?3,?4)",
        params![id.as_ref(), pair[0], pair[1], display],
    )?;
    Ok(())
}

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

const RETIRE_OLD_WAKE_WORK: &str = "
    UPDATE notification_work SET pending=0 WHERE pending=1 AND launch_id IS NOT NULL;
    UPDATE channel_batches SET outcome='unknown' WHERE outcome IS NULL;";

const WAKE_QUEUE_SCHEMA: &str = "
    ALTER TABLE channel_batches ADD COLUMN revoked INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE notification_work ADD COLUMN enqueue_ordinal INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE notification_work ADD COLUMN channel_sequence INTEGER NOT NULL DEFAULT 0;
    DROP INDEX IF EXISTS pending_launch_wakes;
    DROP INDEX IF EXISTS active_batch_launch;
    CREATE UNIQUE INDEX active_batch_launch ON channel_batches(launch_id) WHERE outcome IS NULL AND revoked=0;
    CREATE INDEX unsettled_batch_deadline ON channel_batches(deadline) WHERE outcome IS NULL;
    CREATE INDEX pending_launch_enqueue ON notification_work(launch_id,binding_revision,participant_id,enqueue_ordinal,channel_id)
        WHERE pending=1 AND launch_id IS NOT NULL;
    CREATE INDEX pending_channel_sequence ON notification_work(launch_id,binding_revision,participant_id,channel_id,channel_sequence,addressed)
        WHERE pending=1 AND launch_id IS NOT NULL;
    CREATE INDEX wake_batch_membership ON channel_batches(channel_id,participant_id) WHERE revoked=0;";

pub(crate) struct Store {
    pub(super) connection: Connection,
    pub(super) retirement_epoch: u64,
    pub(super) catalog_epoch: Option<u64>,
}

struct StoredChannel {
    workspace_id: String,
    name: String,
    workspace_wide: bool,
    direct: bool,
    settings: ChannelSettings,
    last_sequence: u64,
    change_sequence: u64,
    last_attention: u64,
}

#[derive(serde::Serialize)]
struct ReplyTarget<'a> {
    channel_id: &'a ChannelId,
    reply_to: &'a MessageId,
}

#[derive(serde::Serialize)]
struct MessagePage<'a> {
    message: MessageSummary,
    text: &'a str,
    body_bytes: usize,
    next_offset: Option<usize>,
    reply_target: ReplyTarget<'a>,
}

fn open_session_directory(root: &Path) -> Result<File> {
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
    Ok(directory)
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let (directory, mut connection, empty) =
            open_channel_database(root, true)?.context("missing channel store")?;
        store_version(&connection, empty)?;
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF;
             PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=EXTRA;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=0;
             PRAGMA cache_size=-1024;",
        )?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = store_version(&tx, empty)?;
        if version < STORE_VERSION {
            if version == 0 {
                tx.execute_batch(include_str!("schema.sql"))?;
                tx.execute_batch(include_str!("wakes.sql"))?;
                tx.execute_batch(ATTACHMENT_STAGING_SCHEMA)?;
                tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            } else if version == 1 {
                tx.execute_batch(
                    "ALTER TABLE members ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '';
                     UPDATE members SET workspace_id=(SELECT workspace_id FROM channels WHERE id=members.channel_id);
                     CREATE INDEX workspace_wide_channels ON channels(workspace_id,id) WHERE workspace_wide=1;
                     CREATE INDEX visible_explicit_channels ON members(workspace_id,participant_id,channel_id);",
                )?;
            }
            if matches!(version, 1 | 2 | FIRST_STAGING_VERSION) {
                tx.execute_batch(include_str!("wakes.sql"))?;
            }
            let has_schema_object = |name: &str| -> rusqlite::Result<bool> {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                    [name],
                    |row| row.get(0),
                )
            };
            if !has_schema_object("catalog_identity")? {
                tx.execute_batch(include_str!("catalog.sql"))?;
            }
            if !has_schema_object("pending_channel_attention")? {
                tx.execute_batch(
                    "UPDATE notification_work SET pending=0 WHERE pending=1 AND launch_id IS NULL;
                     CREATE INDEX pending_channel_attention ON notification_work(message_id)
                        WHERE pending=1 AND launch_id IS NULL;",
                )?;
            }
            if (1..FIRST_STAGING_VERSION).contains(&version) {
                migrate_attachment_staging(&tx)?;
            }
            // Version 9 existed both with ordered wakes and with legacy direct channels.
            // Previously published catalog/attention stores also predate ordered wakes.
            let ordered_wakes: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('notification_work')
                 WHERE name='enqueue_ordinal')",
                [],
                |row| row.get(0),
            )?;
            if !ordered_wakes {
                tx.execute_batch(WAKE_QUEUE_SCHEMA)?;
                tx.execute(
                    "UPDATE channel_batches SET revoked=1,outcome='unknown' WHERE outcome='revoked'",
                    [],
                )?;
            }
            if !has_schema_object("direct_channels")? {
                tx.execute_batch(include_str!("direct.sql"))?;
            }
            if version < 12 {
                if !legacy::source_present(root) {
                    tx.execute_batch("DROP TABLE IF EXISTS agent_state;")?;
                }
                tx.execute_batch(
                    "ALTER TABLE notification_work ADD COLUMN read_at INTEGER;
                     ALTER TABLE notification_work ADD COLUMN acknowledged_at INTEGER;",
                )?;
            }
            let ordered_attention: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('notification_work')
                 WHERE name='attention_posted_ms')",
                [],
                |row| row.get(0),
            )?;
            if !ordered_attention {
                tx.execute_batch(
                    "ALTER TABLE notification_work ADD COLUMN attention_posted_ms INTEGER;
                     ALTER TABLE notification_work ADD COLUMN attention_origin TEXT;
                     UPDATE notification_work AS w
                        SET attention_posted_ms=(SELECT json_extract(p.summary,'$.posted_at_ms')
                            FROM posts p WHERE p.id=w.message_id)
                        WHERE pending=1 AND launch_id IS NULL;",
                )?;
            }
            tx.execute_batch(
                "DROP INDEX pending_channel_attention;
                 CREATE INDEX pending_channel_attention
                    ON notification_work(attention_posted_ms,message_id)
                    WHERE pending=1 AND launch_id IS NULL;",
            )?;
            tx.pragma_update(None, "user_version", STORE_VERSION)?;
        }
        if version != 0 {
            // A new runtime cannot offer abandoned work; old queue rows need no order backfill.
            tx.execute_batch(RETIRE_OLD_WAKE_WORK)?;
        }
        tx.commit()?;
        if version == 0 {
            directory.sync_all()?;
        }
        Ok(Self {
            connection,
            retirement_epoch: 0,
            catalog_epoch: None,
        })
    }

    pub(super) fn maintain_legacy(
        &mut self,
        root: &Path,
        now_ms: u64,
        workspace_for_pane: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<u64>> {
        legacy::maintain(&mut self.connection, root, now_ms, workspace_for_pane)
    }

    pub(super) fn retire_closed_workspaces(
        &mut self,
        workspace_open: &impl Fn(&str) -> bool,
    ) -> Result<()> {
        let mut closed = Vec::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT c.workspace_id FROM notification_work w
                 JOIN channels c ON c.id=w.channel_id WHERE w.pending=1 AND w.launch_id IS NOT NULL
                    AND NOT EXISTS(SELECT 1 FROM direct_channels d WHERE d.channel_id=c.id)
                 UNION SELECT c.workspace_id FROM channel_batches b
                 JOIN channels c ON c.id=b.channel_id WHERE b.outcome IS NULL AND b.revoked=0
                    AND NOT EXISTS(SELECT 1 FROM direct_channels d WHERE d.channel_id=c.id)",
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
                "UPDATE notification_work SET pending=0 WHERE pending=1 AND launch_id IS NOT NULL AND channel_id IN
                    (SELECT id FROM channels WHERE workspace_id=?1 AND id NOT IN (SELECT channel_id FROM direct_channels))",
            )?;
            for id in &closed {
                statement.execute([id])?;
            }
            let mut batches = tx.prepare(
                "UPDATE channel_batches SET revoked=1 WHERE outcome IS NULL AND revoked=0 AND channel_id IN
                    (SELECT id FROM channels WHERE workspace_id=?1 AND id NOT IN (SELECT channel_id FROM direct_channels))",
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
        provision_channels(&tx, workspaces)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn authorize_upload(
        &self,
        id: &ChannelId,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        caller_workspace: Option<&str>,
    ) -> Result<()> {
        let (channel, authority) = self.access(id, admit, caller_workspace)?;
        channel.require_shared()?;
        require_write(&authority)
    }

    pub(super) fn request(
        &mut self,
        request: ChannelRequest,
        admit: impl Fn(&str) -> Result<ChannelAuthority>,
        routes: ChannelRoutes<'_>,
        attention_origin: Option<&str>,
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
            ChannelRequest::UserRoster {
                workspace_id,
                channel_id,
                after,
                limit,
            } => {
                let (authority, members) = if let Some(id) = &channel_id {
                    let (channel, authority) = self.access(id, &admit, routes.workspace)?;
                    ensure!(channel.workspace_id == workspace_id, ChannelAccessLost);
                    channel.require_shared()?;
                    (authority, self.members_if_explicit(id, &channel)?)
                } else {
                    (workspace_authority(&workspace_id, &admit)?, None)
                };
                require_user(&authority)?;
                let (entries, next_after) =
                    members_page(authority, members.as_deref(), after.as_ref(), limit)?;
                let participants = entries
                    .into_iter()
                    .map(|entry| ChannelRosterEntry {
                        binding: entry.launch_id.map(|launch_id| ChannelMentionBinding {
                            participant_id: entry.actor.id.clone(),
                            launch_id,
                            binding_generation: entry.binding_revision,
                        }),
                        participant: entry.actor,
                    })
                    .collect();
                return Ok((
                    ChannelResponse::UserRoster {
                        participants,
                        next_after,
                    },
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
            | ChannelRequest::PostUser { channel_id, .. }
            | ChannelRequest::History { channel_id, .. }
            | ChannelRequest::Read { channel_id, .. }
            | ChannelRequest::MarkRead { channel_id, .. }
            | ChannelRequest::UploadAttachment { channel_id, .. }
            | ChannelRequest::GetAttachment { channel_id, .. } => channel_id,
            _ => unreachable!("workspace and subscription routes are handled separately"),
        };
        let (channel, authority) = self.access(channel_id, &admit, routes.workspace)?;
        if !matches!(
            request,
            ChannelRequest::History { .. }
                | ChannelRequest::Read { .. }
                | ChannelRequest::MarkRead { .. }
                | ChannelRequest::Post { .. }
        ) {
            channel.require_shared()?;
        }
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
            request @ (ChannelRequest::Post { .. } | ChannelRequest::PostUser { .. }) => {
                let (channel_id, post, bindings) = match request {
                    ChannelRequest::Post { channel_id, post } => (channel_id, post, None),
                    ChannelRequest::PostUser {
                        channel_id,
                        post,
                        bindings,
                    } => (channel_id, post, Some(bindings)),
                    _ => unreachable!(),
                };
                let bindings = bindings.as_deref();
                let fingerprint = fingerprint_for(&post, selector.as_ref(), bindings);
                if let Some(receipt) =
                    self.retry_post(&authority.actor.id, &post, &fingerprint, Some(&channel_id))?
                {
                    return Ok((receipt, false));
                }
                self.post(
                    channel,
                    post,
                    authority,
                    retire,
                    routes,
                    attention_origin,
                    (&channel_id, &fingerprint, bindings),
                    None,
                )
            }
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
        caller_workspace: Option<&str>,
    ) -> Result<(StoredChannel, ChannelAuthority)> {
        let channel = self.connection.query_row(
            "SELECT c.workspace_id,COALESCE(d.display_name,c.name),c.workspace_wide,
                    c.settings,c.last_sequence,c.change_sequence,c.last_attention,d.channel_id IS NOT NULL
             FROM channels c LEFT JOIN direct_channels d ON d.channel_id=c.id WHERE c.id=?1",
            [id.as_ref()],
            |row| StoredChannel::read(row, 0),
        ).optional()?.context(ChannelAccessLost)?;
        // Direct membership is session-owned. An agent proves its binding in its
        // own workspace; the creator's workspace only groups the user's discovery.
        let workspace = if channel.direct {
            caller_workspace.unwrap_or(&channel.workspace_id)
        } else {
            &channel.workspace_id
        };
        let authority = workspace_authority(workspace, admit)?;
        if authority.actor.kind == ParticipantKind::Agent {
            if channel.direct {
                ensure!(
                    exists(
                        &self.connection,
                        "SELECT EXISTS(SELECT 1 FROM direct_channels WHERE channel_id=?1
                         AND (first_participant=?2 OR second_participant=?2))",
                        id,
                        authority.actor.id.as_ref(),
                    )?,
                    ChannelAccessLost
                );
            } else if !channel.workspace_wide {
                ensure!(
                    self.explicit_member(id, &authority.actor.id)?,
                    ChannelAccessLost
                );
            }
        }
        Ok((channel, authority))
    }

    fn explicit_member(&self, channel: &ChannelId, participant: &ParticipantId) -> Result<bool> {
        exists(
            &self.connection,
            "SELECT EXISTS(SELECT 1 FROM members WHERE channel_id=?1 AND participant_id=?2)",
            channel,
            participant.as_ref(),
        )
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
                 UNION ALL
                 SELECT channel_id FROM direct_channels
                   WHERE NOT ?4 AND first_participant=?2 AND channel_id>?3
                 UNION ALL
                 SELECT channel_id FROM direct_channels
                   WHERE NOT ?4 AND second_participant=?2 AND channel_id>?3
                 ORDER BY 1 LIMIT ?5
             )
             SELECT c.id,c.workspace_id,COALESCE(d.display_name,c.name),c.workspace_wide,c.settings,
                    c.last_sequence,c.change_sequence,c.last_attention,d.channel_id IS NOT NULL,COALESCE(p.through,0)
             FROM visible v JOIN channels c ON c.id=v.id
               LEFT JOIN direct_channels d ON d.channel_id=c.id
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
                |r| Ok(StoredChannel::read(r, 1)?.summary(ChannelId(r.get(0)?), r.get(9)?, user)),
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
                    direct: false,
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
                    "UPDATE channel_batches SET revoked=1 WHERE channel_id=?1 AND revoked=0
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

    #[allow(clippy::too_many_arguments)]
    fn post(
        &mut self,
        channel: StoredChannel,
        mut post: ChannelPost,
        authority: ChannelAuthority,
        retire: impl FnOnce(&mut Self, &str) -> Result<bool>,
        routes: ChannelRoutes<'_>,
        attention_origin: Option<&str>,
        (id, fingerprint, bindings): (&ChannelId, &[u8], Option<&[ChannelMentionBinding]>),
        selected_direct: Option<(WorkspaceParticipant, bool)>,
    ) -> Result<(ChannelResponse, bool)> {
        if bindings.is_some() {
            require_user(&authority)?;
        } else if authority.actor.kind == ParticipantKind::User {
            ensure!(
                post.mentions.iter().all(|id| *id == authority.actor.id),
                "structured user mentions of agents require an exact binding from the channel roster"
            );
        }
        require_write(&authority)?;
        if channel.direct {
            ensure!(
                authority.actor.kind == ParticipantKind::Agent,
                "direct channels are read-only for users"
            );
            ensure!(
                post.attachments.is_empty(),
                "direct channels do not support attachments"
            );
        }
        let mut authority = authority;
        let direct_members = if channel.direct && selected_direct.is_none() {
            let pair = self.direct_pair(id)?.context(ChannelAccessLost)?;
            let peer = pair
                .iter()
                .find(|p| **p != authority.actor.id)
                .context(ChannelAccessLost)?;
            if !authority.participants.iter().any(|p| p.actor.id == *peer)
                && let Some(participant) = (routes.direct_peer)(peer)?
            {
                authority.participants.push(participant);
            }
            Some(pair)
        } else {
            None
        };
        let selected_peer = selected_direct.as_ref().map(|(peer, _)| peer);
        let recipients =
            selected_peer.map_or(authority.participants.as_slice(), std::slice::from_ref);
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
        let may_wake = !recipients.is_empty()
            && policy != WakePolicy::None
            && (policy != WakePolicy::AddressedOnly || !post.mentions.is_empty());
        let shared_members = if !channel.direct && (may_wake || !post.mentions.is_empty()) {
            self.members_if_explicit(id, &channel)?
        } else {
            None
        };
        let explicit_members = direct_members
            .as_ref()
            .map(|pair| pair.as_slice())
            .or(shared_members.as_deref());
        let can_mention = |participant: &Participant| match selected_peer {
            Some(peer) => {
                participant.kind == ParticipantKind::User
                    || participant.id == authority.actor.id
                    || participant.id == peer.actor.id
            }
            None => member(participant, explicit_members),
        };
        let mut addressed_agent = false;
        for mention in &mut post.mentions {
            let target = if let Some(bindings) = bindings {
                user_mention(&authority, mention, bindings)
            } else if let Some(caller) = routes.exact_mentions {
                exact_mention(&authority, recipients, mention, caller)
            } else {
                authority
                    .roster()
                    .chain(selected_peer.map(|peer| &peer.actor))
                    .find(|actor| actor.id == *mention)
            }
            .filter(|actor| can_mention(actor))
            .context(if bindings.is_some() {
                STALE_USER_MENTION
            } else {
                "channel mention no longer names a current member; refresh the roster"
            })?;
            addressed_agent |= target.kind == ParticipantKind::Agent;
            if *mention != target.id {
                mention.clone_from(&target.id);
            }
        }
        let mut wakes = Vec::new();
        if may_wake {
            for entry in recipients {
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
                        addressed: addressed || channel.direct,
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
        // A post admitted before workspace closure still owes the host its immutable
        // attention event. Only the live agent wake is conditional on continued access.
        let pending_wakes =
            !wakes.is_empty() && (channel.direct || !retire(self, &channel.workspace_id)?);
        if pending_wakes {
            self.reclaim_wake_capacity(wakes.len(), routes.binding_live)?;
        }
        if attention {
            self.reserve_attention(message.posted_at_ms)?;
        }
        // The store slot serializes channel membership. Final admission checks the
        // selected live bindings after any maintenance, before the post transaction.
        let _binding_guard = bindings
            .filter(|bindings| !bindings.is_empty())
            .map(|bindings| (routes.fence_user_mentions)(&channel.workspace_id, bindings))
            .transpose()?;
        let tx = self.connection.transaction()?;
        if let Some((peer, true)) = &selected_direct {
            insert_direct_channel(&tx, id, &channel.workspace_id, &message.author, &peer.actor)?;
        }
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
        insert_post(
            &tx,
            id,
            &message,
            &post.idempotency_key,
            fingerprint,
            &post.text,
            (wake_count, attention),
        )?;
        if !wakes.is_empty() || attention {
            // `posts` is the last rowid table inserted into by this transaction.
            let enqueue_ordinal = tx.last_insert_rowid();
            let mut insert = tx.prepare(
                "INSERT INTO notification_work(channel_id,message_id,participant_id,launch_id,addressed,pending,binding_revision,enqueue_ordinal,channel_sequence,attention_posted_ms,attention_origin)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for wake in &wakes {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    wake.participant,
                    wake.launch,
                    wake.addressed,
                    pending_wakes,
                    wake.binding,
                    enqueue_ordinal,
                    sequence,
                    None::<u64>,
                    None::<&str>,
                ])?;
            }
            if attention {
                insert.execute(params![
                    id.as_ref(),
                    message.id.as_ref(),
                    "",
                    None::<&str>,
                    false,
                    true,
                    0,
                    enqueue_ordinal,
                    sequence,
                    message.posted_at_ms,
                    attention_origin,
                ])?;
            }
        }
        tx.commit()?;
        if pending_wakes {
            (routes.publish)(&wakes);
        }
        Ok((
            ChannelResponse::Posted {
                channel_id: Some(id.clone()),
                message,
                wake_count,
                attention,
            },
            true,
        ))
    }

    fn retry_post(
        &self,
        actor: &ParticipantId,
        post: &ChannelPost,
        fingerprint: &[u8],
        expected: Option<&ChannelId>,
    ) -> Result<Option<ChannelResponse>> {
        let retry = self
            .connection
            .query_row(
                "SELECT channel_id,fingerprint,summary,wake_count,attention
             FROM posts WHERE author_id=?1 AND request_key=?2",
                params![actor.as_ref(), post.idempotency_key],
                |row| {
                    let id = row.get_ref(0)?.as_str()?;
                    if expected.is_some_and(|expected| id != expected.as_ref())
                        || row.get_ref(1)?.as_blob()? != fingerprint
                    {
                        return Ok(None);
                    }
                    Ok(Some(ChannelResponse::Posted {
                        channel_id: Some(ChannelId(id.to_owned())),
                        message: json(row, 2)?,
                        wake_count: row.get(3)?,
                        attention: row.get(4)?,
                    }))
                },
            )
            .optional()?;
        retry
            .map(|receipt| receipt.context("channel post conflicts with an existing retry key"))
            .transpose()
    }

    fn direct_pair(&self, channel: &ChannelId) -> Result<Option<[ParticipantId; 2]>> {
        Ok(self.connection.query_row(
            "SELECT first_participant,second_participant FROM direct_channels WHERE channel_id=?1",
            [channel.as_ref()],
            |row| Ok([ParticipantId(row.get(0)?), ParticipantId(row.get(1)?)]),
        ).optional()?)
    }

    /// Post to an exact launch, resolving the same-workspace target only on a receipt miss.
    pub(crate) fn post_direct(
        &mut self,
        post: ChannelPost,
        authority: ChannelAuthority,
        launch_id: &str,
        workspace: &str,
        routes: ChannelRoutes<'_>,
        resolve_target: impl FnOnce() -> Result<WorkspaceParticipant>,
    ) -> Result<(ChannelResponse, bool)> {
        let selector = ChannelId(format!("\u{1}{launch_id}"));
        let fingerprint = fingerprint_for(&post, Some(&selector), None);
        if let Some(receipt) = self.retry_post(&authority.actor.id, &post, &fingerprint, None)? {
            return Ok((receipt, false));
        }
        let target = resolve_target()?;
        ensure!(
            target.actor.id != authority.actor.id,
            "cannot post a direct message to yourself"
        );
        let mut pair = [authority.actor.id.as_ref(), target.actor.id.as_ref()];
        pair.sort_unstable();
        let existing = self.connection.query_row(
            "SELECT c.id,c.workspace_id,d.display_name,c.workspace_wide,c.settings,c.last_sequence,
                    c.change_sequence,c.last_attention,1
             FROM direct_channels d JOIN channels c ON c.id=d.channel_id
             WHERE d.first_participant=?1 AND d.second_participant=?2",
            pair, |row| Ok((ChannelId(row.get(0)?), StoredChannel::read(row, 1)?)),
        ).optional()?;
        let is_new = existing.is_none();
        let (id, channel) = existing.unwrap_or_else(|| {
            (
                ChannelId(new_id()),
                StoredChannel {
                    workspace_id: workspace.to_owned(),
                    name: String::new(),
                    workspace_wide: false,
                    direct: true,
                    settings: DIRECT_SETTINGS,
                    last_sequence: 0,
                    change_sequence: 0,
                    last_attention: 0,
                },
            )
        });
        self.post(
            channel,
            post,
            authority,
            |_, _| Ok(false),
            routes,
            None,
            (&id, &fingerprint, None),
            Some((target, is_new)),
        )
    }

    pub(crate) fn record_channel_read(
        &self,
        message: &MessageId,
        recipient: super::ChannelRecipient<'_>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE notification_work SET read_at=?5
             WHERE message_id=?1 AND participant_id=?2
                AND launch_id=?3 AND binding_revision=?4 AND read_at IS NULL",
            params![
                message.as_ref(),
                recipient.participant,
                recipient.launch,
                recipient.binding,
                unix_time_ms()?
            ],
        )?;
        Ok(())
    }

    pub(crate) fn acknowledge_channel_post(
        &self,
        channel: &ChannelId,
        message: &MessageId,
        recipient: super::ChannelRecipient<'_>,
        workspace: &str,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<bool> {
        self.access(channel, admit, Some(workspace))?;
        let (read, acknowledged): (bool, bool) = self
            .connection
            .query_row(
                "SELECT read_at IS NOT NULL,acknowledged_at IS NOT NULL FROM notification_work
             WHERE channel_id=?1 AND message_id=?2 AND participant_id=?3
                AND launch_id=?4 AND binding_revision=?5",
                params![
                    channel.as_ref(),
                    message.as_ref(),
                    recipient.participant,
                    recipient.launch,
                    recipient.binding
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .context("no delivery for this channel post and binding")?;
        ensure!(
            read,
            "read the delivered channel post before acknowledging it"
        );
        if acknowledged {
            return Ok(false);
        }
        Ok(self.connection.execute(
            "UPDATE notification_work SET acknowledged_at=?6
             WHERE channel_id=?1 AND message_id=?2 AND participant_id=?3
                AND launch_id=?4 AND binding_revision=?5",
            params![
                channel.as_ref(),
                message.as_ref(),
                recipient.participant,
                recipient.launch,
                recipient.binding,
                unix_time_ms()?
            ],
        )? == 1)
    }

    fn reserve_attention(&self, posted_at_ms: u64) -> Result<()> {
        let count = || -> rusqlite::Result<usize> {
            self.connection.query_row(
                "SELECT count(*) FROM notification_work WHERE pending=1 AND launch_id IS NULL",
                [],
                |row| row.get(0),
            )
        };
        if count()? < MAX_PENDING {
            return Ok(());
        }
        if let Some(cutoff) =
            posted_at_ms.checked_sub(crate::notifications::CHANNEL_ATTENTION_RETRY_MS)
        {
            self.connection.execute(
                "UPDATE notification_work SET pending=0,attention_origin=NULL
                 WHERE pending=1 AND launch_id IS NULL AND attention_posted_ms<=?1",
                [cutoff],
            )?;
        }
        ensure!(count()? < MAX_PENDING, "channel attention queue is full");
        Ok(())
    }

    pub(crate) fn compact_members(
        &mut self,
        request: ChannelRequest,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        caller: (&str, u64),
        caller_workspace: Option<&str>,
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
            .resolve_builtin(&channel_id, caller_workspace, admit)?
            .unwrap_or(channel_id);
        let (channel, authority) = self.access(&channel_id, admit, caller_workspace)?;
        channel.require_shared()?;
        let members = self.members_if_explicit(&channel_id, &channel)?;
        let (entries, next_after) =
            members_page(authority, members.as_deref(), after.as_ref(), limit)?;
        let participants: Vec<_> = entries.into_iter().map(|entry| {
            let launch = (entry.actor.kind == ParticipantKind::Agent).then(|| {
                entry.launch_id.as_deref().map_or(caller, |id| (id, entry.binding_revision))
            });
            let mention_id = launch.map_or_else(|| entry.actor.id.0.clone(), |(id, generation)| {
                super::agent_mention_id(id, generation)
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
        caller_workspace: Option<&str>,
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
            .resolve_builtin(&channel_id, caller_workspace, admit)?
            .unwrap_or(channel_id);
        let (channel, _) = self.access(&channel_id, admit, caller_workspace)?;
        let (lower, upper) = history_bounds(&channel, before, after);
        let sql = if after.is_some() {
            "SELECT summary FROM posts WHERE channel_id=?1 AND sequence>?2 AND sequence<=?3
             ORDER BY sequence LIMIT ?4"
        } else {
            "SELECT summary FROM posts WHERE channel_id=?1 AND sequence>?2 AND sequence<=?3
             ORDER BY sequence DESC LIMIT ?4"
        };
        let mut statement = self.connection.prepare(sql)?;
        let mut messages = statement
            .query_map(params![channel_id.as_ref(), lower, upper, limit], |row| {
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
            "SELECT summary,body FROM posts WHERE channel_id=?1 AND sequence>?2 AND sequence<=?3
             ORDER BY sequence LIMIT ?4"
        } else {
            "SELECT summary,body FROM posts WHERE channel_id=?1 AND sequence>?2 AND sequence<=?3
             ORDER BY sequence DESC LIMIT ?4"
        };
        let (lower, upper) = history_bounds(channel, before, after);
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(params![id.as_ref(), lower, upper, limit])?;
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
        caller_workspace: Option<&str>,
    ) -> Result<serde_json::Value> {
        let ChannelRequest::Read {
            channel_id,
            message_id,
        } = request
        else {
            unreachable!("compact message requires a read request");
        };
        let channel_id = self
            .resolve_builtin(&channel_id, caller_workspace, admit)?
            .unwrap_or(channel_id);
        self.access(&channel_id, admit, caller_workspace)?;
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
            reply_target: ReplyTarget {
                channel_id,
                reply_to: message_id,
            },
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
        catalog::record_read_position(&tx, id, actor, previous)?;
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
        caller_workspace: Option<&str>,
    ) -> Result<(u64, Vec<ChannelChange>, bool, bool)> {
        let (channel, authority) = self.access(id, admit, caller_workspace)?;
        let after = after.unwrap_or(channel.change_sequence);
        ensure!(
            after <= channel.change_sequence,
            "channel cursor is ahead of the change stream"
        );
        if after == channel.change_sequence {
            return Ok((after, Vec::new(), false, authority.workspace_open));
        }
        let mut statement = self.connection.prepare(
            "SELECT sequence,kind FROM changes
             WHERE channel_id=?1 AND participant_id IS NULL AND sequence>?3 AND sequence<=?4
             UNION ALL
             SELECT sequence,kind FROM changes
             WHERE channel_id=?1 AND participant_id=?2 AND sequence>?3 AND sequence<=?4
             ORDER BY sequence LIMIT ?5",
        )?;
        let mut changes = statement
            .query_map(
                params![
                    id.as_ref(),
                    authority.actor.id.as_ref(),
                    after,
                    channel.change_sequence,
                    limit + 1
                ],
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

pub(super) fn unix_time_ms() -> Result<u64> {
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

fn store_version(connection: &Connection, empty: bool) -> Result<i64> {
    let (application, version): (i64, i64) = connection.query_row(
        "SELECT application_id,user_version FROM pragma_application_id(),pragma_user_version()",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        (empty && application == 0 && version == 0)
            || (application == APPLICATION_ID && (1..=STORE_VERSION).contains(&version)),
        "foreign or unsupported channel store"
    );
    Ok(version)
}

fn provision_channels<'a>(
    tx: &Transaction<'_>,
    workspaces: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut exists =
        tx.prepare("SELECT EXISTS(SELECT 1 FROM channels WHERE workspace_id=?1 AND name=?2)")?;
    for workspace in workspaces {
        for (name, settings) in PRESETS {
            if !exists.query_row([workspace, name], |row| row.get::<_, bool>(0))? {
                insert_channel(tx, &ChannelId(new_id()), workspace, name, true, settings)?;
            }
        }
    }
    Ok(())
}

pub(super) fn insert_channel(
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

pub(super) fn insert_post(
    tx: &Transaction<'_>,
    id: &ChannelId,
    message: &MessageSummary,
    request_key: &str,
    fingerprint: &[u8],
    text: &str,
    (wake_count, attention): (u32, bool),
) -> Result<()> {
    tx.execute(
        "INSERT INTO posts(id,channel_id,sequence,author_id,request_key,fingerprint,summary,body,wake_count,attention)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![message.id.as_ref(), id.as_ref(), message.sequence, message.author.id.as_ref(),
                request_key, fingerprint, serde_json::to_string(message)?, text, wake_count, attention],
    )?;
    tx.execute(
        "UPDATE channels SET last_sequence=?2,last_attention=CASE WHEN ?3 THEN ?2 ELSE last_attention END WHERE id=?1",
        params![id.as_ref(), message.sequence, attention],
    )?;
    add_change(tx, id, ChannelChangeKind::Post, None)
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
            direct: row.get(start + 7)?,
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
            direct: self.direct,
        }
    }

    fn require_shared(&self) -> Result<()> {
        ensure!(
            !self.direct,
            "direct channel does not support this operation"
        );
        Ok(())
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
    require_user(authority)
}

fn require_user(authority: &ChannelAuthority) -> Result<()> {
    ensure!(
        authority.actor.kind == ParticipantKind::User,
        "operation requires the session user",
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

fn user_mention<'a>(
    authority: &'a ChannelAuthority,
    mention: &ParticipantId,
    bindings: &[ChannelMentionBinding],
) -> Option<&'a Participant> {
    let binding = bindings.iter().find(|binding| binding.participant_id == *mention);
    if *mention == authority.actor.id {
        return binding.is_none().then_some(&authority.actor);
    }
    let binding = binding?;
    let mut entries = authority
        .participants
        .iter()
        .filter(|entry| entry.actor.id == *mention);
    let entry = entries.next()?;
    (entries.next().is_none()
        && entry.actor.kind == ParticipantKind::Agent
        && entry.launch_id.as_deref() == Some(binding.launch_id.as_str())
        && entry.binding_revision == binding.binding_generation)
        .then_some(&entry.actor)
}

fn exact_mention<'a>(
    authority: &'a ChannelAuthority,
    recipients: &'a [WorkspaceParticipant],
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
        return recipients
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

fn private_storage_metadata(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o077 == 0,
                "unsafe channel storage file"
            );
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn open_channel_database(root: &Path, create: bool) -> Result<Option<(File, Connection, bool)>> {
    let directory = open_session_directory(root)?;
    let path = root.join(STORE_NAME);
    // Closing raw descriptors for a live SQLite file can cancel POSIX locks
    // held by other connections in this process. Only SQLite opens existing files.
    let empty = match private_storage_metadata(&path)? {
        Some(metadata) => metadata.len() == 0,
        None if !create => return Ok(None),
        None => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            true
        }
    };
    if empty && !create {
        return Ok(None);
    }
    for name in [
        "channels.sqlite3-journal",
        "channels.sqlite3-wal",
        "channels.sqlite3-shm",
    ] {
        private_storage_metadata(&root.join(name))?;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::from_bits_retain(rusqlite::ffi::SQLITE_OPEN_NOFOLLOW);
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_millis(100))?;
    Ok(Some((directory, connection, empty)))
}

/// Attention-only connection: no workspace authority, runtime migration, or wake retirement.
pub(super) struct AttentionStore {
    connection: Connection,
    ordered: bool,
}

pub(super) struct AttentionEvent {
    pub(super) channel_id: ChannelId,
    pub(super) workspace_id: String,
    pub(super) message_id: MessageId,
    pub(super) sequence: u64,
    pub(super) posted_at_ms: u64,
    pub(super) intent: ChannelAttentionKind,
    pub(super) origin: Option<bkpr_protocol::SessionInfo>,
}

impl AttentionStore {
    pub(super) fn open(root: &Path) -> Result<Option<Self>> {
        let Some((_directory, connection, empty)) = open_channel_database(root, false)? else {
            return Ok(None);
        };
        let version = store_version(&connection, empty)?;
        if version == 0 {
            return Ok(None);
        }
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=EXTRA;")?;
        Ok(Some(Self {
            connection,
            ordered: version >= ATTENTION_ORIGIN_VERSION,
        }))
    }

    pub(super) fn next(&self) -> Result<Option<AttentionEvent>> {
        if self.ordered {
            return Self::query(&self.connection, true);
        }
        // Check the schema in the same snapshot as the event. A concurrent first
        // session open must not turn a newly committed origin into a legacy one.
        let tx = self.connection.unchecked_transaction()?;
        let version = store_version(&tx, false)?;
        let event = if version >= ATTENTION_ORIGIN_VERSION {
            Self::query(&tx, true)?
        } else {
            let durable: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='pending_channel_attention')",
                [],
                |row| row.get(0),
            )?;
            if durable {
                Self::query(&tx, false)?
            } else {
                None
            }
        };
        tx.commit()?;
        Ok(event)
    }

    fn query(connection: &Connection, ordered: bool) -> Result<Option<AttentionEvent>> {
        let sql = if ordered {
            "SELECT w.channel_id,c.workspace_id,w.message_id,p.sequence,w.attention_posted_ms,
                    json_extract(p.summary,'$.intent'),w.attention_origin FROM notification_work w
             JOIN posts p ON p.id=w.message_id JOIN channels c ON c.id=w.channel_id
             WHERE w.pending=1 AND w.launch_id IS NULL
             ORDER BY w.attention_posted_ms,w.message_id LIMIT 1"
        } else {
            "SELECT w.channel_id,c.workspace_id,w.message_id,p.sequence,
                    json_extract(p.summary,'$.posted_at_ms'),json_extract(p.summary,'$.intent'),NULL
             FROM notification_work w
             JOIN posts p ON p.id=w.message_id JOIN channels c ON c.id=w.channel_id
             WHERE w.pending=1 AND w.launch_id IS NULL
             ORDER BY json_extract(p.summary,'$.posted_at_ms'),w.message_id LIMIT 1"
        };
        Ok(connection
            .query_row(sql, [], |row| {
                Ok(AttentionEvent {
                    channel_id: ChannelId(row.get(0)?),
                    workspace_id: row.get(1)?,
                    message_id: MessageId(row.get(2)?),
                    sequence: row.get(3)?,
                    posted_at_ms: row.get(4)?,
                    intent: match row.get_ref(5)?.as_str()? {
                        "question" => ChannelAttentionKind::Question,
                        "report" => ChannelAttentionKind::Report,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                    origin: if row.get_ref(6)? == rusqlite::types::ValueRef::Null {
                        None
                    } else {
                        Some(json(row, 6)?)
                    },
                })
            })
            .optional()?)
    }

    pub(super) fn finish(&self, id: &MessageId, has_origin: bool) -> Result<()> {
        let sql = if has_origin {
            "UPDATE notification_work SET pending=0,attention_origin=NULL
             WHERE message_id=?1 AND participant_id='' AND launch_id IS NULL AND pending=1"
        } else {
            "UPDATE notification_work SET pending=0
             WHERE message_id=?1 AND participant_id='' AND launch_id IS NULL AND pending=1"
        };
        self.connection.execute(sql, [id.as_ref()])?;
        Ok(())
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

fn fingerprint_for(
    post: &ChannelPost,
    selector: Option<&ChannelId>,
    bindings: Option<&[ChannelMentionBinding]>,
) -> [u8; 32] {
    let mut writer = Sha256::new();
    if let Some(selector) = selector {
        writer.update(selector.as_ref().as_bytes());
        writer.update([0]);
    }
    serde_json::to_writer(&mut writer, post).expect("typed post serialization is infallible");
    if let Some(bindings) = bindings {
        writer.update(b"\0user mention bindings\0");
        serde_json::to_writer(&mut writer, bindings)
            .expect("typed mention binding serialization is infallible");
    }
    writer.finalize().into()
}

fn history_bounds(channel: &StoredChannel, before: Option<u64>, after: Option<u64>) -> (i64, u64) {
    // Direct posts can commit from another connection. Exclude rows newer than
    // the channel metadata read by access, including its returned change cursor.
    let lower = i64::try_from(after.unwrap_or(0)).unwrap_or(i64::MAX);
    let upper = before.map_or(channel.last_sequence, |value| {
        (value - 1).min(channel.last_sequence)
    });
    (lower, upper)
}

fn new_id() -> String {
    Uuid::new_v4().hyphenated().to_string()
}
