use super::*;
use bkpr_protocol::roles::RoleWake;
use std::collections::VecDeque;

const ROLE_WAKE_PREFIX: &str = "role-";
const MAX_PENDING: usize = 16;

#[derive(Default)]
pub(super) struct RoleLane {
    pending: VecDeque<RoleWake>,
    active: Option<Dispatch>,
    revision: u64,
}

struct Dispatch {
    id: String,
    receiver_id: String,
    binding: u64,
    generation: u64,
    deadline: Instant,
}

impl Dispatch {
    fn matches(&self, receiver: &str, id: &str, generation: u64) -> bool {
        self.id == id && self.receiver_id == receiver && self.generation == generation
    }
}

impl RoleLane {
    pub(super) fn availability(state: &AgentState, launch: &Launch) -> (bool, bool, u64) {
        state
            .role_lanes
            .get(&launch.grant.launch_id)
            .map_or((false, false, 0), |lane| {
                let active = lane.active.as_ref().is_some_and(|active| {
                    active.binding == launch.binding_generation && Instant::now() < active.deadline
                });
                (!lane.pending.is_empty(), active, lane.revision)
            })
    }

    pub(super) fn binding_changed(&mut self) {
        self.active = None;
    }
}

pub(super) fn is_role_wake(id: &str) -> bool {
    id.starts_with(ROLE_WAKE_PREFIX)
}

impl Agents {
    pub(super) fn publish_role_wake(&self, launch_id: &str, wake: RoleWake) {
        let mut state = self.state.lock().unwrap();
        if Self::require_live(&state, launch_id).is_err() {
            return;
        }
        let lane = state.role_lanes.entry(launch_id.to_owned()).or_default();
        if lane.pending.len() == MAX_PENDING {
            lane.pending.pop_front();
            crate::diagnostics::record("roles.native_wake", None, "overflow");
        }
        lane.pending.push_back(wake);
        lane.revision = lane.revision.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(super) fn settle_role_wake(
        &self,
        launch_id: &str,
        receiver: &str,
        id: &str,
        generation: u64,
    ) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(lane) = state.role_lanes.get_mut(launch_id) else {
            return false;
        };
        let Some(active) = lane
            .active
            .as_ref()
            .filter(|a| a.matches(receiver, id, generation))
        else {
            return false;
        };
        let on_time = Instant::now() < active.deadline;
        lane.active = None;
        lane.revision = lane.revision.wrapping_add(1);
        self.changed.notify_all();
        on_time
    }
}

impl Registry {
    pub(super) fn claim_role_wake(
        &self,
        launch: &Launch,
        receiver: &Receiver,
        connected: &dyn Fn() -> bool,
    ) -> Result<Option<Value>> {
        let service = &self.inner.agents;
        let _writer = service.callback_guard()?;
        ensure!(connected(), "subscription transport disconnected");
        self.role_receiver_live(launch, &receiver.id)?;
        let mut state = service.state.lock().unwrap();
        let (pending, active, _) = RoleLane::availability(&state, launch);
        if !pending || active || ChannelLane::availability(&state, launch).1 {
            return Ok(None);
        }
        let launch_id = &launch.grant.launch_id;
        let channel = state.channel_lanes.entry(launch_id.clone()).or_default();
        // Native receivers deduplicate across both notification kinds.
        let generation = channel
            .last_generation
            .checked_add(1)
            .context("notification generation exhausted")?;
        channel.last_generation = generation;
        let lane = state
            .role_lanes
            .get_mut(launch_id)
            .expect("pending role wake");
        let wake = lane.pending.pop_front().expect("pending role wake");
        let id = format!("{ROLE_WAKE_PREFIX}{}", Uuid::new_v4());
        lane.active = Some(Dispatch {
            id: id.clone(),
            receiver_id: receiver.id.clone(),
            generation,
            binding: launch.binding_generation,
            deadline: Instant::now() + Duration::from_millis(WAKE_BATCH_LEASE_MS),
        });
        lane.revision = lane.revision.wrapping_add(1);
        service.changed.notify_all();
        Ok(Some(json!({"notification": {
            "notification_id": id, "generation": generation, "session_id": receiver.session_id,
            "mode": receiver.mode, "message_count": 1, "unread_count": 1, "role_wake": wake,
        }})))
    }

    fn role_receiver_live(&self, launch: &Launch, receiver: &str) -> Result<()> {
        self.with_current_agent_receiver(launch, receiver, |current| {
            ensure!(
                current
                    .lifetime
                    .as_ref()
                    .is_some_and(|life| life.is_alive())
                    && launch.channel_launch_alive(),
                "native role receiver or launch exited"
            );
            Ok(())
        })?
    }

    pub(super) fn role_wake_current(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver: &str,
        id: &str,
        generation: u64,
    ) -> Result<bool> {
        self.agent_receiver_peer(launch, peer, receiver)?;
        let service = &self.inner.agents;
        let _writer = service.callback_guard()?;
        self.role_receiver_live(launch, receiver)?;
        Ok(service
            .state
            .lock()
            .unwrap()
            .role_lanes
            .get(&launch.grant.launch_id)
            .and_then(|lane| lane.active.as_ref())
            .is_some_and(|active| {
                active.binding == launch.binding_generation
                    && active.matches(receiver, id, generation)
                    && Instant::now() < active.deadline
            }))
    }

    pub(super) fn role_wake_outcome(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver: &str,
        id: &str,
        generation: u64,
        outcome: AgentWakeOutcome,
    ) -> Result<Value> {
        self.agent_receiver_peer(launch, peer, receiver)?;
        let reason = match &outcome {
            AgentWakeOutcome::Accepted => None,
            AgentWakeOutcome::Failed { reason } | AgentWakeOutcome::Unknown { reason } => {
                Some(reason.as_str())
            }
        };
        ensure!(
            reason.is_none_or(|reason| reason.len() <= 2048
                && !reason.chars().any(char::is_control)
                && !reason.contains(&launch.grant.token)),
            "invalid safe wake reason"
        );
        let service = &self.inner.agents;
        let _writer = service.callback_guard()?;
        self.role_receiver_live(launch, receiver)?;
        let recorded = service.settle_role_wake(&launch.grant.launch_id, receiver, id, generation);
        if recorded {
            let result = match outcome {
                AgentWakeOutcome::Accepted => "accepted",
                AgentWakeOutcome::Failed { .. } => "failed",
                AgentWakeOutcome::Unknown { .. } => "unknown",
            };
            crate::diagnostics::record("roles.native_wake", None, result);
        }
        Ok(json!({"recorded": recorded}))
    }
}
