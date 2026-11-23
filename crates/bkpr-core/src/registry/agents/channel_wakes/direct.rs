use super::*;
use crate::channels::{ChannelAccessLost, ChannelAuthority, ChannelRoutes, WorkspaceParticipant};
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
        let (response, changed) = self.inner.channels.with_delivery_store(
            &|| !connected() || self.session_end_requested(),
            &|id| self.delivery_workspace_open(id),
            |store| {
                let (workspace, authority) =
                    self.channel_direct_sender(launch, &post, connected)?;
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
                        fence_user_mentions: &|_, _| unreachable!("agent direct post"),
                    },
                    || self.channel_direct_target(launch, &launch_id, &workspace, connected),
                )
            },
        )?;
        if changed {
            self.inner.channels.invalidate();
        }
        channel_tool_reply(Payload::Channels(response))
    }

    fn channel_direct_sender(
        &self,
        caller: &Launch,
        post: &ChannelPost,
        connected: &dyn Fn() -> bool,
    ) -> Result<(String, ChannelAuthority)> {
        let state = self.inner.state.lock().unwrap();
        self.channel_registry_admission(&state, connected)?;
        let pane = channel_pane(&state, &caller.grant.pane_id).context(ChannelAccessLost)?;
        let workspace = pane.workspace_id.as_deref().context(ChannelAccessLost)?;
        ensure!(
            workspace_exists(&state, workspace) && !state.closing_workspaces.contains(workspace),
            ChannelAccessLost
        );
        let agents = self.inner.agents.state.lock().unwrap();
        healthy_agents(&agents)?;
        let captures = self.inner.captures.lock().unwrap();
        let (current, agent) = channel_caller(&state, pane, workspace, caller, &agents, &captures)
            .context(ChannelAccessLost)?;
        let participants = if post
            .mentions
            .iter()
            .any(|mention| mention.as_ref() == self.inner.session.session_id.as_str())
        {
            vec![WorkspaceParticipant {
                actor: self.channel_user(),
                launch_id: None,
                binding_revision: 0,
            }]
        } else {
            Vec::new()
        };
        Ok((
            workspace.to_owned(),
            ChannelAuthority {
                actor: channel_agent(current, agent),
                writable: true,
                workspace_open: true,
                participants,
            },
        ))
    }

    fn channel_direct_target(
        &self,
        caller: &Launch,
        launch_id: &str,
        workspace: &str,
        connected: &dyn Fn() -> bool,
    ) -> Result<WorkspaceParticipant> {
        let state = self.inner.state.lock().unwrap();
        self.channel_registry_admission(&state, connected)?;
        ensure!(
            !state.closing_workspaces.contains(workspace),
            ChannelAccessLost
        );
        let agents = self.inner.agents.state.lock().unwrap();
        healthy_agents(&agents)?;
        let captures = self.inner.captures.lock().unwrap();
        captures.authorize(caller).context(ChannelAccessLost)?;
        let (launch, agent) = captures
            .launches
            .get(launch_id)
            .and_then(|launch| channel_pane(&state, &launch.grant.pane_id))
            .filter(|pane| {
                pane.harness
                    .as_ref()
                    .and_then(|harness| harness.managed.as_ref())
                    .is_some_and(|visible| visible.launch_id == launch_id)
            })
            .and_then(|pane| live_channel_agent(&state, pane, workspace, &agents, &captures, None))
            .context("direct recipient is not a live launch in your workspace")?;
        Ok(WorkspaceParticipant {
            actor: channel_agent(launch, agent),
            launch_id: Some(launch.grant.launch_id.clone()),
            binding_revision: launch.binding_generation,
        })
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
