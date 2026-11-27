use super::*;
use crate::registry::sessions::ClientLease;
use bkpr_protocol::channels::{ChannelId, ChannelMentionBinding, Participant};
use bkpr_protocol::roles::{
    APPROVAL_TIMEOUT_MS, RoleApproval, RoleEvent, RoleEventKind, RoleGrant, RoleGrantId,
    RoleOrigin, RoleRequest, RoleRequestId, RoleResponse, RoleWake, RoleWakeChange, WorkspaceRole,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MAX_PENDING: usize = 64;
const MAX_RECEIPTS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RequestState {
    Pending,
    Approved,
    Denied,
    Expired,
    Retired,
    Superseded,
}

struct Pending {
    prompt: RoleApproval,
    deadline: Instant,
    state: RequestState,
    deciding: bool,
}

impl Pending {
    fn receipt(&self) -> Value {
        json!({"request_id": self.prompt.id, "state": self.state, "expires_at_ms": self.prompt.expires_at_ms})
    }
}

#[derive(Default)]
struct Approvals {
    // One latest receipt per exact launch. Terminal receipts cannot be decided again.
    requests: BTreeMap<String, Pending>,
    activity: BTreeMap<String, u64>,
    activity_order: u64,
    recipient: Option<String>,
}

impl Approvals {
    fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for request in self.requests.values_mut() {
            if request.state == RequestState::Pending
                && !request.deciding
                && now >= request.deadline
            {
                request.state = RequestState::Expired;
                changed = true;
            }
        }
        changed
    }

    fn deadline(&self) -> Option<Instant> {
        self.requests
            .values()
            .filter(|r| r.state == RequestState::Pending && !r.deciding)
            .map(|r| r.deadline)
            .min()
    }

    fn prior_holder_changed(&mut self, workspace: &str, current: Option<&RoleGrant>) {
        for pending in self
            .requests
            .values_mut()
            .filter(|r| r.state == RequestState::Pending && r.prompt.workspace_id == workspace)
        {
            if current
                .is_some_and(|grant| grant.holder.launch_id == pending.prompt.requester.launch_id)
            {
                pending.state = RequestState::Approved;
            } else {
                pending.prompt.prior_holder = current.cloned();
            }
        }
    }
}

#[derive(Default)]
pub(in crate::registry) struct RoleService {
    approvals: Mutex<Approvals>,
    revision: AtomicU64,
    grants_present: AtomicBool,
    check_retired: AtomicBool,
    wake: std::sync::OnceLock<crossbeam_channel::Sender<()>>,
}

impl RoleService {
    pub(in crate::registry) fn start(&self, registry: &Registry) -> Result<()> {
        let (wake, events) = crossbeam_channel::bounded(1);
        let inner = Arc::downgrade(&registry.inner);
        std::thread::Builder::new()
            .name("bkpr-role-deadlines".into())
            .spawn(move || {
                let mut deadline = None;
                loop {
                    let result = if let Some(deadline) = deadline {
                        events.recv_deadline(deadline)
                    } else {
                        events
                            .recv()
                            .map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected)
                    };
                    if result == Err(crossbeam_channel::RecvTimeoutError::Disconnected) {
                        break;
                    }
                    let Some(inner) = inner.upgrade() else {
                        break;
                    };
                    let registry = Registry {
                        inner,
                        fresh: false,
                        lifecycle_owner: false,
                    };
                    if registry.session_end_requested() {
                        break;
                    }
                    deadline = registry.maintain_roles_at(Instant::now());
                }
            })?;
        let _ = self.wake.set(wake);
        Ok(())
    }

    fn signal(&self) {
        if let Some(wake) = self.wake.get() {
            let _ = wake.try_send(());
        }
    }
}

impl Registry {
    pub(in crate::registry) fn role_roster_changed(&self) {
        self.publish_role_event(RoleEventKind::Roster, None);
    }

    pub(in crate::registry) fn role_launches_changed(&self) {
        self.role_roster_changed();
        let roles = &self.inner.roles;
        roles.check_retired.store(true, Ordering::Release);
        roles.signal();
    }

    fn publish_role_event(&self, kind: RoleEventKind, only: Option<&str>) {
        let clients = self.inner.clients.lock().unwrap();
        let selected: Vec<_> = if let Some(id) = only {
            clients
                .get(id)
                .filter(|client| client.role_events())
                .cloned()
                .into_iter()
                .collect()
        } else {
            clients
                .values()
                .filter(|client| client.role_events())
                .cloned()
                .collect()
        };
        drop(clients);
        if selected.is_empty() {
            return;
        }
        let revision = self.inner.roles.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let event = ClientEvent::roles_changed(RoleEvent { revision, kind });
        let mut codec = bkpr_protocol::wire::Codec::new();
        if let Ok(frame) = codec.encode_client_event(&event).and_then(|payload| {
            bkpr_protocol::wire::encode_frame(bkpr_protocol::wire::Kind::ClientEvent, payload)
        }) {
            for client in selected {
                client.send_role_event(&frame);
            }
        }
    }

    fn publish_approval_event(&self, recipient: Option<&str>) {
        if let Some(id) = recipient {
            self.publish_role_event(RoleEventKind::Approvals, Some(id));
        }
    }

    fn select_role_recipient(&self, approvals: &mut Approvals) -> Option<String> {
        let clients = self.inner.clients.lock().unwrap();
        approvals.recipient = approvals
            .activity
            .iter()
            .filter(|(id, _)| {
                clients
                    .get(id.as_str())
                    .is_some_and(|client| client.role_connected())
            })
            .max_by_key(|(_, order)| *order)
            .map(|(id, _)| id.clone());
        approvals.recipient.clone()
    }

    pub(in crate::registry) fn role_client_detached(&self, id: &str) {
        let mut approvals = self.inner.roles.approvals.lock().unwrap();
        approvals.activity.remove(id);
        if approvals.recipient.as_deref() != Some(id) {
            return;
        }
        let recipient = self.select_role_recipient(&mut approvals);
        let changed = approvals.deadline().is_some();
        drop(approvals);
        if changed {
            self.publish_approval_event(recipient.as_deref());
        }
    }

    fn role_activity(
        &self,
        client: &ClientLease,
        eligible: bool,
        connected: &dyn Fn() -> bool,
    ) -> Result<RoleResponse> {
        let mut approvals = self.inner.roles.approvals.lock().unwrap();
        ensure!(connected(), "role attachment disconnected");
        client.enable_role_events();
        let was_selected = approvals.recipient.as_deref() == Some(&client.id);
        if eligible {
            approvals.activity_order += 1;
            let order = approvals.activity_order;
            if let Some(current) = approvals.activity.get_mut(&client.id) {
                *current = order;
            } else {
                approvals.activity.insert(client.id.clone(), order);
            }
            if !was_selected {
                let before = approvals.recipient.replace(client.id.clone());
                let changed = approvals.deadline().is_some();
                drop(approvals);
                if changed {
                    self.publish_approval_event(before.as_deref());
                    self.publish_approval_event(Some(&client.id));
                }
            }
        } else {
            approvals.activity.remove(&client.id);
            if was_selected {
                let after = self.select_role_recipient(&mut approvals);
                let changed = approvals.deadline().is_some();
                drop(approvals);
                if changed {
                    self.publish_approval_event(Some(&client.id));
                    self.publish_approval_event(after.as_deref());
                }
            }
        }
        Ok(RoleResponse::Activity {})
    }

    pub(in crate::registry) fn request_roles(
        &self,
        client: &ClientLease,
        request: RoleRequest,
        connected: &dyn Fn() -> bool,
    ) -> Result<Payload> {
        request.validate()?;
        if let RoleRequest::Activity { eligible } = request {
            return self
                .role_activity(client, eligible, connected)
                .map(Payload::Roles);
        }
        let roles = &self.inner.roles;
        let response = self.inner.channels.with_role_store(
            &|| !connected() || self.session_end_requested(),
            |store| {
                ensure!(connected(), "role attachment disconnected");
                match request {
                    RoleRequest::Snapshot {} => {
                        let approvals = roles.approvals.lock().unwrap();
                        let selected = approvals.recipient.as_deref() == Some(&client.id);
                        let now = Instant::now();
                        let prompts = if selected {
                            approvals
                                .requests
                                .values()
                                .filter(|r| {
                                    r.state == RequestState::Pending
                                        && !r.deciding
                                        && now < r.deadline
                                })
                                .map(|r| r.prompt.clone())
                                .collect()
                        } else {
                            Vec::new()
                        };
                        Ok(RoleResponse::Snapshot {
                            revision: roles.revision.load(Ordering::Acquire),
                            grants: store
                                .role_grants()
                                .filter(|g| self.role_holder_alive(g))
                                .cloned()
                                .collect(),
                            approvals: prompts,
                        })
                    }
                    RoleRequest::Set {
                        workspace_id,
                        role,
                        target,
                    } => {
                        let _binding =
                            self.fence_user_mentions(&workspace_id, target.as_slice())?;
                        self.require_role_client(client, connected)?;
                        let grant = if let Some(target) = target {
                            if let Some(grant) = store
                                .role_grant(&workspace_id)
                                .filter(|grant| grant.holder.launch_id == target.launch_id)
                            {
                                return Ok(RoleResponse::Updated {
                                    grant: Some(grant.clone()),
                                });
                            }
                            let (participant, _) =
                                self.resolve_role_target(&workspace_id, &target)?;
                            let grant = new_grant(
                                workspace_id.clone(),
                                role,
                                target,
                                participant.display_name,
                                RoleOrigin::Command,
                            );
                            let former = store.set_role(grant.clone())?;
                            self.role_grant_changed(&workspace_id, former, Some(&grant));
                            roles.grants_present.store(true, Ordering::Release);
                            Some(grant)
                        } else {
                            let former = store.remove_role(&workspace_id);
                            if former.is_some() {
                                self.role_grant_changed(&workspace_id, former, None);
                            }
                            None
                        };
                        roles
                            .grants_present
                            .store(store.role_grants().next().is_some(), Ordering::Release);
                        Ok(RoleResponse::Updated { grant })
                    }
                    RoleRequest::Decide {
                        request_id,
                        requester,
                        expected_holder,
                        approve,
                    } => {
                        let workspace = {
                            let approvals = roles.approvals.lock().unwrap();
                            approvals
                                .requests
                                .get(&requester.launch_id)
                                .filter(|r| r.prompt.id == request_id)
                                .map(|r| r.prompt.workspace_id.clone())
                                .context("role approval is no longer pending")?
                        };
                        let _binding =
                            self.fence_user_mentions(&workspace, std::slice::from_ref(&requester))?;
                        let (role, label, prior) = {
                            let mut approvals = roles.approvals.lock().unwrap();
                            ensure!(
                                connected() && approvals.recipient.as_deref() == Some(&client.id),
                                "role approval is assigned to another attachment"
                            );
                            let pending = approvals
                                .requests
                                .get_mut(&requester.launch_id)
                                .context("role approval is no longer pending")?;
                            let prior = store
                                .role_grant(&workspace)
                                .filter(|g| self.role_holder_alive(g));
                            ensure!(
                                pending.state == RequestState::Pending
                                    && !pending.deciding
                                    && Instant::now() < pending.deadline
                                    && pending.prompt.id == request_id
                                    && pending.prompt.requester == requester
                                    && pending.prompt.prior_holder.as_ref().map(|g| &g.id)
                                        == expected_holder.as_ref()
                                    && prior.map(|g| &g.id) == expected_holder.as_ref(),
                                "role approval changed or expired; refresh before deciding"
                            );
                            if approve {
                                pending.deciding = true;
                            } else {
                                pending.state = RequestState::Denied;
                            }
                            (
                                pending.prompt.role,
                                pending.prompt.participant.display_name.clone(),
                                prior.cloned(),
                            )
                        };
                        if !approve {
                            self.publish_approval_event(Some(&client.id));
                            roles.signal();
                            return Ok(RoleResponse::Updated { grant: prior });
                        }
                        let grant = new_grant(
                            workspace.clone(),
                            role,
                            requester,
                            label,
                            RoleOrigin::Approval,
                        );
                        let prepared = store.set_role(grant.clone());
                        {
                            let mut approvals = roles.approvals.lock().unwrap();
                            let pending = approvals
                                .requests
                                .get_mut(&grant.holder.launch_id)
                                .expect("reserved decision");
                            pending.deciding = false;
                            if prepared.is_ok() {
                                pending.state = RequestState::Approved;
                            }
                        }
                        roles.signal();
                        let former = prepared?;
                        roles.grants_present.store(true, Ordering::Release);
                        self.role_grant_changed(&workspace, former, Some(&grant));
                        Ok(RoleResponse::Updated { grant: Some(grant) })
                    }
                    RoleRequest::Activity { .. } => unreachable!(),
                }
            },
        )?;
        Ok(Payload::Roles(response))
    }

    fn require_role_client(
        &self,
        client: &ClientLease,
        connected: &dyn Fn() -> bool,
    ) -> Result<()> {
        let approvals = self.inner.roles.approvals.lock().unwrap();
        ensure!(
            connected() && approvals.activity.contains_key(&client.id),
            "role command requires an eligible input attachment"
        );
        Ok(())
    }

    fn resolve_role_target(
        &self,
        workspace: &str,
        target: &ChannelMentionBinding,
    ) -> Result<(Participant, String)> {
        let state = self.inner.state.lock().unwrap();
        let name = state
            .record
            .registry
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .filter(|_| !state.closing_workspaces.contains(workspace))
            .context("role workspace unavailable")?;
        let agents = self.inner.agents.state.lock().unwrap();
        let captures = self.inner.captures.lock().unwrap();
        let launch = captures
            .launches
            .get(&target.launch_id)
            .context("role target is not live")?;
        let pane =
            channel_pane(&state, &launch.grant.pane_id).context("role target pane is not live")?;
        let (current, agent) = live_channel_agent(
            &state,
            pane,
            workspace,
            &agents,
            &captures,
            Some(&target.participant_id),
        )
        .filter(|(current, _)| {
            std::ptr::eq(*current, launch)
                && current.binding_generation == target.binding_generation
        })
        .context("role target binding changed")?;
        Ok((
            channel_agent(current, agent, &state, pane),
            name.name.clone(),
        ))
    }

    fn role_workspace_open(&self, id: &str) -> bool {
        workspace_exists(&self.inner.state.lock().unwrap(), id)
    }

    fn role_holder_alive(&self, grant: &RoleGrant) -> bool {
        self.role_workspace_open(&grant.workspace_id)
            && self.role_launch_alive(&grant.holder.launch_id)
    }

    fn role_launch_alive(&self, id: &str) -> bool {
        let agents = self.inner.agents.state.lock().unwrap();
        agents.agents.contains_key(id)
            && self
                .inner
                .captures
                .lock()
                .unwrap()
                .launches
                .get(id)
                .is_some_and(Launch::channel_launch_alive)
    }

    fn agent_role_target(
        &self,
        launch: &Launch,
    ) -> Result<(String, ChannelMentionBinding, Participant, String)> {
        self.authorize_agent_guard(launch)?;
        let workspace = self.channel_caller_workspace(launch)?;
        let target = ChannelMentionBinding {
            participant_id: launch
                .channel_participant
                .clone()
                .context("no live channel participant")?,
            launch_id: launch.grant.launch_id.clone(),
            binding_generation: launch.binding_generation,
        };
        let (participant, name) = self.resolve_role_target(&workspace, &target)?;
        Ok((workspace, target, participant, name))
    }

    pub(super) fn agent_role_request(
        &self,
        launch: &Launch,
        role: WorkspaceRole,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        let roles = &self.inner.roles;
        self.inner.channels.with_role_store(
            &|| !connected() || self.session_end_requested(),
            |store| {
                ensure!(
                    connected() && !self.session_end_requested(),
                    "role requester disconnected"
                );
                let (workspace, target, participant, workspace_name) =
                    self.agent_role_target(launch)?;
                let prior = store
                    .role_grant(&workspace)
                    .filter(|g| self.role_holder_alive(g));
                ensure!(
                    prior.is_none_or(|g| g.holder.launch_id != target.launch_id),
                    "launch already holds that role"
                );
                let clock = Instant::now();
                let mut approvals = roles.approvals.lock().unwrap();
                let is_pending = |r: &Pending| {
                    r.state == RequestState::Pending && (r.deciding || clock < r.deadline)
                };
                if let Some(pending) = approvals.requests.get(&target.launch_id)
                    && is_pending(pending)
                    && pending.prompt.requester == target
                {
                    return Ok(pending.receipt());
                }
                ensure!(
                    approvals
                        .requests
                        .values()
                        .filter(|r| is_pending(r))
                        .count()
                        < MAX_PENDING,
                    "role approval capacity exhausted"
                );
                if approvals.requests.len() >= MAX_RECEIPTS
                    && !approvals.requests.contains_key(&target.launch_id)
                {
                    let evict = approvals
                        .requests
                        .iter()
                        .filter(|(_, r)| !is_pending(r))
                        .min_by_key(|(_, r)| r.deadline)
                        .map(|(key, _)| key.clone())
                        .expect("a terminal receipt");
                    approvals.requests.remove(&evict);
                }
                let launch_id = target.launch_id.clone();
                let pending = Pending {
                    prompt: RoleApproval {
                        id: RoleRequestId(Uuid::new_v4().to_string()),
                        workspace_id: workspace,
                        role,
                        requester: target,
                        participant,
                        prior_holder: prior.cloned(),
                        workspace_name,
                        expires_at_ms: now().saturating_add(APPROVAL_TIMEOUT_MS),
                    },
                    deadline: clock + Duration::from_millis(APPROVAL_TIMEOUT_MS),
                    state: RequestState::Pending,
                    deciding: false,
                };
                let receipt = pending.receipt();
                approvals.requests.insert(launch_id, pending);
                let recipient = self.select_role_recipient(&mut approvals);
                drop(approvals);
                roles.signal();
                self.publish_approval_event(recipient.as_deref());
                Ok(receipt)
            },
        )
    }

    pub(super) fn agent_role_status(
        &self,
        launch: &Launch,
        role: WorkspaceRole,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        self.inner.channels.with_role_store(&|| !connected() || self.session_end_requested(), |store| {
            ensure!(connected(), "role requester disconnected");
            let (workspace, target, _, _) = self.agent_role_target(launch)?;
            let grant = store.role_grant(&workspace).filter(|g| g.holder.launch_id == target.launch_id);
            let approvals = self.inner.roles.approvals.lock().unwrap();
            let request = approvals.requests.get(&target.launch_id).filter(|r| r.prompt.requester == target)
                .map(|r| if r.state == RequestState::Pending && !r.deciding && Instant::now() >= r.deadline {
                    json!({"request_id": r.prompt.id, "state": RequestState::Expired, "expires_at_ms": r.prompt.expires_at_ms})
                } else { r.receipt() });
            Ok(json!({"workspace_id": workspace, "role": role, "request": request, "grant": grant}))
        })
    }

    pub(super) fn agent_role_release(
        &self,
        launch: &Launch,
        role: WorkspaceRole,
        id: RoleGrantId,
        connected: &dyn Fn() -> bool,
    ) -> Result<Value> {
        self.inner.channels.with_role_store(
            &|| !connected() || self.session_end_requested(),
            |store| {
                let (workspace, target, _, _) = self.agent_role_target(launch)?;
                let _binding =
                    self.fence_user_mentions(&workspace, std::slice::from_ref(&target))?;
                ensure!(connected(), "role requester disconnected");
                ensure!(
                    store
                        .role_grant(&workspace)
                        .is_some_and(|grant| grant.id == id
                            && grant.role == role
                            && grant.holder.launch_id == target.launch_id),
                    "role tenure is not current for this launch"
                );
                let former = store.remove_role(&workspace);
                self.inner
                    .roles
                    .grants_present
                    .store(store.role_grants().next().is_some(), Ordering::Release);
                self.role_grant_changed(&workspace, former, None);
                Ok(json!({"released": true}))
            },
        )
    }

    fn role_grant_changed(
        &self,
        workspace: &str,
        former: Option<RoleGrant>,
        active: Option<&RoleGrant>,
    ) {
        let recipient = {
            let mut approvals = self.inner.roles.approvals.lock().unwrap();
            approvals.prior_holder_changed(workspace, active);
            approvals.recipient.clone()
        };
        if let Some(former) = former.as_ref() {
            self.queue_role_wake(former, RoleWakeChange::Revoked);
        }
        if let Some(active) = active {
            self.queue_role_wake(active, RoleWakeChange::Granted);
        }
        self.inner.channels.invalidate();
        self.role_roster_changed();
        self.publish_approval_event(recipient.as_deref());
        self.inner.roles.signal();
    }

    pub(in crate::registry) fn maintain_roles_at(&self, clock: Instant) -> Option<Instant> {
        let roles = &self.inner.roles;
        let dirty = roles.check_retired.swap(false, Ordering::AcqRel);
        let (deadline, recipient, changed) = {
            let mut approvals = roles.approvals.lock().unwrap();
            let mut changed = approvals.expire(clock);
            if dirty {
                for pending in approvals
                    .requests
                    .values_mut()
                    .filter(|r| r.state == RequestState::Pending && !r.deciding)
                {
                    let target = &pending.prompt.requester;
                    let state = if !self.role_launch_alive(&target.launch_id) {
                        RequestState::Retired
                    } else if !self
                        .channel_binding_live(&target.launch_id, target.binding_generation)
                    {
                        RequestState::Superseded
                    } else {
                        continue;
                    };
                    pending.state = state;
                    changed = true;
                }
            }
            (approvals.deadline(), approvals.recipient.clone(), changed)
        };
        if changed {
            self.publish_approval_event(recipient.as_deref());
        }
        if dirty && roles.grants_present.load(Ordering::Acquire) {
            let result =
                self.inner
                    .channels
                    .with_role_store(&|| self.session_end_requested(), |store| {
                        let retired: Vec<_> = store
                            .role_grants()
                            .filter(|g| !self.role_holder_alive(g))
                            .map(|g| g.workspace_id.clone())
                            .collect();
                        for workspace in retired {
                            let former = store.remove_role(&workspace);
                            self.role_grant_changed(&workspace, former, None);
                        }
                        roles
                            .grants_present
                            .store(store.role_grants().next().is_some(), Ordering::Release);
                        Ok(())
                    });
            if result.is_err() {
                roles.check_retired.store(true, Ordering::Release);
                let retry = Instant::now() + Duration::from_millis(250);
                return Some(deadline.map_or(retry, |deadline| deadline.min(retry)));
            }
        }
        deadline
    }

    fn queue_role_wake(&self, grant: &RoleGrant, change: RoleWakeChange) {
        if !self.role_launch_alive(&grant.holder.launch_id) {
            return;
        }
        self.inner.agents.publish_role_wake(
            &grant.holder.launch_id,
            RoleWake {
                workspace_id: grant.workspace_id.clone(),
                role: grant.role,
                grant_id: grant.id.clone(),
                change,
            },
        );
    }
}

fn new_grant(
    workspace_id: String,
    role: WorkspaceRole,
    holder: ChannelMentionBinding,
    display_name: String,
    origin: RoleOrigin,
) -> RoleGrant {
    RoleGrant {
        id: RoleGrantId(Uuid::new_v4().to_string()),
        workspace_id,
        role,
        holder,
        display_name,
        origin,
        granted_at_ms: now(),
        channel_id: ChannelId(Uuid::new_v4().to_string()),
    }
}
