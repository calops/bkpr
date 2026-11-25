//! Volatile launch metadata and native channel wake receivers.
use super::*;
use crate::{
    control_lease::ProcessLifetime,
    harness::{Launch, Peer},
    harness_process as proc,
};
use sha2::{Digest, Sha256};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

mod channel_wakes;
use channel_wakes::{ChannelClaimPoll, ChannelLane};

const WAKE_BATCH_LEASE_MS: u64 = 30_000;
const CHANNEL_CLAIM_RETRY: Duration = Duration::from_millis(100);

struct Agent {
    pane_id: String,
    harness: HarnessKind,
    title: Option<String>,
}

#[derive(Clone)]
struct Receiver {
    id: String,
    process: proc::Process,
    lifetime: Option<Arc<ProcessLifetime>>,
    owner: proc::Process,
    session_id: String,
    conversation_generation: u64,
    mode: AgentReceiverMode,
}
struct AgentState {
    agents: BTreeMap<String, Agent>,
    receivers: BTreeMap<String, Receiver>,
    waiters: BTreeMap<String, BTreeMap<String, usize>>,
    channel_lanes: BTreeMap<String, ChannelLane>,
    closing: bool,
    revision: u64,
}
pub(super) struct Agents {
    state: Mutex<AgentState>,
    writer: Mutex<()>,
    changed: Condvar,
    #[cfg(test)]
    request_barrier: Mutex<Option<(bool, Arc<SpawnBarrier>)>>,
    #[cfg(test)]
    channel_barrier: Mutex<Option<(ChannelTestPause, Arc<SpawnBarrier>)>>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChannelTestPause {
    EmptyClaim,
    Validate,
}
#[derive(Clone)]
struct DiscoveryProject {
    name: String,
    branch: Option<String>,
}

fn discovery_project(
    project: Option<&ProjectSnapshot>,
    cwd: Option<&Path>,
    active: bool,
) -> Option<DiscoveryProject> {
    let project = project.filter(|project| {
        active && project.state == ProjectState::Ready && cwd == Some(project.cwd.as_path())
    })?;
    let repository = project.repository.as_ref()?;
    Some(DiscoveryProject {
        name: repository.name.clone(),
        branch: repository.branch.clone(),
    })
}

#[derive(Clone)]
struct ContextFact {
    launch_id: String,
    active: bool,
    pane: PaneInfo,
    workspace_name: Option<String>,
    conversation: Option<NativeConversation>,
    binding_revision: u64,
    cwd: Option<PathBuf>,
    repository: Option<PathBuf>,
    project_summary: Option<DiscoveryProject>,
    registry_revision: u64,
    workspace_revision: Option<u64>,
    window_revision: Option<u64>,
    pane_revision: Option<u64>,
    first_prompt_submitted_at_ms: Option<u64>,
    status: Value,
}
#[derive(Default)]
struct ContextSelection<'a> {
    all: bool,
    project_summary: bool,
    launch_id: Option<&'a str>,
    pane_id: Option<&'a str>,
}
struct DiscoveryQuery<'a> {
    scope: AgentScope,
    detail: bool,
    launch_id: Option<&'a str>,
    pane_id: Option<&'a str>,
    after: Option<&'a str>,
    limit: Option<usize>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn healthy_agents(state: &AgentState) -> Result<()> {
    ensure!(!state.closing, "agent service is shutting down");
    Ok(())
}
impl Agents {
    #[cfg(test)]
    pub(super) fn pause_request(&self, mutation: bool, barrier: Arc<SpawnBarrier>) {
        *self.request_barrier.lock().unwrap() = Some((mutation, barrier));
    }
    #[cfg(test)]
    fn wait_request_barrier(&self, mutation: bool) {
        let barrier = {
            let mut pending = self.request_barrier.lock().unwrap();
            if pending
                .as_ref()
                .is_some_and(|(phase, _)| *phase == mutation)
            {
                pending.take().map(|(_, barrier)| barrier)
            } else {
                None
            }
        };
        if let Some(barrier) = barrier {
            let _ = barrier.entered.send(());
            let _ = barrier.resume.lock().unwrap().recv();
        }
    }
    #[cfg(test)]
    pub(super) fn pause_channel(&self, phase: ChannelTestPause, barrier: Arc<SpawnBarrier>) {
        *self.channel_barrier.lock().unwrap() = Some((phase, barrier));
    }

    #[cfg(test)]
    fn wait_channel_barrier(&self, phase: ChannelTestPause) {
        let barrier = {
            let mut pending = self.channel_barrier.lock().unwrap();
            if pending.as_ref().is_some_and(|(at, _)| *at == phase) {
                pending.take().map(|(_, barrier)| barrier)
            } else {
                None
            }
        };
        if let Some(barrier) = barrier {
            let _ = barrier.entered.send(());
            let _ = barrier.resume.lock().unwrap().recv();
        }
    }
    pub(super) fn check_health(&self) -> Result<()> {
        healthy_agents(&self.state.lock().unwrap())
    }

    pub(super) fn callback_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        let guard = self.writer.lock().unwrap();
        self.check_health()?;
        Ok(guard)
    }

    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(AgentState {
                agents: BTreeMap::new(),
                receivers: BTreeMap::new(),
                waiters: BTreeMap::new(),
                channel_lanes: BTreeMap::new(),
                closing: false,
                revision: 0,
            }),
            writer: Mutex::new(()),
            changed: Condvar::new(),
            #[cfg(test)]
            request_barrier: Mutex::new(None),
            #[cfg(test)]
            channel_barrier: Mutex::new(None),
        }
    }

    fn register(&self, launch: &Launch) -> Result<()> {
        let _writer = self.callback_guard()?;
        let mut state = self.state.lock().unwrap();
        let std::collections::btree_map::Entry::Vacant(entry) =
            state.agents.entry(launch.grant.launch_id.clone())
        else {
            bail!("agent launch already exists");
        };
        entry.insert(Agent {
            pane_id: launch.grant.pane_id.clone(),
            harness: launch.grant.kind,
            title: None,
        });
        Ok(())
    }

    fn retire(&self, ids: &[String]) {
        let _writer = self.writer.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let mut retired = false;
        for id in ids {
            retired |= state.agents.remove(id).is_some();
            state.receivers.remove(id);
            state.channel_lanes.remove(id);
        }
        if retired {
            state.revision = state.revision.wrapping_add(1);
            self.changed.notify_all();
        }
    }

    pub(super) fn retire_pane(&self, pane_id: &str) {
        let _writer = self.writer.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let AgentState {
            agents,
            receivers,
            channel_lanes,
            ..
        } = &mut *state;
        let mut retired = false;
        agents.retain(|id, agent| {
            if agent.pane_id != pane_id {
                return true;
            }
            receivers.remove(id);
            channel_lanes.remove(id);
            retired = true;
            false
        });
        if retired {
            state.revision = state.revision.wrapping_add(1);
            self.changed.notify_all();
        }
    }

    pub(super) fn shutdown(&self) {
        let _writer = self.writer.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        state.closing = true;
        state.agents.clear();
        state.receivers.clear();
        state.channel_lanes.clear();
        self.changed.notify_all();
    }

    fn require_live(state: &AgentState, id: &str) -> Result<()> {
        ensure!(state.agents.contains_key(id), "agent identity is retired");
        Ok(())
    }
}
fn in_scope(scope: AgentScope, sender: &ContextFact, recipient: &ContextFact) -> bool {
    match scope {
        AgentScope::All => true,
        AgentScope::Repository => sender
            .repository
            .as_ref()
            .is_some_and(|r| recipient.repository.as_ref() == Some(r)),
        AgentScope::Session => sender.active && recipient.active,
        AgentScope::Workspace => {
            sender.active
                && recipient.active
                && sender.pane.workspace_id.is_some()
                && sender.pane.workspace_id == recipient.pane.workspace_id
        }
    }
}
fn current_subscribers(state: &AgentState, launch_id: &str) -> usize {
    state
        .receivers
        .get(launch_id)
        .and_then(|receiver| state.waiters.get(launch_id)?.get(&receiver.id))
        .copied()
        .unwrap_or(0)
}
// Bound encoded JSON bytes, not only UTF-8 source bytes (C0 escapes expand sixfold).
fn preview_end(text: &str, encoded_bytes: usize) -> usize {
    let mut used = 2;
    for (offset, ch) in text.char_indices() {
        let bytes = match ch {
            '"' | '\\' | '\u{08}' | '\t' | '\n' | '\u{0c}' | '\r' => 2,
            '\0'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if used + bytes > encoded_bytes {
            return offset;
        }
        used += bytes;
    }
    text.len()
}

fn status_summary(status: Option<&bkpr_protocol::ProgramStatus>) -> Value {
    let Some(status) = status else {
        return Value::Null;
    };
    let mut truncated = Vec::new();
    let mut preview = |field, text: &str, budget| {
        let end = preview_end(text, budget);
        if end < text.len() {
            truncated.push(field);
        }
        json!(&text[..end])
    };
    let mut value = json!({
        "state":preview("state", &status.state, 24),
        "app":status.app.as_ref().map(|app| preview("app", app, 32)),
        "id":preview("id", &status.id, 64),
        "progress":status.progress,
    });
    if !truncated.is_empty() {
        value["truncated_fields"] = json!(truncated);
    }
    value
}
fn public(
    fact: &ContextFact,
    agent: &Agent,
    receiver: Option<&Receiver>,
    subscribers: usize,
    detail: bool,
) -> Value {
    let ready = receiver.is_some() && subscribers > 0;
    let mut value = json!({
        "launch_id":fact.launch_id,"pane_id":fact.pane.id,
        "conversation":fact.conversation,
        "workspace_id":fact.pane.workspace_id,"window_id":fact.pane.window_id,
        "registry_revision":fact.registry_revision,
        "workspace_revision":fact.workspace_revision,"window_revision":fact.window_revision,
        "pane_revision":fact.pane_revision,"harness":agent.harness.as_str(),
        "status":fact.status,
        "status_changed_at_ms":fact.pane.harness_status_changed_at_ms,
        "first_prompt_submitted_at_ms":fact.first_prompt_submitted_at_ms,
        "notification_ready":ready,"attached":fact.active
    });
    if detail {
        value["status"] = json!(fact.pane.harness_status);
        value["title"] = json!(agent.title);
        value["pane_name"] = json!(fact.pane.title);
        value["workspace_name"] = json!(fact.workspace_name);
        value["cwd"] = json!(fact.cwd.as_deref().map(path_wire::WirePath));
        value["receiver_mode"] = json!(receiver.map(|r| r.mode));
        value["project"] = serde_json::to_value(
            fact.pane
                .project
                .as_ref()
                .filter(|project| fact.active && fact.cwd.as_ref() == Some(&project.cwd)),
        )
        .expect("serializable project snapshot");
    }
    value
}
fn discover(
    state: &AgentState,
    contexts: &BTreeMap<String, ContextFact>,
    own: &ContextFact,
    query: DiscoveryQuery<'_>,
) -> Result<Value> {
    require_scope(query.scope, own)?;
    let limit = query.limit.unwrap_or(10);
    ensure!((1..=10).contains(&limit), "limit must be between 1 and 10");
    let mut candidates = contexts.values().filter(|other| {
        other.launch_id != own.launch_id
            && other.active
            && state.agents.contains_key(&other.launch_id)
            && in_scope(query.scope, own, other)
            && query.launch_id.is_none_or(|id| other.launch_id == id)
            && query.pane_id.is_none_or(|id| other.pane.id == id)
            && query
                .after
                .is_none_or(|after| other.launch_id.as_str() > after)
    });
    let page = candidates.by_ref().take(limit).collect::<Vec<_>>();
    let has_more = candidates.next().is_some();
    let values = page
        .iter()
        .map(|other| {
            let agent = &state.agents[&other.launch_id];
            let ready = state.receivers.contains_key(&other.launch_id)
                && current_subscribers(state, &other.launch_id) > 0;
            if query.detail {
                public(
                    other,
                    agent,
                    state.receivers.get(&other.launch_id),
                    current_subscribers(state, &other.launch_id),
                    true,
                )
            } else {
                let mut value = json!({
                    "launch_id":other.launch_id,"pane_id":other.pane.id,
                    "harness":agent.harness.as_str(),
                    "status":other.status.get("state").map(|state| json!({"state":state})),
                    "notification_ready":ready,
                });
                if let Some(title) = &agent.title {
                    value["title"] = json!(title);
                }
                if let Some(cwd) = &other.cwd {
                    value["cwd"] = json!(path_wire::WirePath(cwd));
                }
                if let Some(project) = &other.project_summary {
                    value["project_name"] = json!(project.name);
                    value["branch"] = json!(project.branch);
                }
                value
            }
        })
        .collect::<Vec<_>>();
    let mut value = json!({"agents":values,"scope":query.scope,"has_more":has_more});
    if has_more {
        value["next_cursor"] = json!(page.last().map(|other| &other.launch_id));
    }
    Ok(value)
}
fn live_channel_agent<'a>(
    state: &State,
    pane: &PaneInfo,
    workspace: &str,
    agents: &'a AgentState,
    captures: &'a crate::harness::Captures,
    author: Option<&channels::ParticipantId>,
) -> Option<(&'a Launch, &'a Agent)> {
    if pane.workspace_id.as_deref() != Some(workspace)
        || pane.status != PaneStatus::Running
        || state.closing_panes.contains(&pane.id)
        || state.closing_document_panes.contains(&pane.id)
        || state.live.get(&pane.id).is_none_or(|pane| {
            pane.shared
                .finished
                .load(std::sync::atomic::Ordering::Acquire)
        })
    {
        return None;
    }
    let visible = pane.harness.as_ref()?.managed.as_ref()?;
    let launch = captures.launches.get(&visible.launch_id)?;
    let participant = launch.channel_participant.as_ref()?;
    if author.is_some_and(|author| participant != author)
        || launch.grant.pane_id != pane.id
        || visible.conversation.is_none()
        || visible.conversation != launch.managed.conversation
    {
        return None;
    }
    let agent = agents.agents.get(&visible.launch_id)?;
    launch.channel_launch_alive().then_some((launch, agent))
}

fn channel_pane<'a>(state: &'a State, id: &str) -> Option<&'a PaneInfo> {
    state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == id)
}

fn channel_caller<'a>(
    state: &State,
    pane: &PaneInfo,
    workspace: &str,
    caller: &Launch,
    agents: &'a AgentState,
    captures: &'a crate::harness::Captures,
) -> Result<(&'a Launch, &'a Agent)> {
    captures.authorize(caller)?;
    let (current, agent) = live_channel_agent(state, pane, workspace, agents, captures, None)
        .context("no bound participant in this workspace")?;
    ensure!(
        current.grant.launch_id == caller.grant.launch_id
            && current.binding_generation == caller.binding_generation
            && current.channel_participant == caller.channel_participant
            && current.control.same_authority(&caller.control),
        "channel participant binding changed"
    );
    Ok((current, agent))
}

pub(super) fn unbound_channel_participant(
    launch_id: &str,
    revision: u64,
) -> channels::ParticipantId {
    let mut digest = Sha256::new();
    digest.update(b"bkpr unbound channel participant");
    digest.update(launch_id.as_bytes());
    digest.update(revision.to_le_bytes());
    let bytes = digest.finalize()[..16].try_into().unwrap();
    channels::ParticipantId(Uuid::from_bytes(bytes).to_string())
}

fn channel_agent(
    launch: &Launch,
    agent: &Agent,
    state: &State,
    pane: &PaneInfo,
) -> channels::Participant {
    channel_actor(
        launch
            .channel_participant
            .clone()
            .expect("live channel identity"),
        agent,
        state,
        pane,
    )
}

fn channel_actor(
    id: channels::ParticipantId,
    agent: &Agent,
    state: &State,
    pane: &PaneInfo,
) -> channels::Participant {
    let explicit_pane =
        (!state.record.automatic_titles.contains(&pane.id)).then_some(pane.title.as_str());
    let display_name = explicit_pane
        .into_iter()
        .chain(agent.title.as_deref())
        .map(str::trim)
        .find(|title| {
            !title.is_empty()
                && title.len() <= channels::MAX_NAME_BYTES
                && !title.chars().any(char::is_control)
        })
        .map_or_else(
            || {
                let id = id.as_ref();
                let end = id.char_indices().nth(6).map_or(id.len(), |(at, _)| at);
                format!("{}·{}", agent.harness.display_name(), &id[..end])
            },
            str::to_owned,
        );
    channels::Participant {
        id,
        kind: channels::ParticipantKind::Agent,
        display_name,
    }
}

impl Registry {
    fn channel_user(&self) -> channels::Participant {
        channels::Participant {
            id: channels::ParticipantId(self.inner.session.session_id.clone()),
            kind: channels::ParticipantKind::User,
            display_name: "User".into(),
        }
    }

    fn channel_registry_admission(
        &self,
        state: &State,
        connected: &dyn Fn() -> bool,
    ) -> Result<()> {
        ensure!(connected(), crate::channels::ChannelAccessLost);
        healthy(state)?;
        ensure!(
            !state.closing && !self.session_end_requested(),
            SessionClosing("session is shutting down")
        );
        Ok(())
    }

    // `caller` is an OS-validated capture with a request-local binding witness.
    // Recheck that generation under the store's admission before any channel access.
    pub(super) fn channel_authority(
        &self,
        workspace: &str,
        caller: Option<&Launch>,
        with_roster: bool,
        connected: &dyn Fn() -> bool,
    ) -> Result<crate::channels::ChannelAuthority> {
        use crate::channels::{ChannelAccessLost, ChannelAuthority, WorkspaceParticipant};

        let state = self.inner.state.lock().unwrap();
        self.channel_registry_admission(&state, connected)?;
        let workspace_open = workspace_exists(&state, workspace);
        let writable = workspace_open && !state.closing_workspaces.contains(workspace);
        if caller.is_none() && (!with_roster || !workspace_open) {
            return Ok(ChannelAuthority {
                actor: self.channel_user(),
                writable,
                workspace_open,
                participants: Vec::new(),
            });
        }

        let agents = self.inner.agents.state.lock().unwrap();
        if caller.is_some() {
            healthy_agents(&agents)?;
        }
        let captures = self.inner.captures.lock().unwrap();
        let actor = if let Some(caller) = caller {
            ensure!(writable, ChannelAccessLost);
            let pane = channel_pane(&state, &caller.grant.pane_id).context(ChannelAccessLost)?;
            let (current, agent) =
                channel_caller(&state, pane, workspace, caller, &agents, &captures)
                    .context(ChannelAccessLost)?;
            channel_agent(current, agent, &state, pane)
        } else {
            self.channel_user()
        };
        let mut participants = Vec::new();
        if with_roster {
            if caller.is_some() {
                participants.push(WorkspaceParticipant {
                    actor: self.channel_user(),
                    launch_id: None,
                    binding_revision: 0,
                });
            }
            if healthy_agents(&agents).is_ok() {
                participants.extend(
                    state
                        .record
                        .registry
                        .panes
                        .iter()
                        .filter_map(|pane| {
                            live_channel_agent(&state, pane, workspace, &agents, &captures, None)
                                .map(|(launch, agent)| (launch, agent, pane))
                        })
                        .filter(|(launch, _, _)| {
                            launch.channel_participant.as_ref() != Some(&actor.id)
                        })
                        .map(|(launch, agent, pane)| WorkspaceParticipant {
                            actor: channel_agent(launch, agent, &state, pane),
                            launch_id: Some(launch.grant.launch_id.clone()),
                            binding_revision: launch.binding_generation,
                        }),
                );
            }
        }
        Ok(ChannelAuthority {
            actor,
            writable,
            workspace_open,
            participants,
        })
    }

    fn channel_author_mention(
        &self,
        workspace: &str,
        caller: &Launch,
        author: &channels::ParticipantId,
    ) -> Result<Option<String>> {
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(
            !state.closing
                && !self.session_end_requested()
                && !state.closing_workspaces.contains(workspace),
            "channel workspace unavailable"
        );
        let agents = self.inner.agents.state.lock().unwrap();
        healthy_agents(&agents)?;
        let captures = self.inner.captures.lock().unwrap();
        let pane = channel_pane(&state, &caller.grant.pane_id)
            .context("no bound participant in this workspace")?;
        channel_caller(&state, pane, workspace, caller, &agents, &captures)?;
        let mut eligible = state
            .record
            .registry
            .panes
            .iter()
            .filter_map(|pane| {
                live_channel_agent(&state, pane, workspace, &agents, &captures, Some(author))
            })
            .map(|(launch, _)| launch);
        Ok(match (eligible.next(), eligible.next()) {
            (Some(launch), None) if launch.binding_generation != 0 => {
                Some(crate::channels::agent_mention_id(
                    &launch.grant.launch_id,
                    launch.binding_generation,
                ))
            }
            _ => None,
        })
    }

    pub(super) fn fence_user_mentions(
        &self,
        workspace: &str,
        bindings: &[channels::ChannelMentionBinding],
    ) -> Result<std::sync::MutexGuard<'_, ()>> {
        use crate::channels::STALE_USER_MENTION;

        let transaction = self.inner.transaction.lock().unwrap();
        let writer = self.inner.agents.callback_guard()?;
        {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(
                !state.closing
                    && !self.session_end_requested()
                    && workspace_exists(&state, workspace)
                    && !state.closing_workspaces.contains(workspace),
                STALE_USER_MENTION
            );
            let agents = self.inner.agents.state.lock().unwrap();
            let captures = self.inner.captures.lock().unwrap();
            for binding in bindings {
                let launch = captures
                    .launches
                    .get(&binding.launch_id)
                    .context(STALE_USER_MENTION)?;
                let current = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|pane| pane.id == launch.grant.pane_id)
                    .and_then(|pane| {
                        live_channel_agent(&state, pane, workspace, &agents, &captures)
                    })
                    .map(|(launch, _)| launch)
                    .context(STALE_USER_MENTION)?;
                ensure!(
                    std::ptr::eq(current, launch)
                        && current.binding_generation == binding.binding_generation
                        && current.channel_participant.as_ref() == Some(&binding.participant_id),
                    STALE_USER_MENTION
                );
            }
        }
        // The admission point is ordered with registry changes. Hold only the
        // existing binding writer across the SQLite commit, not registry locks.
        drop(transaction);
        Ok(writer)
    }
    pub(crate) fn agent_subscription_disconnected(
        &self,
        launch_id: &str,
        receiver_id: &str,
        notification_id: &str,
        generation: u64,
    ) -> Result<()> {
        self.channel_subscription_disconnected(launch_id, receiver_id, notification_id, generation)
    }
    fn agent_contexts(
        &self,
        resolve_repository: bool,
        own_id: &str,
        selection: ContextSelection<'_>,
        detail: bool,
    ) -> Result<BTreeMap<String, ContextFact>> {
        let launches = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .values()
            .filter(|l| {
                l.grant.launch_id == own_id
                    || (selection.all
                        && selection.launch_id.is_none_or(|id| l.grant.launch_id == id)
                        && selection.pane_id.is_none_or(|id| l.grant.pane_id == id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut observed = Vec::with_capacity(launches.len());
        let table = proc::fresh_snapshot()?;
        for launch in launches {
            if let Ok((active, cwd)) = super::capture::foreground_context_in(&launch, &table) {
                observed.push((launch, active, cwd));
            }
        }
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(!state.closing, "daemon shutting down");
        Ok(observed
            .into_iter()
            .filter_map(|(launch, active, cwd)| {
                let original =
                    state.record.registry.panes.iter().find(|p| {
                        p.id == launch.grant.pane_id && p.status == PaneStatus::Running
                    })?;
                let pane = PaneInfo {
                    id: original.id.clone(),
                    workspace_id: original.workspace_id.clone(),
                    window_id: original.window_id.clone(),
                    title: if detail {
                        original.title.clone()
                    } else {
                        String::new()
                    },
                    status: original.status,
                    exit_code: original.exit_code,
                    cwd: if detail {
                        original.cwd.clone()
                    } else {
                        PathBuf::new()
                    },
                    foreground_cwd: if detail {
                        original.foreground_cwd.clone()
                    } else {
                        None
                    },
                    harness: if detail {
                        original.harness.clone()
                    } else {
                        None
                    },
                    harness_status: if detail {
                        original.harness_status.clone()
                    } else {
                        None
                    },
                    harness_status_changed_at_ms: original.harness_status_changed_at_ms,
                    project: if detail {
                        original.project.clone()
                    } else {
                        None
                    },
                    home: if detail {
                        original.home.clone()
                    } else {
                        PathBuf::new()
                    },
                    isolated_environment: original.isolated_environment,
                };
                let repository = resolve_repository
                    .then(|| {
                        original
                            .project
                            .as_ref()
                            .filter(|project| {
                                active
                                    && project.state == ProjectState::Ready
                                    && cwd.as_ref() == Some(&project.cwd)
                            })
                            .and_then(|project| project.repository.as_ref())
                            .map(|repository| repository.common_dir.clone())
                    })
                    .flatten();
                let workspace = pane.workspace_id.as_ref().and_then(|id| {
                    state
                        .record
                        .registry
                        .workspaces
                        .iter()
                        .find(|w| &w.id == id)
                });
                let workspace_name = detail.then(|| workspace.map(|w| w.name.clone())).flatten();
                let workspace_revision = workspace.map(|w| w.revision);
                let window_revision = workspace
                    .and_then(|w| {
                        pane.window_id
                            .as_ref()
                            .and_then(|id| w.windows.iter().find(|window| &window.id == id))
                    })
                    .map(|window| window.revision);
                let pane_revision = state
                    .live
                    .get(&pane.id)
                    .map(|pane| pane.shared.published.lock().unwrap().revision);
                let project_summary = (selection.project_summary
                    && launch.grant.launch_id != own_id)
                    .then(|| discovery_project(original.project.as_ref(), cwd.as_deref(), active))
                    .flatten();
                let id = launch.grant.launch_id;
                Some((
                    id.clone(),
                    ContextFact {
                        launch_id: id,
                        active,
                        pane,
                        workspace_name,
                        conversation: launch.managed.conversation,
                        binding_revision: launch.binding_generation,
                        cwd,
                        repository,
                        project_summary,
                        registry_revision: state.record.registry.revision,
                        workspace_revision,
                        window_revision,
                        pane_revision,
                        first_prompt_submitted_at_ms: launch.managed.first_prompt_submitted_at_ms,
                        status: status_summary(original.harness_status.as_ref()),
                    },
                ))
            })
            .collect())
    }
    pub(super) fn agent_register(&self, launch: &Launch, _cwd: &Path) -> Result<()> {
        self.inner.agents.register(launch)
    }
    pub(super) fn agent_retire(&self, ids: &[String]) {
        self.inner.agents.retire(ids);
    }
    pub(super) fn agent_capture_event(
        &self,
        launch_id: &str,
        token: &str,
        event: CaptureEvent,
    ) -> Result<()> {
        let service = &self.inner.agents;
        let _writer = service.callback_guard()?;
        let mut captures = self.inner.captures.lock().unwrap();
        let before = captures
            .launches
            .get(launch_id)
            .map(|launch| launch.binding_generation);
        captures.event(launch_id, token, event)?;
        let after = captures
            .launches
            .get(launch_id)
            .map(|launch| launch.binding_generation);
        drop(captures);
        if before != after {
            let mut state = service.state.lock().unwrap();
            state.receivers.remove(launch_id);
            if let Some(lane) = state.channel_lanes.get_mut(launch_id) {
                lane.pending_binding = None;
                lane.active = None;
                lane.revision = lane.revision.wrapping_add(1);
            }
            state.revision = state.revision.wrapping_add(1);
            service.changed.notify_all();
        }
        Ok(())
    }
    // Caller owns the registry transaction, which also serializes authenticated
    // End. Recheck both launch capability and launch retirement at admission.
    pub(super) fn authorize_resume_origin(&self, launch: &Launch) -> Result<()> {
        self.capture_pane(&launch.grant.pane_id)?;
        ensure!(
            launch.control.is_valid(),
            "native conversation control binding changed"
        );
        self.authorize_agent_guard(launch)?;
        let state = self.inner.agents.state.lock().unwrap();
        healthy_agents(&state)?;
        Agents::require_live(&state, &launch.grant.launch_id)
    }
    fn authorize_agent_guard(&self, launch: &Launch) -> Result<()> {
        self.inner.captures.lock().unwrap().authorize(launch)
    }
    // The state lock fixes the launch snapshot; captures supplies the
    // binding's linearization point. Capture writers never retain captures while
    // waiting on agent state, so this order cannot invert their writer path.
    fn agent_request_state(
        &self,
        launch: &Launch,
    ) -> Result<std::sync::MutexGuard<'_, AgentState>> {
        let state = self.inner.agents.state.lock().unwrap();
        healthy_agents(&state)?;
        Agents::require_live(&state, &launch.grant.launch_id)?;
        self.authorize_agent_guard(launch)?;
        Ok(state)
    }
    pub(super) fn agent_request(
        &self,
        mut launch: Launch,
        peer: &Peer,
        request: AgentRequest,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Payload> {
        crate::harness::validate_callback(peer, &launch)?;
        self.capture_pane(&launch.grant.pane_id)?;
        self.inner.captures.lock().unwrap().admit(&mut launch)?;
        drop(self.agent_request_state(&launch)?);
        #[cfg(test)]
        self.inner.agents.wait_request_barrier(false);
        let request = match request {
            AgentRequest::Channels { request } => {
                return self.agent_channel_request(&launch, *request, is_connected);
            }
            request => request,
        };
        self.agent_request_value(launch, peer, request, is_connected)
            .map(Payload::Control)
    }

    fn agent_request_value(
        &self,
        launch: Launch,
        peer: &Peer,
        request: AgentRequest,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        let id = &launch.grant.launch_id;
        match request {
            AgentRequest::ChannelDirectPost { launch_id, post } => {
                self.agent_channel_direct_post(&launch, launch_id, *post, is_connected)
            }
            AgentRequest::ChannelAck {
                channel_id,
                message_id,
            } => self.agent_channel_ack(&launch, channel_id, message_id, is_connected),
            AgentRequest::ChannelRead {
                channel_id,
                message_id,
                offset,
                max_bytes,
            } => self.agent_channel_read(
                &launch,
                channel_id,
                message_id,
                offset,
                max_bytes,
                is_connected,
            ),
            AgentRequest::ChannelWake {
                batch_id,
                max_body_bytes,
                include_source,
            } => self.agent_channel_wake_lookup(
                &launch,
                batch_id,
                max_body_bytes,
                include_source,
                is_connected,
            ),
            AgentRequest::Control {
                request,
                scope,
                reply,
            } => {
                bkpr_protocol::control::validate_native(&request)?;
                ensure!(
                    scope != AgentScope::Repository,
                    "repository-scoped control is not supported; explicitly select workspace or session scope"
                );
                let contexts =
                    self.agent_contexts(false, id, ContextSelection::default(), false)?;
                let own = contexts.get(id).context("live agent context unavailable")?;
                ensure!(own.active, "live foreground agent context unavailable");
                {
                    let _state = self.agent_request_state(&launch)?;
                    require_scope(scope, own)?;
                }
                let workspace = if scope == AgentScope::Workspace {
                    Some(
                        own.pane
                            .workspace_id
                            .clone()
                            .context("live foreground workspace context unavailable")?,
                    )
                } else {
                    None
                };
                match *request {
                    Operation::RecoveryList => {
                        let _transaction = self.inner.transaction.lock().unwrap();
                        self.authorize_resume_origin(&launch)?;
                        let recoveries = self
                            .recovery_candidates()?
                            .into_iter()
                            .filter(|candidate| {
                                workspace
                                    .as_ref()
                                    .is_none_or(|id| candidate.workspace_id.as_ref() == Some(id))
                            })
                            .collect::<Vec<_>>();
                        Ok(
                            json!({"policy":"current_native_configuration","recoveries":recoveries,"scope":scope}),
                        )
                    }
                    Operation::PaneResume { pane_id, expected } => {
                        #[cfg(test)]
                        if let Some(barrier) =
                            self.inner.control_resume_barrier.lock().unwrap().take()
                        {
                            barrier
                                .entered
                                .send(())
                                .context("control test receiver closed")?;
                            barrier
                                .resume
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(10))
                                .context("control test barrier was not released")?;
                        }
                        ensure!(is_connected(), "agent control caller disconnected");
                        let operation = Operation::PaneResume {
                            pane_id: pane_id.clone(),
                            expected: expected.clone(),
                        };
                        let result = self.resume_scoped(
                            &pane_id,
                            expected,
                            workspace.clone(),
                            Some((&launch, peer)),
                        )?;
                        let result =
                            super::control::scope_control_result(result, workspace.as_deref());
                        match reply {
                            bkpr_protocol::control::ControlReply::Receipt => {
                                bkpr_protocol::control::mutation_receipt(&operation, result)
                            }
                            bkpr_protocol::control::ControlReply::Full => Ok(result),
                        }
                    }
                    operation => {
                        ensure!(is_connected(), "agent control caller disconnected");
                        self.request_for_control(operation, &launch, peer, workspace, reply)
                    }
                }
            }
            AgentRequest::ReceiverRegister {
                session_id,
                mode,
                pid,
            } => self.agent_receiver_register(&launch, peer, session_id, mode, pid),
            AgentRequest::ReceiverSubscribe {
                receiver_id,
                wait_ms,
            } => self.agent_subscribe(&launch, peer, receiver_id, wait_ms, is_connected),
            AgentRequest::WakeValidate {
                receiver_id,
                notification_id,
                generation,
            } => {
                let current = self.channel_wake_current(
                    &launch,
                    peer,
                    &receiver_id,
                    &notification_id,
                    generation,
                )?;
                Ok(json!({"current":current}))
            }
            AgentRequest::WakeOutcome {
                receiver_id,
                notification_id,
                generation,
                outcome,
            } => self.channel_wake_outcome(
                &launch,
                peer,
                (&receiver_id, &notification_id, generation),
                outcome,
            ),
            AgentRequest::Whoami { detail } => {
                let contexts =
                    self.agent_contexts(false, id, ContextSelection::default(), detail)?;
                let state = self.agent_request_state(&launch)?;
                let own = contexts.get(id).context("live agent context unavailable")?;
                let mut identity = public(
                    own,
                    &state.agents[id],
                    state.receivers.get(id),
                    current_subscribers(&state, id),
                    detail,
                );
                identity["protocol_version"] = json!(bkpr_protocol::PROTOCOL_VERSION);
                identity["binding_revision"] = json!(own.binding_revision);
                Ok(identity)
            }
            AgentRequest::DiscoverAgents {
                scope,
                detail,
                launch_id,
                pane_id,
                after,
                limit,
            } => {
                let contexts = self.agent_contexts(
                    scope == AgentScope::Repository,
                    id,
                    ContextSelection {
                        all: true,
                        project_summary: !detail,
                        launch_id: launch_id.as_deref(),
                        pane_id: pane_id.as_deref(),
                    },
                    detail,
                )?;
                let state = self.agent_request_state(&launch)?;
                let own = contexts.get(id).context("live agent context unavailable")?;
                discover(
                    &state,
                    &contexts,
                    own,
                    DiscoveryQuery {
                        scope,
                        detail,
                        launch_id: launch_id.as_deref(),
                        pane_id: pane_id.as_deref(),
                        after: after.as_deref(),
                        limit,
                    },
                )
            }
            AgentRequest::SetTitle { title } => {
                ensure!(
                    title.len() <= 160
                        && !title.chars().any(char::is_control)
                        && !title.contains(&launch.grant.token),
                    "invalid agent title"
                );
                #[cfg(test)]
                self.inner.agents.wait_request_barrier(true);
                let _writer = self.inner.agents.callback_guard()?;
                let mut state = self.agent_request_state(&launch)?;
                state.agents.get_mut(id).expect("live agent").title = Some(title);
                Ok(json!({"updated":true}))
            }
            AgentRequest::Channels { .. } => unreachable!("channel request dispatched above"),
        }
    }
    fn agent_receiver_register(
        &self,
        launch: &Launch,
        peer: &Peer,
        session_id: String,
        mode: AgentReceiverMode,
        pid: u32,
    ) -> Result<Value> {
        let conversation = launch
            .managed
            .conversation
            .as_ref()
            .context("native conversation is not bound")?;
        ensure!(
            conversation.id == session_id,
            "receiver native conversation mismatch"
        );
        ensure!(
            launch.grant.kind != HarnessKind::Codex || mode == AgentReceiverMode::FutureTurn,
            "Codex only supports future-turn delivery"
        );
        let owner = peer.current()?;
        let process = if pid == 0 {
            let chain = crate::harness::ancestry(&owner, &launch.root)?;
            ensure!(
                chain.len() >= 2,
                "no native child between callback and launch root"
            );
            chain[chain.len() - 2].clone()
        } else {
            proc::process(pid.try_into()?)?.context("receiver process exited")?
        };
        crate::harness::ancestry(&process, &launch.root)?;
        let lifetime = ProcessLifetime::observe(&process).ok().map(Arc::new);
        let service = &self.inner.agents;
        let _writer = service.callback_guard()?;
        {
            let captures = self.inner.captures.lock().unwrap();
            captures.authorize(launch)?;
            let current = captures
                .launches
                .get(&launch.grant.launch_id)
                .context("launch ended during receiver registration")?;
            ensure!(
                current.binding_generation == launch.binding_generation
                    && current.managed.conversation.as_ref() == Some(conversation),
                "receiver conversation changed during registration"
            );
        }
        let mut state = service.state.lock().unwrap();
        Agents::require_live(&state, &launch.grant.launch_id)?;
        if let Some(receiver) = state.receivers.get(&launch.grant.launch_id)
            && receiver.process.identity == process.identity
            && receiver.process.pid == process.pid
            && receiver.session_id == session_id
            && receiver.conversation_generation == conversation.generation
            && receiver.mode == mode
        {
            return Ok(
                json!({"receiver_id":receiver.id,"registered":true,"notification_ready":false}),
            );
        }
        let receiver = Receiver {
            id: Uuid::new_v4().to_string(),
            process,
            lifetime,
            owner,
            session_id,
            conversation_generation: conversation.generation,
            mode,
        };
        let result =
            json!({"receiver_id":receiver.id,"registered":true,"notification_ready":false});
        if let Some(lane) = state.channel_lanes.get_mut(&launch.grant.launch_id) {
            lane.active = None;
        }
        state
            .receivers
            .insert(launch.grant.launch_id.clone(), receiver);
        state.revision = state.revision.wrapping_add(1);
        service.changed.notify_all();
        Ok(result)
    }
    fn with_current_agent_receiver<T>(
        &self,
        launch: &Launch,
        receiver_id: &str,
        use_receiver: impl FnOnce(&Receiver) -> T,
    ) -> Result<T> {
        let state = self.inner.agents.state.lock().unwrap();
        healthy_agents(&state)?;
        Agents::require_live(&state, &launch.grant.launch_id)?;
        let receiver = state
            .receivers
            .get(&launch.grant.launch_id)
            .filter(|r| r.id == receiver_id)
            .context("stale native receiver")?;
        let captures = self.inner.captures.lock().unwrap();
        captures.authorize(launch)?;
        let current = captures
            .launches
            .get(&launch.grant.launch_id)
            .context("launch ended")?;
        ensure!(
            current.binding_generation == launch.binding_generation
                && current.control.same_authority(&launch.control)
                && current.control.is_valid()
                && current
                    .managed
                    .conversation
                    .as_ref()
                    .is_some_and(|conversation| conversation.id == receiver.session_id
                        && conversation.generation == receiver.conversation_generation),
            "receiver conversation binding changed"
        );
        Ok(use_receiver(receiver))
    }

    fn agent_receiver(&self, launch: &Launch, receiver_id: &str) -> Result<Receiver> {
        let receiver = self.with_current_agent_receiver(launch, receiver_id, Receiver::clone)?;
        let current = proc::process(receiver.process.pid)?.context("native receiver exited")?;
        ensure!(
            current.identity == receiver.process.identity && current.tty == receiver.process.tty,
            "native receiver birth changed"
        );
        crate::harness::ancestry(&current, &launch.root)?;
        Ok(receiver)
    }
    fn agent_receiver_peer(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver_id: &str,
    ) -> Result<Receiver> {
        let receiver = self.agent_receiver(launch, receiver_id)?;
        let caller = peer.current()?;
        let receiver_lineage = crate::harness::ancestry(&caller, &receiver.process);
        let owner_lineage = crate::harness::ancestry(&caller, &receiver.owner);
        ensure!(
            receiver_lineage.is_ok() || owner_lineage.is_ok(),
            "callback is outside exact receiver/registration process lineage"
        );
        Ok(receiver)
    }
    fn agent_subscribe(
        &self,
        launch: &Launch,
        peer: &Peer,
        receiver_id: String,
        wait_ms: u64,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        let receiver = self.agent_receiver_peer(launch, peer, &receiver_id)?;
        let service = &self.inner.agents;
        let id = &launch.grant.launch_id;
        {
            let _writer = service.callback_guard()?;
            self.with_current_agent_receiver(launch, &receiver_id, |_| ())?;
            let mut state = service.state.lock().unwrap();
            let waiters = state.waiters.entry(id.clone()).or_default();
            ensure!(
                waiters.values().sum::<usize>() < 2,
                "launch subscription capacity exhausted"
            );
            *waiters.entry(receiver_id.clone()).or_default() += 1;
        }
        struct Waiting<'a> {
            service: &'a Agents,
            id: &'a str,
            receiver_id: &'a str,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                let mut state = self.service.state.lock().unwrap();
                if let Some(waiters) = state.waiters.get_mut(self.id)
                    && let Some(count) = waiters.get_mut(self.receiver_id)
                {
                    *count -= 1;
                    if *count == 0 {
                        waiters.remove(self.receiver_id);
                    }
                }
                if state.waiters.get(self.id).is_some_and(BTreeMap::is_empty) {
                    state.waiters.remove(self.id);
                }
            }
        }
        let _waiting = Waiting {
            service,
            id,
            receiver_id: &receiver_id,
        };
        let deadline = Instant::now() + Duration::from_millis(wait_ms.min(25000));
        let mut observed_revision = None;
        let mut channel_retry = None;
        loop {
            // Cancellation ticks inspect socket liveness only; recheck the
            // receiver process and channel eligibility on state changes.
            ensure!(is_connected(), "subscription transport disconnected");
            let revision = {
                let state = service.state.lock().unwrap();
                self.authorize_agent_guard(launch)?;
                healthy_agents(&state)?;
                (state.revision, ChannelLane::availability(&state, launch))
            };
            let retry_ready = channel_retry.is_some_and(|retry| retry <= Instant::now());
            if retry_ready {
                channel_retry = None;
            }
            if observed_revision != Some(revision) || (retry_ready && Instant::now() < deadline) {
                self.agent_receiver_peer(launch, peer, &receiver_id)?;
                let channel = {
                    let state = service.state.lock().unwrap();
                    healthy_agents(&state)?;
                    if (state.revision, ChannelLane::availability(&state, launch)) != revision {
                        continue;
                    }
                    observed_revision = Some(revision);
                    let (pending, busy, _) = revision.1;
                    pending && !busy && channel_retry.is_none()
                };
                if channel {
                    match self.claim_channel_wake(
                        launch,
                        peer,
                        &receiver,
                        is_connected,
                        deadline,
                    )? {
                        ChannelClaimPoll::Ready(wake) => return Ok(wake),
                        ChannelClaimPoll::Empty => {
                            #[cfg(test)]
                            service.wait_channel_barrier(ChannelTestPause::EmptyClaim);
                        }
                        ChannelClaimPoll::Deferred => {
                            channel_retry = Some(Instant::now() + CHANNEL_CLAIM_RETRY);
                        }
                    }
                }
            }
            let state = service.state.lock().unwrap();
            healthy_agents(&state)?;
            if observed_revision
                != Some((state.revision, ChannelLane::availability(&state, launch)))
            {
                continue;
            }
            if Instant::now() >= deadline {
                return Ok(json!({"notification":null}));
            }
            let (state, _) = service
                .changed
                .wait_timeout(
                    state,
                    deadline
                        .min(channel_retry.unwrap_or(deadline))
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(1000)),
                )
                .unwrap();
            drop(state);
        }
    }
}
fn require_scope(scope: AgentScope, own: &ContextFact) -> Result<()> {
    match scope {
        AgentScope::Workspace => ensure!(
            own.active && own.pane.workspace_id.is_some(),
            "live foreground workspace context unavailable; explicitly opt into broader scope"
        ),
        AgentScope::Session => ensure!(own.active, "live foreground session context unavailable"),
        AgentScope::Repository => ensure!(
            own.repository.is_some(),
            "canonical repository identity unavailable"
        ),
        AgentScope::All => {}
    }
    Ok(())
}

fn wake_deadline(claimed_at: u64) -> u64 {
    claimed_at.saturating_add(WAKE_BATCH_LEASE_MS)
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
