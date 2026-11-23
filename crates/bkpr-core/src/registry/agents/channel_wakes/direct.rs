use super::*;
use crate::channels::{ChannelAccessLost, ChannelRoutes, WorkspaceParticipant};
use bkpr_protocol::channels::{ChannelPost, ParticipantId};

impl Registry {
    pub(in crate::registry::agents) fn agent_channel_ack(
        &self,
        launch: &Launch,
        channel_id: ChannelId,
        message_id: MessageId,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        ChannelRequest::Read {
            channel_id: channel_id.clone(),
            message_id: message_id.clone(),
        }
        .validate()?;
        let workspace = self.channel_caller_workspace(launch)?;
        let acknowledged = self.inner.channels.with_delivery_store(
            &|| !connected() || self.session_end_requested(),
            &|id| self.delivery_workspace_open(id),
            |store| {
                store.acknowledge_channel_post(
                    &channel_id,
                    &message_id,
                    recipient(launch)?,
                    &workspace,
                    &|id| {
                        ensure!(id == workspace, ChannelAccessLost);
                        self.channel_authority(id, Some(launch), false, connected)
                    },
                )
            },
        )?;
        Ok(json!({"acknowledged": acknowledged}))
    }

    pub(in crate::registry::agents) fn agent_channel_direct_post(
        &self,
        launch: &Launch,
        launch_id: String,
        post: ChannelPost,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        ensure!(
            Uuid::parse_str(&launch_id).is_ok() && launch_id != launch.grant.launch_id,
            "direct recipient must be another exact live launch ID"
        );
        post.validate()?;
        ensure!(
            !post.text.contains(&launch.grant.token)
                && !post.idempotency_key.contains(&launch.grant.token)
                && post.source.as_ref().is_none_or(|source| {
                    !source.channel_id.as_ref().contains(&launch.grant.token)
                        && !source.message_id.as_ref().contains(&launch.grant.token)
                }),
            "channel post contains a private capability"
        );
        let workspace = self.channel_caller_workspace(launch)?;
        let (response, changed) = self.inner.channels.with_delivery_store(
            &|| !connected() || self.session_end_requested(),
            &|id| self.delivery_workspace_open(id),
            |store| {
                let authority =
                    self.channel_authority(&workspace, Some(launch), true, connected)?;
                store.post_direct(
                    post,
                    authority,
                    &launch_id,
                    &workspace,
                    ChannelRoutes {
                        workspace: Some(&workspace),
                        exact_mentions: Some((&launch.grant.launch_id, launch.binding_generation)),
                        publish: &|targets| self.inner.agents.publish_channel_work(targets),
                        binding_live: &|id, binding| self.channel_binding_live(id, binding),
                        direct_peer: &|_| Ok(None),
                    },
                )
            },
        )?;
        if changed {
            self.inner.channels.invalidate();
        }
        channel_tool_reply(Payload::Channels(response))
    }

    pub(in crate::registry) fn channel_direct_peer(
        &self,
        caller: &Launch,
        participant: &ParticipantId,
        connected: &dyn Fn() -> bool,
    ) -> Result<Option<WorkspaceParticipant>> {
        let state = self.inner.state.lock().unwrap();
        ensure!(connected(), ChannelAccessLost);
        healthy(&state)?;
        let agents = self.inner.agents.state.lock().unwrap();
        let captures = self.inner.captures.lock().unwrap();
        captures.authorize(caller)?;
        let mut matches = captures
            .launches
            .values()
            .filter(|launch| launch.channel_participant.as_ref() == Some(participant));
        let Some(launch) = matches.next() else {
            return Ok(None);
        };
        ensure!(
            matches.next().is_none(),
            "ambiguous live direct participant"
        );
        let current = state
            .record
            .registry
            .panes
            .iter()
            .find(|pane| pane.id == launch.grant.pane_id)
            .and_then(|pane| {
                let workspace = pane.workspace_id.as_deref()?;
                if state.closing_workspaces.contains(workspace) {
                    return None;
                }
                live_channel_agent(
                    &state,
                    pane,
                    workspace,
                    &agents,
                    &captures,
                    Some(participant),
                )
            });
        Ok(current.map(|(launch, agent)| WorkspaceParticipant {
            actor: channel_agent(launch, agent),
            launch_id: Some(launch.grant.launch_id.clone()),
            binding_revision: launch.binding_generation,
        }))
    }
}
