use crate::{
    observation::{Observer, Publisher},
    transport::Endpoint,
};
use anyhow::{Result, bail, ensure};
use bkpr_protocol::{Operation, Payload, channels::*};
use std::collections::HashMap;

pub(crate) enum Update {
    Snapshot(HashMap<ChannelId, ChannelSummary>),
    Changes(Vec<ChannelCatalogChange>),
    Reset,
}

pub(crate) fn start(endpoint: Endpoint) -> Result<Observer<Update>> {
    Observer::spawn(
        "bkpr-channel-catalog",
        endpoint,
        |endpoint, mut publisher| async move {
            if let Err(error) = observe(&endpoint, &mut publisher).await {
                publisher.send(&endpoint, Err(error)).await;
            }
        },
    )
}

async fn observe(endpoint: &Endpoint, publisher: &mut Publisher<Update>) -> Result<()> {
    let mut cursor = None;
    let mut continuation = None::<ChannelCatalogPage>;
    let mut snapshot = HashMap::new();
    let mut more = false;
    loop {
        let request = match cursor.take() {
            Some(after) => ChannelRequest::CatalogSubscribe {
                after,
                limit: MAX_PAGE_LIMIT,
                wait_ms: if more { 0 } else { MAX_WAIT_MS },
            },
            None => ChannelRequest::CatalogSnapshot {
                after: continuation.clone(),
                limit: MAX_PAGE_LIMIT,
            },
        };
        let changes = matches!(&request, ChannelRequest::CatalogSubscribe { .. });
        let payload = endpoint
            .checked_observation_payload_async(
                Operation::Channels { request },
                &publisher.cancellation,
            )
            .await?;
        match payload {
            Payload::Channels(ChannelResponse::CatalogSnapshot {
                channels,
                cursor: baseline,
                next_after,
            }) if !changes => {
                if let Some(old) = &continuation {
                    ensure!(
                        baseline == old.cursor,
                        "channel catalog snapshot changed baseline"
                    );
                }
                for channel in channels {
                    snapshot.insert(channel.id.clone(), channel);
                }
                if let Some(next) = next_after {
                    ensure!(
                        next.cursor == baseline
                            && continuation.as_ref().is_none_or(|old| (
                                &next.workspace_id,
                                &next.channel_id
                            ) > (
                                &old.workspace_id,
                                &old.channel_id
                            )),
                        "channel catalog page did not advance"
                    );
                    continuation = Some(next);
                } else {
                    continuation = None;
                    cursor = Some(baseline);
                    more = true;
                    if !publisher
                        .send(
                            endpoint,
                            Ok(Update::Snapshot(std::mem::take(&mut snapshot))),
                        )
                        .await
                    {
                        return Ok(());
                    }
                }
            }
            Payload::Channels(ChannelResponse::CatalogChanges {
                cursor: next,
                changes: updates,
                more: pending,
            }) if changes => {
                cursor = Some(next);
                more = pending;
                if !updates.is_empty()
                    && !publisher.send(endpoint, Ok(Update::Changes(updates))).await
                {
                    return Ok(());
                }
            }
            Payload::Channels(ChannelResponse::CatalogReset {}) => {
                continuation = None;
                snapshot.clear();
                if changes && !publisher.send(endpoint, Ok(Update::Reset)).await {
                    return Ok(());
                }
            }
            _ => bail!("unexpected channel catalog response"),
        }
    }
}
