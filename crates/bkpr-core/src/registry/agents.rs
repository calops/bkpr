//! Durable launch inboxes. Only this service writes agent state; capture capabilities stay volatile.
use super::*;
use crate::{
    control_lease::ProcessLifetime,
    harness::{Launch, Peer},
    harness_process as proc,
};
use sha2::{Digest, Sha256};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

mod channel_wakes;
use channel_wakes::{ChannelClaimPoll, ChannelLane, is_channel_batch};
mod messages;

const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const WAKE_BATCH_LEASE_MS: u64 = 30_000;
const CHANNEL_CLAIM_RETRY: Duration = Duration::from_millis(100);
const MAX_INBOX: usize = 1000;
const MAX_STORE_BYTES: u64 = 128 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    agents: BTreeMap<String, Agent>,
    messages: BTreeMap<String, Message>,
    sequence: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Agent {
    pane_id: String,
    harness: HarnessKind,
    title: Option<String>,
    exited_at: Option<u64>,
    generation: u64,
    notification: Option<Notification>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Message {
    id: String,
    from: String,
    to: String,
    text: Option<Arc<String>>,
    key: String,
    digest: String,
    scope: AgentScope,
    sequence: u64,
    created_at: u64,
    read_at: Option<u64>,
    acknowledged_at: Option<u64>,
    expired_at: Option<u64>,
    notification_id: Option<String>,
    generation: Option<u64>,
    receiver_id: Option<String>,
    wake_claimed_at: Option<u64>,
    wake_state: String,
    wake_reason: Option<String>,
    wake_failure: Option<WakeFailure>,
}
#[derive(Clone, Serialize, Deserialize)]
struct WakeFailure {
    state: String,
    reason: String,
    notification_id: String,
    generation: u64,
    recorded_at: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Notification {
    id: String,
    generation: u64,
    receiver_id: String,
    state: String,
    reason: Option<String>,
    claimed_at: u64,
    #[serde(default)]
    message_count: usize,
    #[serde(default)]
    unread_count: usize,
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
    store: Store,
    receivers: BTreeMap<String, Receiver>,
    waiters: BTreeMap<String, BTreeMap<String, usize>>,
    channel_lanes: BTreeMap<String, ChannelLane>,
    failure: Option<String>,
    closing: bool,
    maintenance_at: Option<u64>,
    revision: u64,
}
pub(super) struct Agents {
    root: PathBuf,
    directory: File,
    state: Mutex<AgentState>,
    writer: Mutex<()>,
    changed: Condvar,
    #[cfg(test)]
    fail_directory_sync: std::sync::atomic::AtomicBool,
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
    recipient: Option<&'a str>,
    launch_id: Option<&'a str>,
    pane_id: Option<&'a str>,
    successor_pane: Option<&'a str>,
}
#[derive(Default)]
struct ContextSnapshot {
    facts: BTreeMap<String, ContextFact>,
    recipient_bindings: Vec<(Launch, bool)>,
}

fn matches_recipient(launch: &Launch, selector: &str) -> bool {
    launch.grant.launch_id == selector
        || launch
            .managed
            .conversation
            .as_ref()
            .is_some_and(|conversation| conversation.id == selector)
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
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn payload(to: &str, text: &str, scope: AgentScope) -> String {
    hash(&serde_json::to_vec(&(to, text, scope)).expect("serializable message payload"))
}
fn pending(m: &Message) -> bool {
    m.text.is_some() && m.acknowledged_at.is_none() && m.expired_at.is_none()
}
fn unread(m: &Message) -> bool {
    pending(m) && m.read_at.is_none()
}
fn claimable(m: &Message) -> bool {
    unread(m) && m.notification_id.is_none()
}
fn summary(m: &Message) -> Value {
    json!({"message_id":m.id,"from":m.from,"to":m.to,"scope":m.scope,"created_at":m.created_at,"read_at":m.read_at,"acknowledged_at":m.acknowledged_at,"expired_at":m.expired_at,"wake_failure":m.wake_failure,"delivery":{"state":m.wake_state,"reason":m.wake_reason,"notification_id":m.notification_id,"generation":m.generation}})
}
fn receipt(m: &Message, duplicate: bool) -> Value {
    let mut value = json!({"message_id":m.id,"durable":true,"duplicate":duplicate,"wake":{"state":m.wake_state}});
    if let Some(reason) = &m.wake_reason {
        value["wake"]["reason"] = json!(reason);
    }
    value
}
fn healthy_agents(state: &AgentState) -> Result<()> {
    ensure!(
        state.failure.is_none(),
        "agent durable storage unavailable; delivery suspended until restart"
    );
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
    #[cfg(test)]
    pub(super) fn fail_next_commit(&self) {
        self.fail_directory_sync
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub(super) fn check_health(&self) -> Result<()> {
        healthy_agents(&self.state.lock().unwrap())
    }
    pub(super) fn callback_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        let guard = self.writer.lock().unwrap();
        self.check_health()?;
        Ok(guard)
    }
    pub(super) fn open(root: &Path) -> Result<Self> {
        let root = root.join("agents");
        private_directory(&root)?;
        let mut store = Self::load(&root)?;
        // Capabilities and OS authorities are deliberately absent on restart. Old inboxes
        // remain diagnostics only, even if an identical native conversation is resumed.
        let timestamp = now();
        for agent in store.agents.values_mut() {
            if agent.exited_at.is_none() {
                agent.exited_at = Some(timestamp);
            }
            agent.notification = None;
        }
        for (id, receiver, notification, generation) in pending_dispatches(&store) {
            settle_unknown(
                &mut store,
                &id,
                &receiver,
                &notification,
                generation,
                "daemon restarted before native confirmation",
                timestamp,
            );
        }
        expire(&mut store, timestamp);
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&root)?;
        let this = Self {
            root,
            directory,
            state: Mutex::new(AgentState {
                store,
                receivers: BTreeMap::new(),
                waiters: BTreeMap::new(),
                channel_lanes: BTreeMap::new(),
                failure: None,
                closing: false,
                maintenance_at: None,
                revision: 0,
            }),
            writer: Mutex::new(()),
            changed: Condvar::new(),
            #[cfg(test)]
            fail_directory_sync: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            request_barrier: Mutex::new(None),
            #[cfg(test)]
            channel_barrier: Mutex::new(None),
        };
        this.transaction(|_| Ok(()))?;
        Ok(this)
    }
    fn load(root: &Path) -> Result<Store> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("state.json"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Store {
                    version: 1,
                    agents: BTreeMap::new(),
                    messages: BTreeMap::new(),
                    sequence: 0,
                });
            }
            Err(error) => return Err(error.into()),
        };
        check_file(&file)?;
        ensure!(
            file.metadata()?.nlink() == 1,
            "agent store must not be hard-linked"
        );
        ensure!(
            file.metadata()?.len() <= MAX_STORE_BYTES,
            "agent store exceeds size bound"
        );
        let store: Store = serde_json::from_reader(file)?;
        ensure!(
            store.version == 1,
            "unsupported durable agent store version"
        );
        let mut sequences = BTreeSet::new();
        for (id, m) in &store.messages {
            ensure!(
                id == &m.id
                    && m.sequence > 0
                    && m.sequence <= store.sequence
                    && sequences.insert(m.sequence),
                "invalid durable message identity"
            );
            ensure!(
                store.agents.contains_key(&m.from) && store.agents.contains_key(&m.to),
                "orphan durable message"
            );
            ensure!(
                m.text.as_ref().is_none_or(|t| t.len() <= 65536),
                "oversize durable message"
            );
            ensure!(
                (m.acknowledged_at.is_none() && m.expired_at.is_none()) || m.text.is_none(),
                "retired body retained"
            );
        }
        Ok(store)
    }
    fn save(&self, store: &Store) -> Result<()> {
        // No runtime/registry/capture locks are held across serialization or fsync.
        let metadata = fs::symlink_metadata(&self.root)?;
        let expected = self.directory.metadata()?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
                && metadata.dev() == expected.dev()
                && metadata.ino() == expected.ino(),
            "agent store directory identity changed"
        );
        match fs::symlink_metadata(self.root.join("state.json")) {
            Ok(metadata) => ensure!(
                metadata.is_file()
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o077 == 0
                    && metadata.nlink() == 1,
                "unsafe agent store file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(file.as_file_mut(), store)?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_STORE_BYTES,
            "agent store capacity exhausted"
        );
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(self.root.join("state.json"))?;
        #[cfg(test)]
        if self
            .fail_directory_sync
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            bail!("injected agent directory sync failure");
        }
        self.directory.sync_all()?;
        Ok(())
    }
    fn transaction<T>(&self, change: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        self.transaction_maybe(|store| change(store).map(|value| (value, true)))
    }
    fn transaction_maybe<T>(
        &self,
        change: impl FnOnce(&mut Store) -> Result<(T, bool)>,
    ) -> Result<T> {
        let _writer = self.writer.lock().unwrap();
        self.transaction_locked(change)
    }
    // Caller holds the agent writer; runtime locks are released before storage I/O.
    fn transaction_locked<T>(
        &self,
        change: impl FnOnce(&mut Store) -> Result<(T, bool)>,
    ) -> Result<T> {
        let mut candidate = {
            let state = self.state.lock().unwrap();
            healthy_agents(&state)?;
            state.store.clone()
        };
        let (result, mut changed) = change(&mut candidate)?;
        {
            let state = self.state.lock().unwrap();
            changed |= prune_retired(&mut candidate, now(), &state);
        }
        if !changed {
            return Ok(result);
        }
        if let Err(error) = self.save(&candidate) {
            // Rename may have committed. Never roll back or guess acceptance. Restart
            // reconciles only the authoritative saved file, and all callbacks stop now.
            let mut state = self.state.lock().unwrap();
            state.failure = Some("durable agent commit failed".into());
            state.receivers.clear();
            self.changed.notify_all();
            return Err(
                error.context("agent mutation outcome unknown: durable commit failed; delivery suspended until restart")
            );
        }
        let maintenance_at = next_maintenance(&candidate);
        {
            let mut state = self.state.lock().unwrap();
            state.store = candidate;
            state.maintenance_at = maintenance_at;
            state.revision = state.revision.wrapping_add(1);
        }
        self.changed.notify_all();
        Ok(result)
    }
    fn register(&self, launch: &Launch) -> Result<()> {
        self.transaction(|store| {
            ensure!(
                !store.agents.contains_key(&launch.grant.launch_id),
                "agent launch already exists"
            );
            store.agents.insert(
                launch.grant.launch_id.clone(),
                Agent {
                    pane_id: launch.grant.pane_id.clone(),
                    harness: launch.grant.kind,
                    title: None,
                    exited_at: None,
                    generation: 0,
                    notification: None,
                },
            );
            Ok(())
        })
    }
    fn retire(&self, ids: &[String], timestamp: u64) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.transaction_maybe(|store| {
            let mut changed = false;
            for id in ids {
                if let Some(agent) = store.agents.get_mut(id) {
                    if agent.exited_at.is_none() {
                        agent.exited_at = Some(timestamp);
                        changed = true;
                    }
                    changed |= agent.notification.take().is_some();
                }
            }
            for (id, receiver, notification, generation) in pending_dispatches(store)
                .into_iter()
                .filter(|(id, ..)| ids.contains(id))
            {
                changed |= settle_unknown(
                    store,
                    &id,
                    &receiver,
                    &notification,
                    generation,
                    "recipient retired before native confirmation",
                    timestamp,
                );
            }
            changed |= expire(store, timestamp);
            Ok(((), changed))
        })?;
        let mut state = self.state.lock().unwrap();
        for id in ids {
            state.receivers.remove(id);
            state.channel_lanes.remove(id);
        }
        self.changed.notify_all();
        Ok(())
    }
    pub(super) fn retire_pane(&self, pane_id: &str) -> Result<()> {
        let ids = self
            .state
            .lock()
            .unwrap()
            .store
            .agents
            .iter()
            .filter(|(_, a)| a.pane_id == pane_id && a.exited_at.is_none())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        self.retire(&ids, now())
    }
    pub(super) fn shutdown(&self) -> Result<()> {
        if self.state.lock().unwrap().closing {
            return Ok(());
        }
        let result = self.transaction_maybe(|store| {
            {
                let mut state = self.state.lock().unwrap();
                state.closing = true;
                state.receivers.clear();
                self.changed.notify_all();
            }
            let timestamp = now();
            let mut changed = false;
            for agent in store.agents.values_mut() {
                if agent.exited_at.is_none() {
                    agent.exited_at = Some(timestamp);
                    changed = true;
                }
                changed |= agent.notification.take().is_some();
            }
            for (id, receiver, notification, generation) in pending_dispatches(store) {
                changed |= settle_unknown(
                    store,
                    &id,
                    &receiver,
                    &notification,
                    generation,
                    "daemon stopped before native confirmation",
                    timestamp,
                );
            }
            changed |= expire(store, timestamp);
            Ok(((), changed))
        });
        let mut state = self.state.lock().unwrap();
        state.closing = true;
        state.receivers.clear();
        self.changed.notify_all();
        result
    }
    fn require_live(store: &Store, id: &str) -> Result<()> {
        ensure!(
            store.agents.get(id).is_some_and(|a| a.exited_at.is_none()),
            "agent identity is retired; an exited launch cannot resume its old inbox"
        );
        Ok(())
    }
    fn maintain(&self, timestamp: u64) -> Result<()> {
        self.transaction_maybe(|store| {
            let mut changed = false;
            for agent in store.agents.values_mut() {
                changed |= expire_batch(agent, timestamp).is_some();
            }
            // Read/ack can release a slot before the native outcome arrives.
            // Even detached pending receipts keep their original timeout.
            for message in store.messages.values_mut() {
                changed |= expire_retired_message(&store.agents, message, timestamp);
                changed |= expire_pending_wake(message, timestamp);
            }
            Ok(((), changed))
        })
    }
}
fn expire(store: &mut Store, timestamp: u64) -> bool {
    let mut changed = false;
    for message in store.messages.values_mut() {
        changed |= expire_retired_message(&store.agents, message, timestamp);
    }
    changed
}

fn expire_retired_message(
    agents: &BTreeMap<String, Agent>,
    message: &mut Message,
    timestamp: u64,
) -> bool {
    if let Some(deadline) = retention_deadline(agents, message)
        && deadline <= timestamp
    {
        message.text = None;
        message.expired_at = Some(deadline);
        return true;
    }
    false
}

fn retention_deadline(agents: &BTreeMap<String, Agent>, message: &Message) -> Option<u64> {
    if !pending(message) {
        return None;
    }
    agents
        .get(&message.to)?
        .exited_at
        .map(|exit| exit.saturating_add(RETENTION_MS))
}

fn prune_retired(store: &mut Store, timestamp: u64, state: &AgentState) -> bool {
    let referenced = store
        .messages
        .values()
        .flat_map(|message| [&message.from, &message.to])
        .collect::<BTreeSet<_>>();
    let before = store.agents.len();
    // Existing seven-day retention applies only to unreferenced launch bookkeeping.
    // Message receipts and pane/recovery history have no new deletion policy.
    store.agents.retain(|id, agent| {
        agent
            .exited_at
            .is_none_or(|exit| timestamp < exit.saturating_add(RETENTION_MS))
            || agent.notification.is_some()
            || referenced.contains(id)
            || state.receivers.contains_key(id)
            || state.waiters.contains_key(id)
    });
    before != store.agents.len()
}
fn in_scope(
    scope: AgentScope,
    sender: &ContextFact,
    recipient: &ContextFact,
    _store: &Store,
) -> bool {
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
    require_scope(query.scope, own, &state.store)?;
    let limit = messages::page_limit(query.limit)?;
    let mut candidates = contexts.values().filter(|other| {
        other.launch_id != own.launch_id
            && other.active
            && state
                .store
                .agents
                .get(&other.launch_id)
                .is_some_and(|a| a.exited_at.is_none())
            && in_scope(query.scope, own, other, &state.store)
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
            let agent = &state.store.agents[&other.launch_id];
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
fn cursor_start(
    messages: &[&Message],
    store: &Store,
    id: &str,
    after: Option<&str>,
    inbox: bool,
) -> Result<usize> {
    let Some(after) = after else {
        return Ok(0);
    };
    let m = store
        .messages
        .get(after)
        .context("unknown message cursor")?;
    ensure!(
        if inbox { m.to == id } else { m.from == id },
        "foreign message cursor"
    );
    Ok(messages.partition_point(|candidate| candidate.sequence <= m.sequence))
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
    let agent = agents.store.agents.get(&visible.launch_id)?;
    (agent.exited_at.is_none() && launch.channel_launch_alive()).then_some((launch, agent))
}

fn channel_caller<'a>(
    state: &State,
    workspace: &str,
    caller: &Launch,
    agents: &'a AgentState,
    captures: &'a crate::harness::Captures,
) -> Result<(&'a Launch, &'a Agent)> {
    captures.authorize(caller)?;
    let (current, agent) = state
        .record
        .registry
        .panes
        .iter()
        .find(|pane| pane.id == caller.grant.pane_id)
        .and_then(|pane| live_channel_agent(state, pane, workspace, agents, captures, None))
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

fn channel_agent(launch: &Launch, agent: &Agent) -> channels::Participant {
    channels::Participant {
        id: launch
            .channel_participant
            .clone()
            .expect("live channel identity"),
        kind: channels::ParticipantKind::Agent,
        display_name: agent
            .title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| launch.grant.kind.display_name().to_owned()),
    }
}

impl Registry {
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
        use channels::{Participant, ParticipantId, ParticipantKind};

        let state = self.inner.state.lock().unwrap();
        ensure!(connected(), ChannelAccessLost);
        healthy(&state)?;
        ensure!(
            !state.closing && !self.session_end_requested(),
            SessionClosing("session is shutting down")
        );
        let workspace_open = workspace_exists(&state, workspace);
        let writable = workspace_open && !state.closing_workspaces.contains(workspace);
        let user = || Participant {
            id: ParticipantId(self.inner.session.session_id.clone()),
            kind: ParticipantKind::User,
            display_name: "User".into(),
        };
        if caller.is_none() && (!with_roster || !workspace_open) {
            return Ok(ChannelAuthority {
                actor: user(),
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
            let (current, agent) = channel_caller(&state, workspace, caller, &agents, &captures)
                .context(ChannelAccessLost)?;
            channel_agent(current, agent)
        } else {
            user()
        };
        let mut participants = Vec::new();
        if with_roster {
            if caller.is_some() {
                participants.push(WorkspaceParticipant {
                    actor: user(),
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
                        })
                        .filter(|(launch, _)| {
                            launch.channel_participant.as_ref() != Some(&actor.id)
                        })
                        .map(|(launch, agent)| WorkspaceParticipant {
                            actor: channel_agent(launch, agent),
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
        channel_caller(&state, workspace, caller, &agents, &captures)?;
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

    pub(super) fn start_agent_maintenance(&self) -> Result<()> {
        let weak = Arc::downgrade(&self.inner.agents);
        std::thread::Builder::new()
            .name("bkpr-agent-retention".into())
            .spawn(move || {
                loop {
                    let Some(service) = weak.upgrade() else {
                        break;
                    };
                    if Arc::strong_count(&service) == 1 {
                        break;
                    }
                    let state = service.state.lock().unwrap();
                    if state.closing || state.failure.is_some() {
                        break;
                    }
                    let timestamp = now();
                    if state
                        .maintenance_at
                        .is_some_and(|deadline| deadline <= timestamp)
                    {
                        drop(state);
                        if service.maintain(timestamp).is_err() {
                            break;
                        }
                    } else {
                        // The periodic weak-owner check performs no store/process scan.
                        let wait = state.maintenance_at.map_or(1000, |deadline| {
                            deadline.saturating_sub(timestamp).min(1000)
                        });
                        let (state, _) = service
                            .changed
                            .wait_timeout(state, Duration::from_millis(wait))
                            .unwrap();
                        drop(state);
                    }
                    drop(service);
                }
            })?;
        Ok(())
    }
    pub(crate) fn agent_subscription_disconnected(
        &self,
        launch_id: &str,
        receiver_id: &str,
        notification_id: &str,
        generation: u64,
    ) -> Result<()> {
        if is_channel_batch(notification_id) {
            return self.channel_subscription_disconnected(
                launch_id,
                receiver_id,
                notification_id,
                generation,
            );
        }
        self.inner.agents.transaction_maybe(|store| {
            let changed = settle_unknown(
                store,
                launch_id,
                receiver_id,
                notification_id,
                generation,
                "subscription transport disconnected before native confirmation",
                now(),
            );
            Ok(((), changed))
        })
    }
    fn agent_contexts(
        &self,
        resolve_repository: bool,
        own_id: &str,
        selection: ContextSelection<'_>,
        detail: bool,
    ) -> Result<ContextSnapshot> {
        let launches = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .values()
            .filter(|l| {
                l.grant.launch_id == own_id
                    || selection
                        .recipient
                        .is_some_and(|selector| matches_recipient(l, selector))
                    || Some(l.grant.pane_id.as_str()) == selection.successor_pane
                    || (selection.all
                        && selection.launch_id.is_none_or(|id| l.grant.launch_id == id)
                        && selection.pane_id.is_none_or(|id| l.grant.pane_id == id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let recipient_bindings = if let Some(selector) = selection.recipient {
            let state = self.inner.agents.state.lock().unwrap();
            healthy_agents(&state)?;
            launches
                .iter()
                .filter(|launch| matches_recipient(launch, selector))
                .map(|launch| {
                    let live = state
                        .store
                        .agents
                        .get(&launch.grant.launch_id)
                        .is_some_and(|agent| agent.exited_at.is_none());
                    (launch.clone(), live)
                })
                .collect()
        } else {
            Vec::new()
        };
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
        let facts = observed
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
            .collect();
        Ok(ContextSnapshot {
            facts,
            recipient_bindings,
        })
    }
    // The registry transaction and agent writer fix ownership and bindings here.
    // Foreground/process observations were collected before either lock.
    fn admit_agent_recipient(
        &self,
        store: &Store,
        snapshot: &ContextSnapshot,
        selector: &str,
        scope: AgentScope,
    ) -> Result<()> {
        const CHANGED: &str =
            "recipient context changed before admission; rediscover; nothing was sent";
        let captures = self.inner.captures.lock().unwrap();
        ensure!(
            captures
                .launches
                .values()
                .filter(|launch| matches_recipient(launch, selector))
                .count()
                == snapshot.recipient_bindings.len(),
            CHANGED
        );
        for (expected, live) in &snapshot.recipient_bindings {
            let current = captures
                .launches
                .get(&expected.grant.launch_id)
                .context(CHANGED)?;
            ensure!(
                current.grant.pane_id == expected.grant.pane_id
                    && current.root.identity == expected.root.identity
                    && current.binding_generation == expected.binding_generation
                    && current.managed.conversation == expected.managed.conversation
                    && current.control.same_authority(&expected.control)
                    && current.control.is_valid()
                    && store
                        .agents
                        .get(&expected.grant.launch_id)
                        .is_some_and(|agent| agent.exited_at.is_none())
                        == *live,
                CHANGED
            );
        }
        drop(captures);
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(!state.closing, "daemon shutting down");
        for fact in snapshot.facts.values() {
            let current = state
                .record
                .registry
                .panes
                .iter()
                .find(|pane| pane.id == fact.pane.id)
                .context(CHANGED)?;
            ensure!(
                current.status == PaneStatus::Running
                    && current.workspace_id == fact.pane.workspace_id,
                CHANGED
            );
            if scope == AgentScope::Repository {
                let repository = current
                    .project
                    .as_ref()
                    .filter(|project| {
                        fact.active
                            && project.state == ProjectState::Ready
                            && fact.cwd.as_ref() == Some(&project.cwd)
                    })
                    .and_then(|project| project.repository.as_ref())
                    .map(|repository| &repository.common_dir);
                ensure!(repository == fact.repository.as_ref(), CHANGED);
            }
        }
        Ok(())
    }
    pub(super) fn agent_register(&self, launch: &Launch, _cwd: &Path) -> Result<()> {
        self.inner.agents.register(launch)
    }
    pub(super) fn agent_retire(&self, ids: &[String]) -> Result<()> {
        self.inner.agents.retire(ids, now())
    }
    pub(super) fn agent_capture_event(
        &self,
        launch_id: &str,
        token: &str,
        event: CaptureEvent,
    ) -> Result<()> {
        let result = self.inner.agents.transaction_maybe(|store| {
            let mut captures = self.inner.captures.lock().unwrap();
            let before = captures
                .launches
                .get(launch_id)
                .map(|launch| launch.binding_generation);
            let changes_binding = matches!(
                &event,
                CaptureEvent::Bind { .. } | CaptureEvent::Unbind { .. }
            );
            captures.event(launch_id, token, event)?;
            let after = captures
                .launches
                .get(launch_id)
                .map(|launch| launch.binding_generation);
            drop(captures);
            let mut changed = false;
            if changes_binding && before != after {
                {
                    let mut state = self.inner.agents.state.lock().unwrap();
                    state.receivers.remove(launch_id);
                    if let Some(lane) = state.channel_lanes.get_mut(launch_id) {
                        lane.pending_binding = None;
                        lane.active = None;
                        lane.revision = lane.revision.wrapping_add(1);
                    }
                    state.revision = state.revision.wrapping_add(1);
                }
                let timestamp = now();
                for (id, receiver, notification, generation) in pending_dispatches(store)
                    .into_iter()
                    .filter(|(id, ..)| id == launch_id)
                {
                    changed |= settle_unknown(
                        store,
                        &id,
                        &receiver,
                        &notification,
                        generation,
                        "native conversation binding changed before confirmation",
                        timestamp,
                    );
                }
                if let Some(agent) = store.agents.get_mut(launch_id) {
                    changed |= agent.notification.take().is_some();
                }
            }
            Ok(((), changed))
        });
        self.inner.agents.changed.notify_all();
        result
    }
    // Caller owns the registry transaction, which also serializes authenticated
    // End. Recheck both launch capability and durable-agent retirement at admission.
    pub(super) fn authorize_resume_origin(&self, launch: &Launch) -> Result<()> {
        self.capture_pane(&launch.grant.pane_id)?;
        ensure!(
            launch.control.is_valid(),
            "native conversation control binding changed"
        );
        self.authorize_agent_guard(launch)?;
        let state = self.inner.agents.state.lock().unwrap();
        healthy_agents(&state)?;
        Agents::require_live(&state.store, &launch.grant.launch_id)
    }
    fn authorize_agent_guard(&self, launch: &Launch) -> Result<()> {
        self.inner.captures.lock().unwrap().authorize(launch)
    }
    // The state lock fixes the durable/read snapshot; captures supplies the
    // binding's linearization point. Capture writers never retain captures while
    // waiting on agent state, so this order cannot invert their writer path.
    fn agent_request_state(
        &self,
        launch: &Launch,
    ) -> Result<std::sync::MutexGuard<'_, AgentState>> {
        let state = self.inner.agents.state.lock().unwrap();
        healthy_agents(&state)?;
        Agents::require_live(&state.store, &launch.grant.launch_id)?;
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
        if let AgentRequest::SendMessage {
            to,
            text,
            idempotency_key,
            scope,
        } = &request
        {
            let state = self.agent_request_state(&launch)?;
            if let Some(receipt) = messages::retry(
                &state.store,
                id,
                messages::SendPayload {
                    to,
                    text,
                    scope: *scope,
                },
                idempotency_key,
                &launch.grant.token,
            )? {
                return Ok(receipt);
            }
        }
        {
            let state = self.agent_request_state(&launch)?;
            if let Some(result) = messages::unchanged(&state.store, id, &request)? {
                return Ok(result);
            }
        }
        match request {
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
                let snapshot =
                    self.agent_contexts(false, id, ContextSelection::default(), false)?;
                let own = snapshot
                    .facts
                    .get(id)
                    .context("live agent context unavailable")?;
                ensure!(own.active, "live foreground agent context unavailable");
                {
                    let state = self.agent_request_state(&launch)?;
                    require_scope(scope, own, &state.store)?;
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
                if is_channel_batch(&notification_id) {
                    let current = self.channel_wake_current(
                        &launch,
                        peer,
                        &receiver_id,
                        &notification_id,
                        generation,
                    )?;
                    return Ok(json!({"current":current}));
                }
                self.agent_receiver_peer(&launch, peer, &receiver_id)?;
                let state = self.agent_request_state(&launch)?;
                let current = state.store.agents[id]
                    .notification
                    .as_ref()
                    .is_some_and(|n| {
                        n.receiver_id == receiver_id
                            && n.id == notification_id
                            && n.generation == generation
                            && n.state == "pending"
                            && now() < wake_deadline(n.claimed_at)
                    });
                Ok(json!({"current":current}))
            }
            AgentRequest::WakeOutcome {
                receiver_id,
                notification_id,
                generation,
                outcome,
            } => {
                if is_channel_batch(&notification_id) {
                    return self.channel_wake_outcome(
                        &launch,
                        peer,
                        (&receiver_id, &notification_id, generation),
                        outcome,
                    );
                }
                self.agent_receiver_peer(&launch, peer, &receiver_id)?;
                self.inner.agents.transaction(|store| {
                    self.authorize_agent_guard(&launch)?;
                    self.agent_receiver_peer(&launch, peer, &receiver_id)?;
                    messages::outcome(
                        store,
                        id,
                        &receiver_id,
                        &notification_id,
                        generation,
                        outcome,
                        &launch.grant.token,
                    )
                })
            }
            request => {
                let resolve_repository = matches!(
                    &request,
                    AgentRequest::DiscoverAgents {
                        scope: AgentScope::Repository,
                        ..
                    } | AgentRequest::SendMessage {
                        scope: AgentScope::Repository,
                        ..
                    }
                );
                let needs_context = matches!(
                    &request,
                    AgentRequest::Whoami { .. }
                        | AgentRequest::DiscoverAgents { .. }
                        | AgentRequest::SendMessage { .. }
                );
                let (selection, detail, successor_pane) = match &request {
                    AgentRequest::DiscoverAgents {
                        launch_id,
                        pane_id,
                        detail,
                        ..
                    } => (
                        ContextSelection {
                            all: true,
                            project_summary: !*detail,
                            launch_id: launch_id.as_deref(),
                            pane_id: pane_id.as_deref(),
                            ..Default::default()
                        },
                        *detail,
                        None,
                    ),
                    AgentRequest::SendMessage { to, .. } => {
                        let state = self.inner.agents.state.lock().unwrap();
                        let pane = state
                            .store
                            .agents
                            .get(to)
                            .filter(|agent| agent.exited_at.is_some())
                            .map(|agent| agent.pane_id.clone());
                        (
                            ContextSelection {
                                recipient: Some(to),
                                ..Default::default()
                            },
                            false,
                            pane,
                        )
                    }
                    AgentRequest::Whoami { detail } => (ContextSelection::default(), *detail, None),
                    _ => (ContextSelection::default(), false, None),
                };
                let snapshot = if needs_context {
                    self.agent_contexts(
                        resolve_repository,
                        id,
                        ContextSelection {
                            successor_pane: successor_pane.as_deref(),
                            ..selection
                        },
                        detail,
                    )?
                } else {
                    ContextSnapshot::default()
                };
                let contexts = &snapshot.facts;
                #[cfg(test)]
                self.inner.agents.wait_request_barrier(true);
                match request {
                    AgentRequest::Whoami { detail } => {
                        let state = self.agent_request_state(&launch)?;
                        let own = contexts.get(id).context("live agent context unavailable")?;
                        let mut identity = public(
                            own,
                            &state.store.agents[id],
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
                        let state = self.agent_request_state(&launch)?;
                        let own = contexts.get(id).context("live agent context unavailable")?;
                        discover(
                            &state,
                            contexts,
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
                    AgentRequest::InspectOutbox {
                        message_id,
                        after,
                        limit,
                    } => {
                        // Diagnostics never observe process exit or advance retention.
                        let state = self.agent_request_state(&launch)?;
                        if let Some(message_id) = message_id {
                            let message = state
                                .store
                                .messages
                                .get(&message_id)
                                .context("unknown message")?;
                            ensure!(message.from == *id, "foreign sender outbox");
                            return Ok(json!({"message":summary(message)}));
                        }
                        let mut messages = state
                            .store
                            .messages
                            .values()
                            .filter(|m| m.from == *id)
                            .collect::<Vec<_>>();
                        messages.sort_unstable_by_key(|m| m.sequence);
                        let start =
                            cursor_start(&messages, &state.store, id, after.as_deref(), false)?;
                        let end = (start + messages::page_limit(limit)?).min(messages.len());
                        Ok(
                            json!({"messages":messages[start..end].iter().map(|m| summary(m)).collect::<Vec<_>>(),"has_more":end<messages.len(),"next_cursor":messages[start..end].last().map(|m| &m.id)}),
                        )
                    }
                    AgentRequest::SetTitle { title } => {
                        ensure!(
                            title.len() <= 160
                                && !title.chars().any(char::is_control)
                                && !title.contains(&launch.grant.token),
                            "invalid agent title"
                        );
                        self.inner.agents.transaction_maybe(|store| {
                            self.authorize_agent_guard(&launch)?;
                            Agents::require_live(store, id)?;
                            let agent = store.agents.get_mut(id).unwrap();
                            let changed = agent.title.as_deref() != Some(title.as_str());
                            agent.title = Some(title);
                            Ok((json!({"updated":true}), changed))
                        })
                    }
                    AgentRequest::SendMessage {
                        to,
                        text,
                        idempotency_key,
                        scope,
                    } => {
                        let own = contexts.get(id).context("live agent context unavailable")?;
                        let transaction = self.inner.transaction.lock().unwrap();
                        self.inner.agents.transaction(|store| {
                            self.authorize_agent_guard(&launch)?;
                            let result = messages::send(
                                store,
                                own,
                                contexts,
                                messages::SendInput {
                                    to,
                                    text,
                                    idempotency_key,
                                    scope,
                                    timestamp: now(),
                                },
                                &launch.grant.token,
                                |store, selector| {
                                    self.admit_agent_recipient(store, &snapshot, selector, scope)
                                },
                                |recipient| {
                                    let state = self.inner.agents.state.lock().unwrap();
                                    state.receivers.contains_key(recipient)
                                        && current_subscribers(&state, recipient) > 0
                                },
                            );
                            // Admission fixes the launch and scope. Only the agent
                            // writer must remain held while persisting that decision.
                            drop(transaction);
                            result
                        })
                    }
                    AgentRequest::ReadInbox {
                        after,
                        limit,
                        ack_ids,
                        include_read,
                    } => self.inner.agents.transaction_maybe(|store| {
                        self.authorize_agent_guard(&launch)?;
                        messages::read(
                            store,
                            id,
                            after.as_deref(),
                            limit,
                            &ack_ids,
                            include_read,
                            now(),
                        )
                    }),
                    AgentRequest::ReadMessage {
                        message_id,
                        offset,
                        max_bytes,
                    } => self.inner.agents.transaction_maybe(|store| {
                        self.authorize_agent_guard(&launch)?;
                        messages::read_message(store, id, &message_id, offset, max_bytes, now())
                    }),
                    AgentRequest::AckMessage { ids } => {
                        self.inner.agents.transaction_maybe(|store| {
                            self.authorize_agent_guard(&launch)?;
                            messages::ack(store, id, &ids, now())
                        })
                    }
                    _ => unreachable!("receiver requests dispatched above"),
                }
            }
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
        let _writer = self.inner.agents.callback_guard()?;
        self.authorize_agent_guard(launch)?;
        let current = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .get(&launch.grant.launch_id)
            .cloned()
            .context("launch ended during receiver registration")?;
        ensure!(
            current.managed.conversation.as_ref() == Some(conversation),
            "receiver conversation changed during registration"
        );
        let previous = {
            let state = self.inner.agents.state.lock().unwrap();
            healthy_agents(&state)?;
            Agents::require_live(&state.store, &launch.grant.launch_id)?;
            state.receivers.get(&launch.grant.launch_id).cloned()
        };
        if let Some(receiver) = &previous
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
        if let Some(previous) = previous {
            self.inner.agents.transaction_locked(|store| {
                let mut changed = false;
                let timestamp = now();
                for (id, receiver, notification, generation) in pending_dispatches(store)
                    .into_iter()
                    .filter(|(id, receiver, ..)| {
                        id == &launch.grant.launch_id && receiver == &previous.id
                    })
                {
                    changed |= settle_unknown(
                        store,
                        &id,
                        &receiver,
                        &notification,
                        generation,
                        "native receiver replaced before confirmation",
                        timestamp,
                    );
                }
                if let Some(agent) = store.agents.get_mut(&launch.grant.launch_id)
                    && agent
                        .notification
                        .as_ref()
                        .is_some_and(|n| n.receiver_id == previous.id)
                {
                    agent.notification = None;
                    changed = true;
                }
                Ok(((), changed))
            })?;
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
        {
            let mut state = self.inner.agents.state.lock().unwrap();
            if let Some(lane) = state.channel_lanes.get_mut(&launch.grant.launch_id) {
                lane.active = None;
            }
            state
                .receivers
                .insert(launch.grant.launch_id.clone(), receiver);
            state.revision = state.revision.wrapping_add(1);
        }
        self.inner.agents.changed.notify_all();
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
        Agents::require_live(&state.store, &launch.grant.launch_id)?;
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
            self.authorize_agent_guard(launch)?;
            let mut state = service.state.lock().unwrap();
            healthy_agents(&state)?;
            Agents::require_live(&state.store, id)?;
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
            // Cancellation ticks inspect socket liveness only. Process authority and
            // inbox eligibility are refreshed on state changes, never idle 1 Hz scans.
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
                let (direct, channel_first, channel) = {
                    let state = service.state.lock().unwrap();
                    healthy_agents(&state)?;
                    if (state.revision, ChannelLane::availability(&state, launch)) != revision {
                        continue;
                    }
                    observed_revision = Some(revision);
                    let (channel, busy, _) = revision.1;
                    let agent = &state.store.agents[id];
                    let free = !busy && agent.notification.is_none();
                    let direct = free
                        && state
                            .store
                            .messages
                            .values()
                            .any(|m| m.to == *id && claimable(m));
                    let channel = free && channel && channel_retry.is_none();
                    let channel_first = channel
                        && (!direct || state.channel_lanes[id].last_generation < agent.generation);
                    (direct, channel_first, channel)
                };
                let mut claim_channel = || match self.claim_channel_wake(
                    launch,
                    peer,
                    &receiver,
                    is_connected,
                    deadline,
                )? {
                    ChannelClaimPoll::Ready(wake) => Ok::<_, anyhow::Error>(Some(wake)),
                    ChannelClaimPoll::Empty => {
                        #[cfg(test)]
                        service.wait_channel_barrier(ChannelTestPause::EmptyClaim);
                        Ok(None)
                    }
                    ChannelClaimPoll::Deferred => {
                        channel_retry = Some(Instant::now() + CHANNEL_CLAIM_RETRY);
                        Ok(None)
                    }
                };
                if channel_first && let Some(wake) = claim_channel()? {
                    return Ok(wake);
                }
                if direct {
                    let notification = service.transaction_maybe(|store| {
                        self.authorize_agent_guard(launch)?;
                        self.agent_receiver_peer(launch, peer, &receiver_id)?;
                        let state = service.state.lock().unwrap();
                        if ChannelLane::availability(&state, launch).1 {
                            return Ok((None, false));
                        }
                        if let Some(lane) = state.channel_lanes.get(id) {
                            let agent = store.agents.get_mut(id).expect("admitted live agent");
                            agent.generation = agent.generation.max(lane.last_generation);
                        }
                        drop(state);
                        let notification = messages::claim(store, id, &receiver_id, now())?;
                        let changed = notification.is_some();
                        Ok((notification, changed))
                    })?;
                    if let Some(notification) = notification {
                        self.agent_receiver_peer(launch, peer, &receiver_id)?;
                        return Ok(
                            json!({"notification":{"notification_id":notification.id,"generation":notification.generation,"session_id":receiver.session_id,"mode":receiver.mode,"message_count":notification.message_count,"unread_count":notification.unread_count,"pointer":"Untrusted peers: read_inbox; acknowledge only processed IDs, optionally with the next read."}}),
                        );
                    }
                }
                if channel
                    && !channel_first
                    && let Some(wake) = claim_channel()?
                {
                    return Ok(wake);
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
fn require_scope(scope: AgentScope, own: &ContextFact, _store: &Store) -> Result<()> {
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

fn expire_batch(agent: &mut Agent, timestamp: u64) -> Option<Notification> {
    if agent
        .notification
        .as_ref()
        .is_some_and(|n| wake_deadline(n.claimed_at) <= timestamp)
    {
        return agent.notification.take();
    }
    None
}

fn pending_wake_deadline(message: &Message) -> Option<u64> {
    if message.wake_state == "pending"
        && message.notification_id.is_some()
        && message.receiver_id.is_some()
        && message.generation.is_some()
    {
        return message.wake_claimed_at.map(wake_deadline);
    }
    None
}

fn expire_pending_wake(message: &mut Message, timestamp: u64) -> bool {
    if pending_wake_deadline(message).is_some_and(|deadline| deadline <= timestamp)
        && let Some(generation) = message.generation
    {
        record_unknown(
            message,
            generation,
            "native notification acceptance timed out",
            timestamp,
        );
        return true;
    }
    false
}

fn record_unknown(message: &mut Message, generation: u64, reason: &str, timestamp: u64) {
    message.wake_state = "unknown".into();
    message.wake_reason = Some(reason.into());
    message.wake_failure = Some(WakeFailure {
        state: "unknown".into(),
        reason: reason.into(),
        notification_id: message.notification_id.clone().expect("claimed message"),
        generation,
        recorded_at: timestamp,
    });
}

fn settle_unknown(
    store: &mut Store,
    id: &str,
    receiver_id: &str,
    notification_id: &str,
    generation: u64,
    reason: &str,
    timestamp: u64,
) -> bool {
    let mut changed = false;
    if let Some(agent) = store.agents.get_mut(id)
        && agent.notification.as_ref().is_some_and(|n| {
            n.receiver_id == receiver_id
                && n.id == notification_id
                && n.generation == generation
                && n.state == "pending"
        })
    {
        // Retire only this claim. Its messages keep the historical unknown outcome
        // and notification identity, so they cannot be ambiguously reoffered.
        agent.notification = None;
        changed = true;
    }
    for m in store.messages.values_mut().filter(|m| {
        m.to == id
            && m.receiver_id.as_deref() == Some(receiver_id)
            && m.notification_id.as_deref() == Some(notification_id)
            && m.generation == Some(generation)
            && m.wake_state == "pending"
    }) {
        record_unknown(m, generation, reason, timestamp);
        changed = true;
    }
    changed
}
fn pending_dispatches(store: &Store) -> BTreeSet<(String, String, String, u64)> {
    store
        .messages
        .values()
        .filter(|m| m.wake_state == "pending")
        .filter_map(|m| {
            m.wake_claimed_at?;
            Some((
                m.to.clone(),
                m.receiver_id.clone()?,
                m.notification_id.clone()?,
                m.generation?,
            ))
        })
        .collect()
}
fn next_maintenance(store: &Store) -> Option<u64> {
    let batches = store
        .agents
        .values()
        .filter_map(|a| a.notification.as_ref())
        .map(|n| wake_deadline(n.claimed_at));
    let receipts = store.messages.values().flat_map(|m| {
        retention_deadline(&store.agents, m)
            .into_iter()
            .chain(pending_wake_deadline(m))
    });
    batches.chain(receipts).min()
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
