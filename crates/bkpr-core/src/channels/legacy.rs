//! One-shot #84 launch-ledger import. There is no live reader for an old inbox.
use super::{insert_direct_channel, insert_post, open_session_directory};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::{
    AgentScope, HarnessKind,
    channels::{
        ChannelId, MAX_DIRECT_MESSAGE_BYTES, MessageId, MessageSummary, Participant, ParticipantId,
        ParticipantKind, PostIntent,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::CStr,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};
use uuid::Uuid;

#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;

const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_STORE_BYTES: u64 = 128 * 1024 * 1024;
const SOURCE: &CStr = c"state.json";
const STAGED: &CStr = c"state.json.migrating";

#[derive(Deserialize)]
struct Document {
    version: u32,
    agents: Option<BTreeMap<String, Enrollment>>,
    messages: Option<BTreeMap<String, Receipt>>,
    sequence: Option<u64>,
    retained: Option<Vec<RetainedBody>>,
}

impl Document {
    fn into_ledger(self) -> Result<Ledger> {
        ensure!(
            self.version == 1 && self.retained.is_none(),
            "invalid legacy agent ledger"
        );
        Ok(Ledger {
            agents: self.agents.context("missing legacy enrollments")?,
            messages: self.messages.context("missing legacy receipts")?,
            sequence: self.sequence.context("missing legacy sequence")?,
        })
    }
}

struct Ledger {
    agents: BTreeMap<String, Enrollment>,
    messages: BTreeMap<String, Receipt>,
    sequence: u64,
}

#[derive(Deserialize)]
struct Enrollment {
    pane_id: String,
    harness: HarnessKind,
    exited_at: Option<u64>,
}

#[derive(Deserialize)]
struct Receipt {
    id: String,
    from: String,
    to: String,
    text: Option<String>,
    scope: AgentScope,
    sequence: u64,
    created_at: u64,
    acknowledged_at: Option<u64>,
    expired_at: Option<u64>,
}

impl Receipt {
    fn purged(&self) -> bool {
        self.acknowledged_at.is_some() || self.expired_at.is_some()
    }
}

// Version 2 of this file is purge-only: no ids, participants, receipts or API.
#[derive(Serialize, Deserialize)]
struct Quarantine {
    version: u32,
    retained: Vec<RetainedBody>,
}

#[derive(Serialize, Deserialize)]
struct RetainedBody {
    expires_at: u64,
    text: String,
}

pub(super) fn source_present(root: &Path) -> bool {
    !matches!(fs::symlink_metadata(root.join("agents/state.json")),
        Err(error) if error.kind() == io::ErrorKind::NotFound)
}

pub(super) fn maintain(
    connection: &mut Connection,
    root: &Path,
    now: u64,
    workspace_for_pane: &impl Fn(&str) -> Option<String>,
) -> Result<Option<u64>> {
    let root = open_session_directory(root)?;
    let Some(directory) = open_at(&root, c"agents", libc::O_RDONLY | libc::O_DIRECTORY)? else {
        return Ok(None);
    };
    let metadata = directory.metadata()?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
        "legacy agents directory is not private"
    );
    let Some(source) = existing(&directory, SOURCE)? else {
        return Ok(None);
    };
    let source_stamp = stamp(&source)?;
    let document: Document = decode(&source)?;
    match document.version {
        1 => {
            let (staged, mut quarantine) = stage_import(
                connection,
                &directory,
                &source_stamp,
                document.into_ledger()?,
                now,
                workspace_for_pane,
            )?;
            replace(&directory, &source_stamp, &staged)?;
            let published_stamp = stamp(&staged)?;
            finish(&directory, &published_stamp, &mut quarantine, now)
        }
        2 => {
            ensure!(
                document.agents.is_none()
                    && document.messages.is_none()
                    && document.sequence.is_none(),
                "legacy quarantine contains inbox state"
            );
            let mut quarantine = Quarantine {
                version: 2,
                retained: document
                    .retained
                    .context("missing legacy quarantine bodies")?,
            };
            remove_staged(&directory)?;
            finish(&directory, &source_stamp, &mut quarantine, now)
        }
        _ => anyhow::bail!("unsupported legacy agent ledger version"),
    }
}

fn stage_import(
    connection: &mut Connection,
    directory: &File,
    source_stamp: &str,
    ledger: Ledger,
    now: u64,
    workspace_for_pane: &impl Fn(&str) -> Option<String>,
) -> Result<(File, Quarantine)> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_dm_import (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            source TEXT NOT NULL,
            staged TEXT NOT NULL
        ) STRICT;",
    )?;
    let committed = tx
        .query_row(
            "SELECT source,staged FROM legacy_dm_import WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((expected, staged_stamp)) = committed {
        // The SQL commit owns the import. A crash before rename must not rerun it
        // against a different pane registry or restart a newly retired launch's TTL.
        ensure!(
            expected == source_stamp,
            "legacy agent ledger was replaced after import"
        );
        let staged = existing(directory, STAGED)?.context("missing staged legacy cleanup")?;
        ensure!(
            staged_stamp == stamp(&staged)?,
            "staged legacy cleanup was replaced"
        );
        let quarantine = decode::<Quarantine>(&staged)?;
        tx.rollback()?;
        return Ok((staged, quarantine));
    }

    remove_staged(directory)?;
    let quarantine = import(&tx, ledger, now, workspace_for_pane)?;
    let staged = stage(directory, &quarantine)?;
    directory.sync_all()?;
    check_identity(directory, SOURCE, source_stamp)?;
    tx.execute(
        "INSERT INTO legacy_dm_import VALUES(1,?1,?2)",
        params![source_stamp, stamp(&staged)?],
    )?;
    tx.execute_batch("DROP TABLE IF EXISTS agent_state;")?;
    tx.commit()?;
    Ok((staged, quarantine))
}

fn import(
    tx: &Transaction<'_>,
    ledger: Ledger,
    now: u64,
    workspace_for_pane: &impl Fn(&str) -> Option<String>,
) -> Result<Quarantine> {
    let newer = v9_ledger(tx)?;
    let mut known_post = tx.prepare("SELECT EXISTS(SELECT 1 FROM posts WHERE id=?1)")?;
    let mut receipt_order = ledger.messages.into_iter().collect::<Vec<_>>();
    receipt_order.sort_unstable_by_key(|(_, message)| message.sequence);
    let mut last = 0;
    let mut retained = Vec::new();
    for (id, message) in receipt_order {
        ensure!(
            id == message.id
                && Uuid::parse_str(&id).is_ok()
                && last < message.sequence
                && message.sequence <= ledger.sequence,
            "invalid legacy message identity"
        );
        last = message.sequence;
        let sender = ledger
            .agents
            .get(&message.from)
            .context("orphaned legacy sender")?;
        let recipient = ledger
            .agents
            .get(&message.to)
            .context("orphaned legacy recipient")?;
        ensure!(
            message.from != message.to
                && Uuid::parse_str(&message.from).is_ok()
                && Uuid::parse_str(&message.to).is_ok(),
            "invalid legacy launch pair"
        );
        if message.purged() || known_post.query_row([&id], |row| row.get::<_, bool>(0))? {
            continue;
        }
        let Some(text) = message.text else { continue };
        ensure!(
            text.len() <= MAX_DIRECT_MESSAGE_BYTES,
            "oversized legacy message body"
        );
        let matching_receipt = newer
            .as_ref()
            .and_then(|snapshot| snapshot.messages.get(&id))
            .filter(|receipt| {
                receipt.id == id
                    && receipt.from == message.from
                    && receipt.to == message.to
                    && receipt.scope == message.scope
                    && receipt.created_at == message.created_at
                    && receipt.sequence == message.sequence
            });
        if matching_receipt.is_some_and(Receipt::purged) {
            continue;
        }
        let corroborated = newer.as_ref().is_none_or(|snapshot| {
            matching_receipt.is_some()
                && [&message.from, &message.to].into_iter().all(|id| {
                    snapshot.agents.get(id).is_some_and(|agent| {
                        let old = &ledger.agents[id];
                        agent.pane_id == old.pane_id && agent.harness == old.harness
                    })
                })
        });
        let newer_retirement = newer
            .as_ref()
            .and_then(|snapshot| snapshot.agents.get(&message.to))
            .filter(|agent| agent.pane_id == recipient.pane_id)
            .and_then(|agent| agent.exited_at);
        let retirement = recipient
            .exited_at
            .into_iter()
            .chain(newer_retirement)
            .min()
            .unwrap_or(now);
        let expires_at = retirement.saturating_add(RETENTION_MS);
        if expires_at <= now {
            continue;
        }
        if corroborated
            && let Some(workspace) =
                owner_workspace(sender, recipient, message.scope, workspace_for_pane)
        {
            archive_post(
                tx,
                &workspace,
                id,
                message.created_at,
                sender.actor(&message.from),
                recipient.actor(&message.to),
                &text,
            )?;
        } else {
            retained.push(RetainedBody { expires_at, text });
        }
    }
    Ok(Quarantine {
        version: 2,
        retained,
    })
}

fn owner_workspace(
    sender: &Enrollment,
    recipient: &Enrollment,
    scope: AgentScope,
    workspace_for_pane: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let owner = workspace_for_pane(&sender.pane_id);
    if scope != AgentScope::Workspace {
        return owner;
    }
    // #84's exact workspace admission established equality of both panes' owners.
    let peer = workspace_for_pane(&recipient.pane_id);
    match (owner, peer) {
        (Some(owner), Some(peer)) if owner != peer => None,
        (owner, peer) => owner.or(peer),
    }
}

fn v9_ledger(tx: &Transaction<'_>) -> Result<Option<Ledger>> {
    if !tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='agent_state')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    let mut statement = tx.prepare("SELECT snapshot FROM agent_state WHERE singleton=1")?;
    let mut rows = statement.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let snapshot = row.get_ref(0)?.as_blob()?;
    ensure!(
        snapshot.len() as u64 <= MAX_STORE_BYTES,
        "legacy SQLite ledger exceeds size limit"
    );
    let document: Document = serde_json::from_slice(snapshot)?;
    document.into_ledger().map(Some)
}

impl Enrollment {
    fn actor(&self, launch: &str) -> Participant {
        Participant {
            // The production auth paths only mint UUIDs (random or derived).
            // A prefixed launch snapshot can never be enrolled or recovered.
            id: ParticipantId(format!("legacy-launch:{launch}")),
            kind: ParticipantKind::Agent,
            display_name: format!("{} (retired {})", self.harness.display_name(), &launch[..8]),
        }
    }
}

fn archive_post(
    tx: &Transaction<'_>,
    workspace: &str,
    id: String,
    timestamp: u64,
    from: Participant,
    to: Participant,
    text: &str,
) -> Result<()> {
    let mut pair = [from.id.as_ref(), to.id.as_ref()];
    pair.sort_unstable();
    let thread = tx.query_row(
        "SELECT c.id,c.last_sequence FROM direct_channels d JOIN channels c ON c.id=d.channel_id
         WHERE d.first_participant=?1 AND d.second_participant=?2",
        pair, |row| Ok((ChannelId(row.get(0)?), row.get::<_, u64>(1)?)),
    ).optional()?;
    let (channel, last) = if let Some(thread) = thread {
        thread
    } else {
        let channel = ChannelId(Uuid::new_v4().to_string());
        insert_direct_channel(tx, &channel, workspace, &from, &to)?;
        (channel, 0)
    };
    let key = format!("\u{1}{id}");
    let message = MessageSummary {
        id: MessageId(id),
        sequence: last
            .checked_add(1)
            .context("legacy direct sequence exhausted")?,
        author: from,
        posted_at_ms: timestamp,
        intent: PostIntent::Note,
        mentions: Vec::new(),
        mention_spans: Vec::new(),
        reply_to: None,
        attachments: Vec::new(),
        source: None,
        role: None,
    };
    insert_post(tx, &channel, &message, &key, &[], text, (0, false))
}

fn decode<T: serde::de::DeserializeOwned>(file: &File) -> Result<T> {
    let mut file = file;
    ensure!(
        file.metadata()?.len() <= MAX_STORE_BYTES,
        "legacy agent ledger exceeds size limit"
    );
    file.rewind()?;
    serde_json::from_reader(BufReader::new(file.take(MAX_STORE_BYTES + 1)))
        .context("decode legacy agent ledger")
}

fn finish(
    directory: &File,
    stamp: &str,
    quarantine: &mut Quarantine,
    now: u64,
) -> Result<Option<u64>> {
    ensure!(quarantine.version == 2, "invalid legacy quarantine version");
    let before = quarantine.retained.len();
    quarantine.retained.retain(|body| body.expires_at > now);
    let next = quarantine.retained.iter().map(|body| body.expires_at).min();
    if next.is_none() {
        unlink(directory, SOURCE, stamp)?;
    } else if before != quarantine.retained.len() {
        let staged = stage(directory, quarantine)?;
        replace(directory, stamp, &staged)?;
    }
    directory.sync_all()?;
    Ok(next)
}

fn stage(directory: &File, quarantine: &Quarantine) -> Result<File> {
    let file = open_at(
        directory,
        STAGED,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?
    .context("create legacy cleanup stage")?;
    let mut writer = BufWriter::new(&file);
    serde_json::to_writer(&mut writer, quarantine)?;
    writer.flush()?;
    drop(writer);
    file.sync_all()?;
    Ok(file)
}

fn stamp(file: &File) -> Result<String> {
    let m = file.metadata()?;
    Ok(format!(
        "{}:{}:{}:{}",
        m.dev(),
        m.ino(),
        m.ctime(),
        m.ctime_nsec()
    ))
}

fn check_identity(directory: &File, name: &CStr, expected: &str) -> Result<()> {
    let file = existing(directory, name)?.context("legacy cleanup file disappeared")?;
    ensure!(
        stamp(&file)? == expected,
        "legacy cleanup file was replaced"
    );
    Ok(())
}

fn replace(directory: &File, expected_source: &str, staged: &File) -> Result<()> {
    check_identity(directory, SOURCE, expected_source)?;
    check_identity(directory, STAGED, &stamp(staged)?)?;
    let fd = directory.as_raw_fd();
    if unsafe { libc::renameat(fd, STAGED.as_ptr(), fd, SOURCE.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn unlink(directory: &File, name: &CStr, expected: &str) -> Result<()> {
    check_identity(directory, name, expected)?;
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn remove_staged(directory: &File) -> Result<()> {
    if let Some(file) = existing(directory, STAGED)? {
        unlink(directory, STAGED, &stamp(&file)?)?;
    }
    Ok(())
}

fn existing(directory: &File, name: &CStr) -> Result<Option<File>> {
    let file = open_at(directory, name, libc::O_RDONLY)?;
    if let Some(file) = &file {
        crate::registry::check_file(file)?;
    }
    Ok(file)
}

fn open_at(directory: &File, name: &CStr, flags: libc::c_int) -> Result<Option<File>> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd >= 0 {
        // SAFETY: openat returned a fresh owned descriptor.
        return Ok(Some(unsafe { File::from_raw_fd(fd) }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error.into())
    }
}
