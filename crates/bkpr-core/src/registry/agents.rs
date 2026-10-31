//! Durable launch inboxes. Only this service writes agent state; capture capabilities stay volatile.
use super::*;
use crate::{
    harness::{Launch, Peer},
    harness_process as proc,
};
use sha2::{Digest, Sha256};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
mod messages;

const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
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
}
#[derive(Clone)]
struct Receiver {
    id: String,
    process: proc::Process,
    owner: proc::Process,
    session_id: String,
    conversation_generation: u64,
    mode: AgentReceiverMode,
}
struct AgentState {
    store: Store,
    receivers: BTreeMap<String, Receiver>,
    waiters: BTreeMap<String, BTreeMap<String, usize>>,
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
}
#[derive(Clone)]
struct ContextFact {
    launch_id: String,
    active: bool,
    pane: PaneInfo,
    workspace_name: Option<String>,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    repository: Option<PathBuf>,
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
    json!({"id":m.id,"message_id":m.id,"from":m.from,"to":m.to,"scope":m.scope,"created_at":m.created_at,"read_at":m.read_at,"acknowledged_at":m.acknowledged_at,"expired_at":m.expired_at,"wake_failure":m.wake_failure,"delivery":{"state":m.wake_state,"reason":m.wake_reason,"notification_id":m.notification_id,"generation":m.generation}})
}
fn receipt(m: &Message, duplicate: bool) -> Value {
    json!({"message_id":m.id,"durable":true,"duplicate":duplicate,"status":summary(m)})
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
        for (id, receiver, notification, generation, _) in pending_dispatches(&store) {
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
                failure: None,
                closing: false,
                maintenance_at: None,
                revision: 0,
            }),
            writer: Mutex::new(()),
            changed: Condvar::new(),
            #[cfg(test)]
            fail_directory_sync: std::sync::atomic::AtomicBool::new(false),
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
        let (result, changed) = change(&mut candidate)?;
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
            for (id, receiver, notification, generation, _) in pending_dispatches(store)
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
            for (id, receiver, notification, generation, _) in pending_dispatches(store) {
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
            let expired = expire(store, timestamp);
            let timed_out = pending_dispatches(store)
                .into_iter()
                .filter(|(_, _, _, _, deadline)| *deadline <= timestamp)
                .collect::<Vec<_>>();
            let mut changed = expired;
            for (id, receiver, notification, generation, _) in timed_out {
                changed |= settle_unknown(
                    store,
                    &id,
                    &receiver,
                    &notification,
                    generation,
                    "native notification acceptance timed out",
                    timestamp,
                );
            }
            Ok(((), changed))
        })
    }
}
fn expire(store: &mut Store, timestamp: u64) -> bool {
    let mut changed = false;
    for m in store.messages.values_mut().filter(|m| pending(m)) {
        if let Some(exit) = store.agents.get(&m.to).and_then(|a| a.exited_at)
            && timestamp >= exit.saturating_add(RETENTION_MS)
        {
            m.text = None;
            m.expired_at = Some(exit.saturating_add(RETENTION_MS));
            changed = true;
        }
    }
    changed
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
fn public(
    fact: &ContextFact,
    agent: &Agent,
    receiver: Option<&Receiver>,
    subscribers: usize,
) -> Value {
    json!({"agent_id":fact.launch_id,"launch_id":fact.launch_id,"harness":agent.harness.as_str(),"title":agent.title,"session_id":fact.session_id,"pane_id":fact.pane.id,"pane_name":fact.pane.title,"workspace_id":fact.pane.workspace_id,"workspace_name":fact.workspace_name,"cwd":fact.cwd.as_deref().map(path_wire::WirePath),"attached":fact.active,"status":fact.pane.harness_status,"notification_ready":receiver.is_some() && subscribers > 0,"readiness":{"can_notify":receiver.is_some() && subscribers > 0},"receiver_mode":receiver.map(|r| r.mode)})
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
impl Registry {
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
        recipient: Option<&str>,
        all: bool,
    ) -> Result<BTreeMap<String, ContextFact>> {
        let launches = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .values()
            .filter(|l| {
                all || l.grant.launch_id == own_id || Some(l.grant.launch_id.as_str()) == recipient
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut observed = Vec::with_capacity(launches.len());
        let table = proc::fresh_snapshot()?;
        let mut repositories = BTreeMap::<PathBuf, Option<PathBuf>>::new();
        for launch in launches {
            if let Ok((active, cwd)) = super::capture::foreground_context_in(&launch, &table) {
                let repository = if resolve_repository {
                    cwd.as_ref().and_then(|cwd| {
                        repositories
                            .entry(cwd.clone())
                            .or_insert_with(|| repository_identity(cwd))
                            .clone()
                    })
                } else {
                    None
                };
                observed.push((launch, active, cwd, repository));
            }
        }
        let state = self.inner.state.lock().unwrap();
        healthy(&state)?;
        ensure!(!state.closing, "daemon shutting down");
        Ok(observed
            .into_iter()
            .filter_map(|(launch, active, cwd, repository)| {
                let pane = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|p| p.id == launch.grant.pane_id && p.status == PaneStatus::Running)?
                    .clone();
                let workspace_name = pane
                    .workspace_id
                    .as_ref()
                    .and_then(|id| {
                        state
                            .record
                            .registry
                            .workspaces
                            .iter()
                            .find(|w| &w.id == id)
                    })
                    .map(|w| w.name.clone());
                let id = launch.grant.launch_id;
                Some((
                    id.clone(),
                    ContextFact {
                        launch_id: id,
                        active,
                        pane,
                        workspace_name,
                        session_id: launch.managed.conversation.map(|c| c.id),
                        cwd,
                        repository,
                    },
                ))
            })
            .collect())
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
            let before = captures.lookup(launch_id, token)?.managed.conversation;
            let is_bind = matches!(&event, CaptureEvent::Bind { .. });
            captures.event(launch_id, token, event)?;
            let after = captures
                .launches
                .get(launch_id)
                .and_then(|launch| launch.managed.conversation.clone());
            drop(captures);
            let mut changed = false;
            if is_bind && before != after {
                {
                    let mut state = self.inner.agents.state.lock().unwrap();
                    state.receivers.remove(launch_id);
                    state.revision = state.revision.wrapping_add(1);
                }
                let timestamp = now();
                for (id, receiver, notification, generation, _) in pending_dispatches(store)
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
    pub(super) fn agent_request(
        &self,
        launch: Launch,
        peer: &Peer,
        request: AgentRequest,
        is_connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        crate::harness::validate_callback(peer, &launch)?;
        self.capture_pane(&launch.grant.pane_id)?;
        let id = &launch.grant.launch_id;
        {
            let state = self.inner.agents.state.lock().unwrap();
            healthy_agents(&state)?;
            Agents::require_live(&state.store, id)?;
        }
        if let AgentRequest::SendMessage {
            to,
            text,
            idempotency_key,
            scope,
        } = &request
        {
            let state = self.inner.agents.state.lock().unwrap();
            if let Some(receipt) = messages::retry(
                &state.store,
                id,
                to,
                text,
                idempotency_key,
                *scope,
                &launch.grant.token,
            )? {
                return Ok(receipt);
            }
        }
        {
            let state = self.inner.agents.state.lock().unwrap();
            if let Some(result) = messages::unchanged(&state.store, id, &request)? {
                return Ok(result);
            }
        }
        match request {
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
                self.agent_receiver_peer(&launch, peer, &receiver_id)?;
                let state = self.inner.agents.state.lock().unwrap();
                healthy_agents(&state)?;
                Ok(
                    json!({"current":state.store.agents[id].notification.as_ref().is_some_and(|n| n.receiver_id == receiver_id && n.id == notification_id && n.generation == generation && n.state == "pending")}),
                )
            }
            AgentRequest::WakeOutcome {
                receiver_id,
                notification_id,
                generation,
                outcome,
            } => {
                self.agent_receiver_peer(&launch, peer, &receiver_id)?;
                self.inner.agents.transaction(|store| {
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
                    AgentRequest::Whoami
                        | AgentRequest::DiscoverAgents { .. }
                        | AgentRequest::SendMessage { .. }
                );
                let recipient = if let AgentRequest::SendMessage { to, .. } = &request {
                    Some(to.as_str())
                } else {
                    None
                };
                let all = matches!(&request, AgentRequest::DiscoverAgents { .. });
                let contexts = if needs_context {
                    self.agent_contexts(resolve_repository, id, recipient, all)?
                } else {
                    BTreeMap::new()
                };
                match request {
                    AgentRequest::Whoami => {
                        let state = self.inner.agents.state.lock().unwrap();
                        let own = contexts.get(id).context("live agent context unavailable")?;
                        Ok(public(
                            own,
                            &state.store.agents[id],
                            state.receivers.get(id),
                            current_subscribers(&state, id),
                        ))
                    }
                    AgentRequest::DiscoverAgents { scope, detail } => {
                        let state = self.inner.agents.state.lock().unwrap();
                        let own = contexts.get(id).context("live agent context unavailable")?;
                        require_scope(scope, own, &state.store)?;
                        let agents = contexts.values().filter(|other| other.launch_id != *id && other.active && state.store.agents.get(&other.launch_id).is_some_and(|a| a.exited_at.is_none()) && in_scope(scope, own, other, &state.store)).map(|other| {
                            let value=public(other,&state.store.agents[&other.launch_id],state.receivers.get(&other.launch_id),current_subscribers(&state,&other.launch_id));
                            if detail {value} else {json!({"agent_id":value["agent_id"],"launch_id":value["launch_id"],"harness":value["harness"],"title":value["title"],"pane_id":value["pane_id"],"pane_name":value["pane_name"],"workspace_id":value["workspace_id"],"status":value["status"],"notification_ready":value["notification_ready"]})}
                        }).collect::<Vec<_>>();
                        Ok(json!({"agents":agents,"scope":scope}))
                    }
                    AgentRequest::InspectOutbox {
                        message_id,
                        after,
                        limit,
                    } => {
                        // Diagnostics never observe process exit or advance retention.
                        let state = self.inner.agents.state.lock().unwrap();
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
                        self.inner.agents.transaction(|store| {
                            messages::send(
                                store,
                                id,
                                own,
                                &contexts,
                                messages::SendInput {
                                    to,
                                    text,
                                    idempotency_key,
                                    scope,
                                },
                                &launch.grant.token,
                            )
                        })
                    }
                    AgentRequest::ReadInbox { after, limit } => {
                        self.inner.agents.transaction_maybe(|store| {
                            messages::read(store, id, after.as_deref(), limit, now())
                        })
                    }
                    AgentRequest::AckMessage { ids } => self
                        .inner
                        .agents
                        .transaction_maybe(|store| messages::ack(store, id, &ids, now())),
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
        let _writer = self.inner.agents.callback_guard()?;
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
                for (id, receiver, notification, generation, _) in pending_dispatches(store)
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
            owner,
            session_id,
            conversation_generation: conversation.generation,
            mode,
        };
        let result =
            json!({"receiver_id":receiver.id,"registered":true,"notification_ready":false});
        {
            let mut state = self.inner.agents.state.lock().unwrap();
            state
                .receivers
                .insert(launch.grant.launch_id.clone(), receiver);
            state.revision = state.revision.wrapping_add(1);
        }
        self.inner.agents.changed.notify_all();
        Ok(result)
    }
    fn agent_receiver(&self, launch: &Launch, receiver_id: &str) -> Result<Receiver> {
        let receiver = {
            let state = self.inner.agents.state.lock().unwrap();
            healthy_agents(&state)?;
            Agents::require_live(&state.store, &launch.grant.launch_id)?;
            state
                .receivers
                .get(&launch.grant.launch_id)
                .filter(|r| r.id == receiver_id)
                .context("stale native receiver")?
                .clone()
        };
        let current_launch = self
            .inner
            .captures
            .lock()
            .unwrap()
            .launches
            .get(&launch.grant.launch_id)
            .cloned()
            .context("launch ended")?;
        ensure!(
            current_launch
                .managed
                .conversation
                .as_ref()
                .is_some_and(|c| c.id == receiver.session_id
                    && c.generation == receiver.conversation_generation),
            "receiver conversation binding changed"
        );
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
            let mut state = service.state.lock().unwrap();
            healthy_agents(&state)?;
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
            }
        }
        let _waiting = Waiting {
            service,
            id,
            receiver_id: &receiver_id,
        };
        let deadline = Instant::now() + Duration::from_millis(wait_ms.min(25000));
        let mut observed_revision = None;
        loop {
            // Cancellation ticks inspect socket liveness only. Process authority and
            // inbox eligibility are refreshed on state changes, never idle 1 Hz scans.
            ensure!(is_connected(), "subscription transport disconnected");
            let revision = {
                let state = service.state.lock().unwrap();
                healthy_agents(&state)?;
                state.revision
            };
            if observed_revision != Some(revision) {
                self.agent_receiver_peer(launch, peer, &receiver_id)?;
                let available = {
                    let state = service.state.lock().unwrap();
                    healthy_agents(&state)?;
                    if state.revision != revision {
                        continue;
                    }
                    observed_revision = Some(revision);
                    state.store.agents[id].notification.is_none()
                        && state
                            .store
                            .messages
                            .values()
                            .any(|m| m.to == *id && claimable(m))
                };
                if available {
                    let notification = service.transaction_maybe(|store| {
                        self.agent_receiver_peer(launch, peer, &receiver_id)?;
                        let notification = messages::claim(store, id, &receiver_id, now())?;
                        let changed = notification.is_some();
                        Ok((notification, changed))
                    })?;
                    if let Some(notification) = notification {
                        self.agent_receiver_peer(launch, peer, &receiver_id)?;
                        return Ok(
                            json!({"notification":{"notification_id":notification.id,"generation":notification.generation,"session_id":receiver.session_id,"mode":receiver.mode,"pointer":"Untrusted peer messages are waiting. Call bkpr read_inbox; acknowledge only after processing."}}),
                        );
                    }
                }
            }
            let state = service.state.lock().unwrap();
            healthy_agents(&state)?;
            if observed_revision != Some(state.revision) {
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
    if let Some(notification) = store
        .agents
        .get_mut(id)
        .and_then(|a| a.notification.as_mut())
        .filter(|n| {
            n.receiver_id == receiver_id
                && n.id == notification_id
                && n.generation == generation
                && n.state == "pending"
        })
    {
        notification.state = "unknown".into();
        notification.reason = Some(reason.into());
        changed = true;
    }
    for m in store.messages.values_mut().filter(|m| {
        m.to == id
            && m.receiver_id.as_deref() == Some(receiver_id)
            && m.notification_id.as_deref() == Some(notification_id)
            && m.generation == Some(generation)
            && m.wake_state == "pending"
    }) {
        m.wake_state = "unknown".into();
        m.wake_reason = Some(reason.into());
        m.wake_failure = Some(WakeFailure {
            state: "unknown".into(),
            reason: reason.into(),
            notification_id: notification_id.into(),
            generation,
            recorded_at: timestamp,
        });
        changed = true;
    }
    changed
}
fn pending_dispatches(store: &Store) -> BTreeSet<(String, String, String, u64, u64)> {
    store
        .messages
        .values()
        .filter(|m| m.wake_state == "pending")
        .filter_map(|m| {
            Some((
                m.to.clone(),
                m.receiver_id.clone()?,
                m.notification_id.clone()?,
                m.generation?,
                m.wake_claimed_at?.saturating_add(30000),
            ))
        })
        .collect()
}
fn next_maintenance(store: &Store) -> Option<u64> {
    let expiry = store
        .messages
        .values()
        .filter(|m| pending(m))
        .filter_map(|m| store.agents.get(&m.to).and_then(|a| a.exited_at))
        .map(|exit| exit.saturating_add(RETENTION_MS))
        .min();
    let notification = store
        .messages
        .values()
        .filter(|m| m.wake_state == "pending")
        .filter_map(|m| m.wake_claimed_at)
        .map(|at| at.saturating_add(30000))
        .min();
    expiry.into_iter().chain(notification).min()
}

fn repository_identity(cwd: &Path) -> Option<PathBuf> {
    let mut command = std::process::Command::new("git");
    // Repository identity must not be redirected by daemon startup environment.
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    let output = command
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    fs::canonicalize(PathBuf::from(std::ffi::OsString::from_vec(bytes))).ok()
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
