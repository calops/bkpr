use super::{Store, StoredChannel, provision_channels, take_extra};
use anyhow::{Result, ensure};
use bkpr_protocol::channels::{
    ChannelCatalogChange, ChannelCatalogCursor, ChannelCatalogPage, ChannelId, ChannelResponse,
    ChannelSummary, ParticipantId,
};
use rusqlite::{Connection, Row, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) enum CatalogRead<'a> {
    Snapshot(Option<&'a ChannelCatalogPage>),
    Changes(&'a ChannelCatalogCursor),
}

impl Store {
    pub(in crate::channels) fn sync_catalog(
        &mut self,
        workspaces: BTreeSet<String>,
        actor_id: &str,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE catalog_identity SET actor_id=?1 WHERE actor_id IS NULL",
            [actor_id],
        )?;
        let owner: String =
            tx.query_row("SELECT actor_id FROM catalog_identity", [], |r| r.get(0))?;
        ensure!(
            actor_id == owner,
            "channel catalog belongs to a different session"
        );
        let tracked = {
            let mut statement = tx.prepare("SELECT id FROM catalog_workspaces")?;
            statement
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?
        };
        for id in tracked.difference(&workspaces) {
            tx.execute("DELETE FROM catalog_workspaces WHERE id=?1", [id])?;
        }
        let new = workspaces.difference(&tracked);
        provision_channels(&tx, new.clone().map(String::as_str))?;
        for id in new {
            tx.execute("INSERT INTO catalog_workspaces VALUES(?1)", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(in crate::channels) fn read_catalog(
        &mut self,
        read: CatalogRead<'_>,
        actor_id: &str,
        limit: u16,
    ) -> Result<ChannelResponse> {
        // Read journal bounds, visibility and summaries in one SQLite snapshot.
        let tx = self.connection.transaction()?;
        let (store_id, oldest, newest): (String, u64, u64) = tx.query_row(
            "SELECT store_id, COALESCE((SELECT min(sequence) FROM catalog_events),0),
                              COALESCE((SELECT max(sequence) FROM catalog_events),0)
             FROM catalog_identity WHERE actor_id=?1",
            [actor_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let supplied = match read {
            CatalogRead::Snapshot(after) => after.map(|page| &page.cursor),
            CatalogRead::Changes(after) => Some(after),
        };
        if supplied.is_some_and(|cursor| {
            cursor.store_id != store_id
                || !(oldest.saturating_sub(1)..=newest).contains(&cursor.sequence)
        }) {
            return Ok(ChannelResponse::CatalogReset {});
        }
        match read {
            CatalogRead::Snapshot(after) => snapshot(
                &tx,
                actor_id,
                ChannelCatalogCursor {
                    store_id,
                    sequence: supplied.map_or(newest, |cursor| cursor.sequence),
                },
                after,
                limit,
            ),
            CatalogRead::Changes(after) => {
                changes(&tx, actor_id, store_id, after.sequence, newest, limit)
            }
        }
    }
}

fn snapshot(
    connection: &Connection,
    actor_id: &str,
    cursor: ChannelCatalogCursor,
    after: Option<&ChannelCatalogPage>,
    limit: u16,
) -> Result<ChannelResponse> {
    if after.is_some()
        && connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_events WHERE channel_id IS NULL AND sequence>?1)",
            [cursor.sequence],
            |row| row.get::<_, bool>(0),
        )?
    {
        // A continuation cannot expose a workspace that has since left the live
        // view, nor silently omit one from an otherwise consistent snapshot.
        return Ok(ChannelResponse::CatalogReset {});
    }
    let (workspace, channel) = after.map_or(("", ""), |page| {
        (page.workspace_id.as_str(), page.channel_id.as_ref())
    });
    let mut statement = connection.prepare(
        "SELECT c.id,c.workspace_id,COALESCE(d.display_name,c.name),
                COALESCE(e.old_workspace_wide,c.workspace_wide), COALESCE(e.old_settings,c.settings),
                COALESCE(e.old_last_sequence,c.last_sequence),c.change_sequence,
                COALESCE(e.old_last_attention,c.last_attention),d.channel_id IS NOT NULL,
                COALESCE(e.old_through,p.through,0)
         FROM catalog_workspaces w CROSS JOIN channels c ON c.workspace_id=w.id
         LEFT JOIN direct_channels d ON d.channel_id=c.id
         LEFT JOIN positions p ON p.channel_id=c.id AND p.participant_id=?1
         LEFT JOIN catalog_events e ON e.sequence=(SELECT min(sequence) FROM catalog_events
                                                   WHERE channel_id=c.id AND sequence>?4)
         WHERE w.id>=?2 AND c.id>CASE WHEN w.id=?2 THEN ?3 ELSE '' END
           AND (e.sequence IS NULL OR e.old_last_sequence IS NOT NULL)
         ORDER BY w.id,c.id LIMIT ?5",
    )?;
    let mut channels = statement
        .query_map(
            params![actor_id, workspace, channel, cursor.sequence, limit + 1],
            |r| summary(r, 0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_after = take_extra(&mut channels, limit).then(|| {
        let last = channels.last().unwrap();
        ChannelCatalogPage {
            cursor: cursor.clone(),
            workspace_id: last.workspace_id.clone(),
            channel_id: last.id.clone(),
        }
    });
    Ok(ChannelResponse::CatalogSnapshot {
        channels,
        cursor,
        next_after,
    })
}

fn changes(
    connection: &Connection,
    actor_id: &str,
    store_id: String,
    after: u64,
    newest: u64,
    limit: u16,
) -> Result<ChannelResponse> {
    let mut changes = Vec::new();
    let mut sequence = newest;
    let mut more = false;
    if after < newest {
        let mut statement = connection.prepare(
            "SELECT e.sequence,e.workspace_id,c.id,c.workspace_id,COALESCE(d.display_name,c.name),
                    c.workspace_wide,c.settings,c.last_sequence,c.change_sequence,c.last_attention,
                    d.channel_id IS NOT NULL,COALESCE(p.through,0)
             FROM catalog_events e
             LEFT JOIN catalog_workspaces w ON w.id=e.workspace_id
             LEFT JOIN channels c ON c.id=e.channel_id
             LEFT JOIN direct_channels d ON d.channel_id=c.id
             LEFT JOIN positions p ON p.channel_id=c.id AND p.participant_id=?1
             WHERE e.sequence>?2 AND (e.channel_id IS NULL OR w.id IS NOT NULL)
             ORDER BY e.sequence LIMIT ?3",
        )?;
        let mut rows = statement.query(params![actor_id, after, limit + 1])?;
        while let Some(row) = rows.next()? {
            if changes.len() == usize::from(limit) {
                more = true;
                break;
            }
            sequence = row.get(0)?;
            changes.push(
                if matches!(row.get_ref(2)?, rusqlite::types::ValueRef::Null) {
                    ChannelCatalogChange::WorkspaceClosed {
                        workspace_id: row.get(1)?,
                    }
                } else {
                    ChannelCatalogChange::Upsert {
                        channel: summary(row, 2)?,
                    }
                },
            );
        }
        if !more {
            sequence = newest;
        }
    }
    Ok(ChannelResponse::CatalogChanges {
        cursor: ChannelCatalogCursor { store_id, sequence },
        changes,
        more,
    })
}

pub(super) fn record_read_position(
    tx: &Transaction<'_>,
    id: &ChannelId,
    actor: &ParticipantId,
    previous: u64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO catalog_events(workspace_id,channel_id,old_workspace_wide,old_settings,
                                    old_last_sequence,old_last_attention,old_through)
         SELECT c.workspace_id,c.id,c.workspace_wide,c.settings,c.last_sequence,c.last_attention,?3
         FROM channels c JOIN catalog_identity i ON i.actor_id=?2 WHERE c.id=?1",
        params![id.as_ref(), actor.as_ref(), previous],
    )?;
    Ok(())
}

fn summary(row: &Row<'_>, start: usize) -> rusqlite::Result<ChannelSummary> {
    Ok(StoredChannel::read(row, start + 1)?.summary(
        ChannelId(row.get(start)?),
        row.get(start + 9)?,
        true,
    ))
}
