use super::{
    ChannelAccessLost, ChannelAuthority, ChannelId, MAX_PENDING, MessageId, MessagePage, Store,
    params,
};
use crate::channels::{ChannelClaim, ChannelRecipient, ChannelWakeAccess};
use anyhow::{Context, Result, ensure};
use bkpr_protocol::channels::{ChannelSource, ChannelWake, ParticipantKind};
use rusqlite::{OptionalExtension, Row};
use serde::Serialize;

const MAX_BATCH: u32 = 32;

#[derive(Serialize)]
struct ChannelWakeLookup<'a> {
    #[serde(flatten)]
    wake: &'a ChannelWake,
    messages: &'a [WakeItem],
    #[serde(skip_serializing_if = "Option::is_none")]
    post: Option<WakePost<'a>>,
}

#[derive(Serialize)]
struct WakePost<'a> {
    #[serde(flatten)]
    page: &'a MessagePage<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author_mention_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<MessagePage<'a>>,
}

#[derive(Debug)]
struct WakeLookupDisconnected;

impl std::fmt::Display for WakeLookupDisconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("channel request disconnected")
    }
}

impl std::error::Error for WakeLookupDisconnected {}

fn wake_reply(
    wake: &ChannelWake,
    messages: &[WakeItem],
    post: Option<WakePost<'_>>,
) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(ChannelWakeLookup {
        wake,
        messages,
        post,
    })?)
}

#[derive(Serialize)]
struct WakeItem {
    id: MessageId,
    sequence: u64,
    addressed: bool,
}

impl WakeItem {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: MessageId(row.get(0)?),
            sequence: row.get(1)?,
            addressed: row.get(2)?,
        })
    }
}

impl Store {
    pub(super) fn reclaim_wake_capacity(
        &mut self,
        needed: usize,
        binding_live: &dyn Fn(&str, u64) -> bool,
    ) -> Result<()> {
        let count = |connection: &rusqlite::Connection| -> rusqlite::Result<usize> {
            connection.query_row(
                "SELECT (SELECT count(*) FROM notification_work WHERE pending=1)
                   + (SELECT COALESCE(sum(pending_count),0) FROM channel_batches WHERE outcome IS NULL AND revoked=0)",
                [],
                |row| row.get(0),
            )
        };
        if needed <= MAX_PENDING.saturating_sub(count(&self.connection)?) {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        tx.execute(
            "UPDATE channel_batches SET outcome='unknown' WHERE outcome IS NULL
               AND deadline <= unixepoch('subsec') * 1000",
            [],
        )?;
        {
            let mut bindings = tx.prepare(
                "SELECT launch_id,binding_revision FROM notification_work WHERE pending=1 AND launch_id IS NOT NULL
                 UNION SELECT launch_id,binding_revision FROM channel_batches WHERE outcome IS NULL",
            )?;
            let mut rows = bindings.query([])?;
            let mut retired = Vec::new();
            while let Some(row) = rows.next()? {
                let launch = row.get_ref(0)?.as_str()?;
                let binding = row.get(1)?;
                if !binding_live(launch, binding) {
                    retired.push((launch.to_owned(), binding));
                }
            }
            let mut work = tx.prepare(
                "UPDATE notification_work SET pending=0 WHERE pending=1 AND launch_id=?1 AND binding_revision=?2",
            )?;
            let mut batches = tx.prepare(
                "UPDATE channel_batches SET outcome='unknown' WHERE outcome IS NULL AND launch_id=?1 AND binding_revision=?2",
            )?;
            for entry in retired {
                work.execute(params![entry.0, entry.1])?;
                batches.execute(params![entry.0, entry.1])?;
            }
        }
        let available = MAX_PENDING.saturating_sub(count(&tx)?);
        tx.commit()?;
        ensure!(
            needed <= available,
            "channel notification capacity exceeded"
        );
        Ok(())
    }

    /// Claims the oldest pending channel, superseding only earlier unrevoked work.
    pub(crate) fn claim_wakes(
        &mut self,
        claim: &ChannelClaim<'_>,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        recheck: impl FnOnce() -> Result<()>,
    ) -> Result<Option<ChannelWake>> {
        let ChannelRecipient {
            launch,
            binding,
            participant,
        } = claim.recipient;
        let channel = self
            .connection
            .query_row(
                "SELECT channel_id FROM notification_work
               WHERE pending=1 AND launch_id=?1 AND binding_revision=?2 AND participant_id=?3
               ORDER BY enqueue_ordinal LIMIT 1",
                params![launch, binding, participant],
                |row| Ok(ChannelId(row.get(0)?)),
            )
            .optional()?;
        let Some(channel_id) = channel else {
            return Ok(None);
        };
        self.access(&channel_id, admit)?;
        let tx = self.connection.transaction()?;
        let items = {
            let mut statement = tx.prepare(
                "SELECT message_id,channel_sequence,addressed FROM notification_work
                   WHERE pending=1 AND launch_id=?1 AND binding_revision=?2 AND participant_id=?3 AND channel_id=?4
                   ORDER BY channel_sequence LIMIT ?5",
            )?;
            statement
                .query_map(
                    params![launch, binding, participant, channel_id.as_ref(), MAX_BATCH],
                    WakeItem::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let pending_count = u32::try_from(items.len())?;
        let addressed_message_id = items
            .iter()
            .rev()
            .find(|item| item.addressed)
            .map(|item| item.id.as_ref());
        tx.execute(
            "UPDATE channel_batches SET outcome='unknown' WHERE outcome IS NULL AND revoked=0 AND launch_id=?1",
            [launch],
        )?;
        tx.execute(
            "INSERT INTO channel_batches(id,channel_id,participant_id,launch_id,binding_revision,receiver_id,generation,pending_count,addressed_message_id,deadline)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![claim.batch_id, channel_id.as_ref(), participant, launch, binding, claim.receiver_id,
                claim.generation, pending_count, addressed_message_id, claim.deadline],
        )?;
        {
            let mut statement = tx.prepare(
                "UPDATE notification_work SET pending=0,batch_id=?3 WHERE message_id=?1 AND participant_id=?2 AND pending=1",
            )?;
            for item in &items {
                ensure!(
                    statement.execute(params![item.id.as_ref(), participant, claim.batch_id])? == 1,
                    "channel wake work changed during claim"
                );
            }
        }
        recheck()?;
        tx.commit()?;
        Ok(Some(ChannelWake {
            batch_id: claim.batch_id.to_owned(),
            channel_id,
            pending_count,
            addressed_message_id: addressed_message_id.map(|id| MessageId(id.to_owned())),
        }))
    }

    /// Checks the exact unresolved batch, its deadline, and current recipient channel access.
    pub(crate) fn channel_wake_current(
        &self,
        claim: &ChannelClaim<'_>,
        timestamp: u64,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
    ) -> Result<bool> {
        let recipient = claim.recipient;
        let id = self.connection.query_row(
            "SELECT channel_id FROM channel_batches WHERE id=?1 AND launch_id=?2 AND binding_revision=?3
              AND participant_id=?4 AND receiver_id=?5 AND generation=?6 AND outcome IS NULL AND revoked=0 AND deadline>?7",
            params![claim.batch_id, recipient.launch, recipient.binding, recipient.participant,
                claim.receiver_id, claim.generation, timestamp],
            |row| Ok(ChannelId(row.get(0)?)),
        ).optional()?;
        let Some(id) = id else {
            return Ok(false);
        };
        match self.access(&id, admit) {
            Ok(_) => Ok(true),
            Err(error) if error.is::<ChannelAccessLost>() => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Records one exact native confirmation before its deadline, independently of lookup revocation.
    #[expect(
        clippy::too_many_arguments,
        reason = "Exact receiver and batch settlement fence"
    )]
    pub(crate) fn settle_channel_wake(
        &mut self,
        launch: &str,
        receiver: &str,
        batch: &str,
        generation: u64,
        outcome: &str,
        reason: Option<&str>,
        recheck: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        let tx = self.connection.transaction()?;
        let recorded = tx
            .query_row(
                "UPDATE channel_batches SET
                    outcome=CASE WHEN deadline <= unixepoch('subsec') * 1000 THEN 'unknown' ELSE ?5 END,
                    reason=CASE WHEN deadline <= unixepoch('subsec') * 1000 THEN 'native confirmation deadline elapsed' ELSE ?6 END
                  WHERE id=?1 AND launch_id=?2 AND receiver_id=?3 AND generation=?4 AND outcome IS NULL
                  RETURNING deadline > unixepoch('subsec') * 1000",
                params![batch, launch, receiver, generation, outcome, reason],
                |row| row.get(0),
            )
            .optional()?;
        let Some(recorded) = recorded else {
            return Ok(false);
        };
        recheck()?;
        tx.commit()?;
        Ok(recorded)
    }

    /// Returns the recipient's retained batch and, when requested, bounded authorized body pages.
    pub(crate) fn channel_wake_lookup(
        &self,
        recipient: ChannelRecipient<'_>,
        batch_id: String,
        max_body_bytes: Option<usize>,
        include_source: bool,
        authority: ChannelWakeAccess<'_>,
    ) -> Result<serde_json::Value> {
        let ChannelWakeAccess {
            admit,
            author_mention,
            connected,
        } = authority;
        let access = |workspace: &str| {
            ensure!(connected(), WakeLookupDisconnected);
            admit(workspace)
        };
        let (channel_id, pending_count, addressed_message_id) = self.connection.query_row(
            "SELECT channel_id,pending_count,addressed_message_id FROM channel_batches
               WHERE id=?1 AND launch_id=?2 AND binding_revision=?3 AND participant_id=?4 AND revoked=0",
            params![batch_id, recipient.launch, recipient.binding, recipient.participant],
            |row| Ok((ChannelId(row.get(0)?), row.get(1)?, row.get::<_, Option<String>>(2)?.map(MessageId))),
        ).optional()?.context(ChannelAccessLost)?;
        let (channel, _) = self.access(&channel_id, &access)?;
        let mut statement = self.connection.prepare(
            "SELECT w.message_id,p.sequence,w.addressed FROM notification_work w JOIN posts p ON p.id=w.message_id
               WHERE w.batch_id=?1 ORDER BY p.sequence LIMIT ?2",
        )?;
        let messages = statement
            .query_map(params![batch_id, MAX_BATCH], WakeItem::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let wake = ChannelWake {
            batch_id,
            channel_id,
            pending_count,
            addressed_message_id,
        };
        let selected = wake
            .addressed_message_id
            .as_ref()
            .or(match messages.as_slice() {
                [only] => Some(&only.id),
                _ => None,
            });
        let maximum = max_body_bytes.or(include_source.then_some(2048));
        if let (Some(maximum), Some(id)) = (maximum, selected) {
            return self.with_message_page(&wake.channel_id, id, 0, maximum, |page| {
                let author = &page.message.author;
                let author_mention_id = if author.kind == ParticipantKind::Agent
                    && author.id.as_ref() != recipient.participant
                    && (channel.workspace_wide
                        || self.explicit_member(&wake.channel_id, &author.id)?)
                {
                    ensure!(connected(), WakeLookupDisconnected);
                    author_mention(&channel.workspace_id, &author.id)?
                } else {
                    None
                };
                let project = |source: Option<MessagePage<'_>>| {
                    wake_reply(
                        &wake,
                        &messages,
                        Some(WakePost {
                            page: &page,
                            author_mention_id: author_mention_id.as_deref(),
                            source,
                        }),
                    )
                };
                if let Some(link) = page.message.source.as_ref().filter(|_| include_source) {
                    self.with_wake_source(link, maximum, &access, project)
                } else {
                    project(None)
                }
            });
        }
        wake_reply(&wake, &messages, None)
    }

    fn with_wake_source(
        &self,
        link: &ChannelSource,
        maximum: usize,
        admit: &impl Fn(&str) -> Result<ChannelAuthority>,
        project: impl Fn(Option<MessagePage<'_>>) -> Result<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        match self.access(&link.channel_id, admit) {
            Ok(_) => (),
            Err(error) if error.is::<WakeLookupDisconnected>() => return Err(error),
            Err(error) if error.is::<ChannelAccessLost>() => return project(None),
            Err(error) => return Err(error),
        }
        if let Some(reply) = self.optional_message_page(
            &link.channel_id,
            &link.message_id,
            0,
            maximum,
            |mut page| {
                page.message.source = None;
                project(Some(page))
            },
        )? {
            Ok(reply)
        } else {
            project(None)
        }
    }
}
