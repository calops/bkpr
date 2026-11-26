use super::*;
use crate::channels::{ChannelClaim, ChannelRecipient, WakeTarget};
use bkpr_protocol::channels::{ChannelId, ChannelRequest, MessageId};

const BATCH_PREFIX: &str = "channel-";
const MCP_PAGE: u16 = 10;

pub(super) fn is_channel_batch(id: &str) -> bool {
    id.starts_with(BATCH_PREFIX)
}

#[derive(Default)]
pub(super) struct ChannelLane {
    pub(super) pending_binding: Option<u64>,
    pub(super) last_generation: u64,
    pub(super) active: Option<ChannelDispatch>,
}

pub(super) struct ChannelDispatch {
    batch_id: String,
    deadline: u64,
    generation: u64,
}

impl ChannelLane {
    pub(super) fn availability(state: &AgentState, launch: &Launch) -> (bool, bool) {
        state
            .channel_lanes
            .get(&launch.grant.launch_id)
            .map_or((false, false), |lane| {
                (
                    lane.pending_binding == Some(launch.binding_generation),
                    lane.active
                        .as_ref()
                        .is_some_and(|active| now() < active.deadline),
                )
            })
    }
}

impl Agents {
    pub(in crate::registry) fn publish_channel_work(&self, targets: &[WakeTarget<'_>]) {
        let mut state = self.state.lock().unwrap();
        let mut changed = false;
        for target in targets {
            if Self::require_live(&state.store, target.launch).is_err() {
                continue;
            }
            let lane = if let Some(lane) = state.channel_lanes.get_mut(target.launch) {
                lane
            } else {
                state
                    .channel_lanes
                    .entry(target.launch.to_owned())
                    .or_default()
            };
            if lane
                .pending_binding
                .is_none_or(|binding| binding < target.binding)
            {
                lane.pending_binding = Some(target.binding);
                changed = true;
            }
        }
        if changed {
            self.changed.notify_all();
        }
    }

    fn release_channel_dispatch(&self, id: &str, batch: &str, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if let Some(lane) = state.channel_lanes.get_mut(id)
            && lane
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation && active.batch_id == batch)
        {
            lane.active = None;
            self.changed.notify_all();
        }
    }
}

fn recipient(launch: &Launch) -> Result<ChannelRecipient<'_>> {
    Ok(ChannelRecipient {
        launch: &launch.grant.launch_id,
        binding: launch.binding_generation,
        participant: launch
            .channel_participant
            .as_ref()
            .context("no live channel participant")?
            .as_ref(),
    })
}

impl Registry {
    pub(in crate::registry) fn channel_caller_workspace(&self, launch: &Launch) -> Result<String> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        state
            .record
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == launch.grant.pane_id)
            .and_then(|pane| pane.workspace_id.clone())
            .context("channel workspace unavailable")
    }

    pub(in crate::registry) fn channel_binding_live(&self, id: &str, binding: u64) -> bool {
        self.inner
            .captures
            .lock()
            .unwrap()
            .launches
            .get(id)
            .is_some_and(|launch| {
                launch.binding_generation == binding
                    && launch.managed.conversation.is_some()
                    && launch.control.is_valid()
            })
    }

    pub(super) fn agent_channel_request(
        &self,
        launch: &Launch,
        request: ChannelRequest,
        connected: &dyn Fn() -> bool,
    ) -> Result<Payload> {
        match &request {
            ChannelRequest::List { limit, .. }
            | ChannelRequest::ListMembers { limit, .. }
            | ChannelRequest::History { limit, .. } => ensure!(
                (1..=MCP_PAGE).contains(limit),
                "channel page limit must be between 1 and 10"
            ),
            ChannelRequest::Post { post, .. } => ensure!(
                !post.text.contains(&launch.grant.token)
                    && !post.idempotency_key.contains(&launch.grant.token),
                "channel post contains a private capability"
            ),
            ChannelRequest::Create { name, .. } => ensure!(
                !name.contains(&launch.grant.token),
                "channel name contains a private capability"
            ),
            ChannelRequest::UploadAttachment { upload, .. } => ensure!(
                upload
                    .alt_text
                    .as_ref()
                    .is_none_or(|text| !text.contains(&launch.grant.token)),
                "channel attachment alt text contains a private capability"
            ),
            ChannelRequest::GetAttachment { .. } => {}
            _ => bail!("unsupported agent channel operation"),
        }
        let attachment = matches!(
            request,
            ChannelRequest::UploadAttachment { .. } | ChannelRequest::GetAttachment { .. }
        );
        let payload =
            self.request_channels_for(request, Some(launch), connected, ChannelProjection::Agent)?;
        if attachment {
            Ok(payload)
        } else {
            payload.into_json().map(Payload::Control)
        }
    }

    pub(super) fn agent_channel_read(
        &self,
        launch: &Launch,
        channel_id: ChannelId,
        message_id: MessageId,
        offset: Option<u32>,
        max_bytes: Option<u32>,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        let maximum = max_bytes.unwrap_or(2048) as usize;
        ensure!(
            (4..=4096).contains(&maximum),
            "channel body page must be between 4 and 4096 bytes"
        );
        let offset = offset.unwrap_or(0) as usize;
        let request = ChannelRequest::Read {
            channel_id,
            message_id,
        };
        self.request_channels_for(
            request,
            Some(launch),
            connected,
            ChannelProjection::MessagePage { offset, maximum },
        )?
        .into_json()
    }

    fn delivery_workspace_open(&self, workspace: &str) -> bool {
        workspace_exists(&self.inner.state.lock().unwrap(), workspace)
    }

    pub(super) fn agent_channel_wake_lookup(
        &self,
        launch: &Launch,
        batch_id: String,
        max_body_bytes: Option<u32>,
        include_source: bool,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        ensure!(
            batch_id
                .strip_prefix(BATCH_PREFIX)
                .is_some_and(|id| id.len() == 36 && Uuid::parse_str(id).is_ok()),
            "channel wake unavailable"
        );
        ensure!(
            max_body_bytes.is_none_or(|maximum| (4..=4096).contains(&maximum)),
            "channel body page must be between 4 and 4096 bytes"
        );
        self.inner.channels.with_delivery_store(
            &|| !connected() || self.session_end_requested(),
            &|workspace| self.delivery_workspace_open(workspace),
            |store| {
                let reply = store.channel_wake_lookup(
                    recipient(launch)?,
                    batch_id,
                    max_body_bytes.map(|maximum| maximum as usize),
                    include_source,
                    crate::channels::ChannelWakeAccess {
                        admit: &|workspace| {
                            self.channel_authority(workspace, Some(launch), false, connected)
                        },
                        author_mention: &|workspace, author| {
                            self.channel_author_mention(workspace, launch, author)
                        },
                        connected,
                    },
                )?;
                ensure!(
                    (!include_source && max_body_bytes.is_none()) || connected(),
                    "channel request disconnected"
                );
                Ok(reply)
            },
        )
    }

    pub(super) fn claim_channel_wake(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver: &Receiver,
        connected: &dyn Fn() -> bool,
    ) -> Result<Option<Value>> {
        let recipient = recipient(launch)?;
        let service = &self.inner.agents;
        let claimed = self.inner.channels.with_delivery_store(
            &|| !connected() || self.session_end_requested(),
            &|workspace| self.delivery_workspace_open(workspace),
            |store| {
                let deadline = wake_deadline(now());
                let batch_id = format!("{BATCH_PREFIX}{}", Uuid::new_v4());
                let generation = {
                    // Reserve the existing native wake slot before SQLite I/O. A slow
                    // channel commit must not hold up DM sends to unrelated launches.
                    let _writer = service.callback_guard()?;
                    self.with_current_agent_receiver(launch, &receiver.id, |_| ())?;
                    let mut state = service.state.lock().unwrap();
                    let (pending, active) = ChannelLane::availability(&state, launch);
                    let agent = &state.store.agents[recipient.launch];
                    if !pending || active || agent.notification.is_some() {
                        return Ok(None);
                    }
                    let last = agent.generation;
                    let lane = state
                        .channel_lanes
                        .get_mut(recipient.launch)
                        .expect("pending channel work");
                    let generation = last
                        .max(lane.last_generation)
                        .checked_add(1)
                        .context("notification generation exhausted")?;
                    lane.last_generation = generation;
                    lane.active = Some(ChannelDispatch {
                        batch_id: batch_id.clone(),
                        deadline,
                        generation,
                    });
                    generation
                };
                let claim = ChannelClaim {
                    recipient,
                    batch_id: &batch_id,
                    receiver_id: &receiver.id,
                    generation,
                    deadline,
                };
                let wake = store.claim_wakes(&claim, &|workspace| {
                    ensure!(connected(), "subscription transport disconnected");
                    self.with_current_agent_receiver(launch, &receiver.id, |_| ())?;
                    self.channel_authority(workspace, Some(launch), false, connected)
                });
                if !matches!(wake, Ok(Some(_))) {
                    service.release_channel_dispatch(recipient.launch, &batch_id, generation);
                    if matches!(wake, Ok(None)) {
                        let mut state = service.state.lock().unwrap();
                        if let Some(lane) = state.channel_lanes.get_mut(recipient.launch)
                            && lane.pending_binding == Some(recipient.binding)
                        {
                            lane.pending_binding = None;
                        }
                        service.changed.notify_all();
                    }
                }
                let wake = wake?;
                Ok(wake.map(|wake| (wake, generation, deadline)))
            },
        )?;
        let Some((wake, generation, deadline)) = claimed else {
            return Ok(None);
        };
        if now() >= deadline
            || !connected()
            || self
                .agent_receiver_peer(launch, peer, &receiver.id)
                .is_err()
        {
            self.channel_subscription_disconnected(
                recipient.launch,
                &receiver.id,
                &wake.batch_id,
                generation,
            )?;
            bail!("channel subscriber changed before wake response");
        }
        Ok(Some(json!({"notification": {
            "notification_id": wake.batch_id, "generation": generation,
            "session_id": receiver.session_id, "mode": receiver.mode,
            "message_count": wake.pending_count, "unread_count": wake.pending_count,
            "channel_wake": wake,
        }})))
    }

    pub(super) fn channel_wake_current(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver_id: &str,
        batch_id: &str,
        generation: u64,
    ) -> Result<bool> {
        let recipient = recipient(launch)?;
        self.agent_receiver_peer(launch, peer, receiver_id)?;
        let current = self.inner.channels.with_delivery_store(
            &|| self.session_end_requested(),
            &|workspace| self.delivery_workspace_open(workspace),
            |store| {
                self.with_current_agent_receiver(launch, receiver_id, |_| ())?;
                let claim = ChannelClaim {
                    recipient,
                    batch_id,
                    receiver_id,
                    generation,
                    deadline: 0,
                };
                store.channel_wake_current(&claim, now(), &|workspace| {
                    self.channel_authority(workspace, Some(launch), false, &|| true)
                })
            },
        )?;
        if !current {
            self.inner.agents.release_channel_dispatch(
                &launch.grant.launch_id,
                batch_id,
                generation,
            );
        }
        Ok(current)
    }

    pub(super) fn channel_wake_outcome(
        &self,
        launch: &Launch,
        peer: &Peer,
        (receiver, batch, generation): (&str, &str, u64),
        outcome: AgentWakeOutcome,
    ) -> Result<Value> {
        let (outcome, reason) = match &outcome {
            AgentWakeOutcome::Accepted => ("accepted", None),
            AgentWakeOutcome::Failed { reason } => ("failed", Some(reason.as_str())),
            AgentWakeOutcome::Unknown { reason } => ("unknown", Some(reason.as_str())),
        };
        ensure!(
            reason.is_none_or(|reason| reason.len() <= 2048
                && !reason.chars().any(char::is_control)
                && !reason.contains(&launch.grant.token)),
            "invalid safe wake reason"
        );
        self.agent_receiver_peer(launch, peer, receiver)?;
        let recorded = self.inner.channels.with_delivery_store(
            &|| self.session_end_requested(),
            &|workspace| self.delivery_workspace_open(workspace),
            |store| {
                self.with_current_agent_receiver(launch, receiver, |_| ())?;
                store.settle_channel_wake(
                    &launch.grant.launch_id,
                    receiver,
                    batch,
                    generation,
                    outcome,
                    reason,
                )
            },
        )?;
        self.inner
            .agents
            .release_channel_dispatch(&launch.grant.launch_id, batch, generation);
        Ok(json!({"recorded": recorded}))
    }

    pub(super) fn channel_subscription_disconnected(
        &self,
        launch: &str,
        receiver: &str,
        batch: &str,
        generation: u64,
    ) -> Result<()> {
        self.inner.channels.with_delivery_store(
            &|| self.session_end_requested(),
            &|workspace| self.delivery_workspace_open(workspace),
            |store| {
                store.settle_channel_wake(
                    launch,
                    receiver,
                    batch,
                    generation,
                    "unknown",
                    Some("subscription transport disconnected before native confirmation"),
                )
            },
        )?;
        self.inner
            .agents
            .release_channel_dispatch(launch, batch, generation);
        Ok(())
    }
}
