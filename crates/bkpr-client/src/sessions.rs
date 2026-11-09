use super::*;
use crate::attachments::SessionTarget;
use crate::transport::Lifecycle;
use std::{io, sync::Mutex};
use tokio_util::sync::CancellationToken;

const CONNECTION_WORKERS: usize = 8;
const REMOTE_CONNECTION_WORKERS: usize = 6;
const SESSION_SPINNER_FRAMES: &[char] = &[
    '\u{f0a9e}',
    '\u{f0a9f}',
    '\u{f0aa0}',
    '\u{f0aa1}',
    '\u{f0aa2}',
    '\u{f0aa3}',
    '\u{f0aa4}',
    '\u{f0aa5}',
];

#[derive(Clone)]
pub(super) enum SessionAction {
    Tree {
        target: crate::tree::Target,
        action: crate::keyguide::Action,
        size: Size,
    },
    CiRefresh {
        owner: crate::tree::Owner,
        target: bkpr_protocol::CiRefreshTarget,
    },
}

pub(super) enum Attachment {
    Local(RegisteredClient),
    Remote(crate::remote::RemoteSession),
}
impl Attachment {
    fn endpoint(&self) -> Endpoint {
        match self {
            Self::Local(client) => client.endpoint().clone(),
            Self::Remote(session) => session.endpoint(),
        }
    }
    fn info(&self) -> &SessionInfo {
        match self {
            Self::Local(client) => client.info(),
            Self::Remote(session) => session.info(),
        }
    }
    fn activate(&mut self, detach_others: bool, cancellation: &CancellationToken) -> Result<()> {
        match self {
            Self::Local(client) => client.activate_cancellable(detach_others, cancellation),
            Self::Remote(session) => session.activate_cancellable(detach_others, cancellation),
        }
    }
    pub(super) fn lifecycle(&mut self) -> Result<Lifecycle> {
        match self {
            Self::Local(client) => client.poll_lifecycle(),
            Self::Remote(session) => session.poll_lifecycle(),
        }
    }
    fn lifetime_fd(&self) -> std::os::fd::RawFd {
        match self {
            Self::Local(client) => client.lifetime_fd(),
            Self::Remote(session) => session.lifetime_fd(),
        }
    }
    fn cancel(&mut self) {
        match self {
            Self::Local(client) => client.cancel(),
            Self::Remote(session) => session.cancel(),
        }
    }
    fn close_for_shutdown(&mut self) {
        match self {
            Self::Local(client) => client.close_for_shutdown(),
            Self::Remote(session) => session.close_for_shutdown(),
        }
    }
    pub(super) fn detach(&mut self) -> Result<()> {
        match self {
            Self::Local(client) => client.detach(),
            Self::Remote(session) => session.disconnect(),
        }
    }
}

struct Validated {
    owner: Attachment,
    defaults: SpawnDefaults,
}
struct Active {
    validated: Validated,
    registry: RegistrySnapshot,
    registry_pump: Pump<RegistrySnapshot>,
    input: InputPump,
}
enum Admission {
    Validated(Box<Validated>),
    Active(Box<Active>),
}
struct Retirement {
    error: Option<String>,
}
struct Completion {
    attachment: usize,
    generation: u64,
    result: Result<Admission>,
}
impl Completion {
    fn from_job(completion: jobs::Completion<Admission>) -> Result<Self> {
        let (attachment, generation) = completion
            .key
            .split_once(':')
            .context("invalid connection job key")?;
        Ok(Self {
            attachment: attachment.parse()?,
            generation: generation.parse()?,
            result: completion.result,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Connecting,
    Activating,
    Online,
    Offline,
    Rejected,
}
struct Session {
    target: SessionTarget,
    generation: u64,
    cancellation: CancellationToken,
    expected_session: Option<String>,
    identity: Option<SessionInfo>,
    owner: Option<Attachment>,
    app: Option<App>,
    registry: Option<Pump<RegistrySnapshot>>,
    phase: Phase,
    status: String,
    failures: u32,
    next_attempt: Option<Instant>,
    initial_activation: bool,
    retiring: usize,
    present: bool,
    ever_active: bool,
    last_error: Option<String>,
    membership: Membership,
    presentation: Option<Arc<crate::tree::Session>>,
}

impl Session {
    fn new(target: SessionTarget, now: Instant) -> Self {
        Self {
            target,
            generation: 0,
            cancellation: CancellationToken::new(),
            expected_session: None,
            identity: None,
            owner: None,
            app: None,
            registry: None,
            phase: Phase::Queued,
            status: "queued".into(),
            failures: 0,
            next_attempt: Some(now),
            initial_activation: true,
            retiring: 0,
            present: true,
            ever_active: false,
            last_error: None,
            membership: Membership::default(),
            presentation: None,
        }
    }

    fn record_failure(&mut self, error: anyhow::Error, now: Instant) {
        let rejected =
            error.downcast_ref::<IdentityRejection>() == Some(&IdentityRejection::Replacement);
        let message = format!("{error:#}");
        self.last_error = Some(message.clone());
        self.status = if rejected {
            format!("rejected: {message}\nPress R to retry this target.")
        } else if self.ever_active {
            format!("offline: {message}\nWaiting to reconnect.")
        } else {
            format!("offline: {message}\nPress R to retry this target.")
        };
        self.phase = if rejected {
            Phase::Rejected
        } else {
            Phase::Offline
        };
        self.failures = self.failures.saturating_add(1);
        self.next_attempt =
            (self.ever_active && !rejected).then(|| now + retry_delay(self.failures));
    }

    fn queue_reconnect(&mut self, now: Instant) -> Result<()> {
        // Invalidate before worker admission: an old cancelled result must
        // never overwrite this queued user intent while workers are occupied.
        self.cancellation.cancel();
        self.generation = self
            .generation
            .checked_add(1)
            .context("session attempt generation exhausted")?;
        self.cancellation = CancellationToken::new();
        self.phase = Phase::Queued;
        self.status = match &self.last_error {
            Some(error) => format!("reconnect queued\nPrevious failure: {error}"),
            None => "reconnect queued".into(),
        };
        self.next_attempt = Some(now);
        Ok(())
    }
}

/// One physical terminal; every native authority and asynchronous completion is
/// owned by exactly one attachment and one attempt generation. Admission uses
/// independent cancellable workers, never the shared management executor.
pub(super) struct Coordinator {
    sessions: Vec<Session>,
    selected: usize,
    offline: App,
    options: Options,
    jobs: jobs::Scope<Background>,
    connections: Option<jobs::Jobs<Admission>>,
    spinner: Spinner,
    started: bool,
    pub(super) tree: Arc<Mutex<crate::tree::Tree>>,
    pub(super) toasts: Arc<Mutex<toast::ToastStack>>,
    tree_owner: Option<crate::tree::Owner>,
    release_owners: HashMap<String, (usize, u64)>,
    offer_origins: VecDeque<(String, usize, u64)>,
    presentation_dirty: bool,
    empty: EmptyMetadata,
    pointer_tombstone: bool,
    admissions: HashMap<(usize, u64), bool>,
    retirements: HashSet<(usize, u64)>,
    retirement_jobs: Option<jobs::Jobs<Retirement>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityRejection {
    Replacement,
    Alias,
}
impl std::fmt::Display for IdentityRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Replacement => {
                "target name now identifies a different logical session; refusing replacement"
            }
            Self::Alias => "target is an alias of an already admitted session",
        })
    }
}
impl std::error::Error for IdentityRejection {}

#[derive(Default)]
struct Membership {
    previous: Option<(usize, usize, usize)>,
}
impl Membership {
    fn observe(&mut self, registry: &RegistrySnapshot) -> bool {
        let current = (
            registry.workspaces.len(),
            registry
                .workspaces
                .iter()
                .map(|workspace| workspace.windows.len())
                .sum(),
            registry.panes.len(),
        );
        let ended = match self.previous {
            Some(previous) => {
                (previous.0 > 0 && current.0 == 0)
                    || (previous.1 > 0 && current.1 == 0)
                    || (previous.2 > 0 && current.2 == 0)
            }
            None => false,
        };
        self.previous = Some(current);
        ended
    }
}

struct EmptyMetadata {
    registry: Arc<RegistrySnapshot>,
    rows: Arc<[crate::controller::SidebarRow]>,
    snapshots: Arc<HashMap<String, PaneSnapshot>>,
    statuses: Arc<HashMap<String, &'static str>>,
    history: Arc<HashMap<String, (u64, u64)>>,
}
impl EmptyMetadata {
    fn new() -> Self {
        Self {
            registry: Arc::new(empty_registry()),
            rows: Arc::from([]),
            snapshots: Arc::new(HashMap::new()),
            statuses: Arc::new(HashMap::new()),
            history: Arc::new(HashMap::new()),
        }
    }
}

fn refresh_presentation(index: usize, session: &mut Session, empty: &EmptyMetadata) -> bool {
    if !session.present {
        return session.presentation.take().is_some();
    }
    let owner = crate::tree::Owner {
        slot: index,
        generation: session.generation,
    };
    let status = match session.phase {
        Phase::Queued => SessionStatus::Queued,
        Phase::Connecting if session.ever_active => SessionStatus::Reconnecting,
        Phase::Connecting => SessionStatus::Connecting,
        Phase::Activating => SessionStatus::Activating,
        Phase::Online => SessionStatus::Online,
        Phase::Offline => SessionStatus::Offline,
        Phase::Rejected => SessionStatus::Rejected,
    };
    let previous = session
        .presentation
        .as_ref()
        .filter(|previous| previous.owner == owner);
    let (registry, rows, snapshots, statuses, history, workspace, window, focused) =
        if let Some(app) = session.app.as_mut() {
            let unchanged_registry = previous.filter(|previous| {
                previous.group.status == SessionStatus::Online
                    && previous.registry.revision == app.registry.revision
            });
            let registry = match unchanged_registry {
                Some(previous) => Arc::clone(&previous.registry),
                None => Arc::new(app.registry.clone()),
            };
            let state = &mut app.state;
            state.sidebar.reconcile(
                &app.registry,
                state.workspace.as_deref(),
                state.window.as_deref(),
                state.focused.as_deref(),
            );
            let rows = previous
                .filter(|previous| previous.rows.as_ref() == state.sidebar.rows.as_slice())
                .map(|previous| Arc::clone(&previous.rows))
                .unwrap_or_else(|| Arc::from(state.sidebar.rows.clone()));
            let old_snapshots = previous.map_or(&empty.snapshots, |previous| &previous.snapshots);
            let snapshots_changed = old_snapshots.len() != app.views.len()
                || app.views.iter().any(|(id, view)| {
                    old_snapshots.get(id).is_none_or(|snapshot| {
                        snapshot.snapshot_revision != view.metadata.snapshot_revision
                    })
                });
            let snapshots = if snapshots_changed {
                Arc::new(
                    app.views
                        .iter()
                        .map(|(id, view)| (id.clone(), view.metadata.clone()))
                        .collect(),
                )
            } else {
                Arc::clone(old_snapshots)
            };
            let old_statuses = previous.map_or(&empty.statuses, |previous| &previous.statuses);
            let statuses_changed = old_statuses.len()
                != app
                    .views
                    .keys()
                    .filter(|pane| app.badge(pane).is_some())
                    .count()
                || app
                    .views
                    .keys()
                    .any(|pane| app.badge(pane).as_ref() != old_statuses.get(pane));
            let statuses = if statuses_changed {
                Arc::new(
                    app.views
                        .keys()
                        .filter_map(|pane| app.badge(pane).map(|status| (pane.clone(), status)))
                        .collect(),
                )
            } else {
                Arc::clone(old_statuses)
            };
            let old_history = previous.map_or(&empty.history, |previous| &previous.history);
            let history_changed = old_history.len()
                != app
                    .views
                    .keys()
                    .filter(|pane| app.history(pane).is_some())
                    .count()
                || app
                    .views
                    .keys()
                    .any(|pane| app.history(pane).as_ref() != old_history.get(pane));
            let history = if history_changed {
                Arc::new(
                    app.views
                        .keys()
                        .filter_map(|pane| app.history(pane).map(|range| (pane.clone(), range)))
                        .collect(),
                )
            } else {
                Arc::clone(old_history)
            };
            (
                registry,
                rows,
                snapshots,
                statuses,
                history,
                app.state.workspace.as_ref(),
                app.state.window.as_ref(),
                app.state.focused.as_ref(),
            )
        } else {
            (
                Arc::clone(&empty.registry),
                Arc::clone(&empty.rows),
                Arc::clone(&empty.snapshots),
                Arc::clone(&empty.statuses),
                Arc::clone(&empty.history),
                None,
                None,
                None,
            )
        };
    let host = match &session.target {
        SessionTarget::Remote(target) => Some(target.destination.as_str()),
        SessionTarget::Local { .. } => None,
    };
    let name = match session.identity.as_ref() {
        Some(identity) => std::borrow::Cow::Borrowed(identity.name.as_str()),
        None => match &session.target {
            SessionTarget::Remote(target) => {
                std::borrow::Cow::Borrowed(target.session_name.as_str())
            }
            SessionTarget::Local {
                name: Some(name), ..
            } => std::borrow::Cow::Borrowed(name.as_str()),
            SessionTarget::Local { socket, .. } => socket
                .file_name()
                .unwrap_or(socket.as_os_str())
                .to_string_lossy(),
        },
    };
    if previous.is_some_and(|previous| {
        previous.owner == owner
            && previous.group.name == name.as_ref()
            && previous.group.host.as_deref() == host
            && previous.group.status == status
            && previous.group.detail == session.status
            && Arc::ptr_eq(&previous.registry, &registry)
            && Arc::ptr_eq(&previous.rows, &rows)
            && Arc::ptr_eq(&previous.snapshots, &snapshots)
            && Arc::ptr_eq(&previous.statuses, &statuses)
            && Arc::ptr_eq(&previous.history, &history)
            && previous.workspace.as_ref() == workspace
            && previous.window.as_ref() == window
            && previous.focused.as_ref() == focused
    }) {
        return false;
    }
    session.presentation = Some(Arc::new(crate::tree::Session {
        owner,
        group: SessionGroup {
            name: name.into_owned(),
            host: host.map(str::to_owned),
            status,
            detail: session.status.clone(),
        },
        registry,
        rows,
        snapshots,
        statuses,
        history,
        workspace: workspace.cloned(),
        window: window.cloned(),
        focused: focused.cloned(),
    }));
    true
}

fn empty_registry() -> RegistrySnapshot {
    RegistrySnapshot {
        revision: 0,
        workspaces: Vec::new(),
        panes: Vec::new(),
    }
}

impl Coordinator {
    pub fn new(targets: Vec<SessionTarget>, options: Options, size: Size) -> Result<Self> {
        let mut exact = HashSet::new();
        let targets = targets
            .into_iter()
            .filter(|target| exact.insert(target.key()))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !targets.is_empty(),
            "at least one session target is required"
        );
        let jobs = jobs::Scope::new(8)?;
        let connections = jobs::Jobs::new(CONNECTION_WORKERS)?;
        let retirement_limit = targets
            .len()
            .saturating_mul(2)
            .saturating_add(CONNECTION_WORKERS);
        let retirement_jobs = jobs::Jobs::with_workers(retirement_limit, 2)?;
        let tree = Arc::new(Mutex::new(crate::tree::Tree::new()));
        let toasts = Arc::new(Mutex::new(toast::ToastStack::default()));
        let offline = App::new(
            Endpoint::unavailable(),
            options.clone(),
            None,
            empty_registry(),
            size,
            AppRuntime {
                jobs: jobs.scope()?,
                input: None,
                tree: Arc::clone(&tree),
                toasts: Arc::clone(&toasts),
            },
        )?;
        let sessions = targets
            .into_iter()
            .map(|target| Session::new(target, Instant::now()))
            .collect();
        Ok(Self {
            sessions,
            selected: 0,
            offline,
            options,
            jobs,
            connections: Some(connections),
            spinner: Spinner::new(Instant::now(), SESSION_SPINNER_FRAMES),
            started: false,
            tree,
            toasts,
            tree_owner: None,
            release_owners: HashMap::new(),
            offer_origins: VecDeque::new(),
            presentation_dirty: true,
            empty: EmptyMetadata::new(),
            pointer_tombstone: false,
            admissions: HashMap::new(),
            retirements: HashSet::new(),
            retirement_jobs: Some(retirement_jobs),
        })
    }

    pub fn app(&self) -> &App {
        self.sessions[self.selected]
            .app
            .as_ref()
            .unwrap_or(&self.offline)
    }
    pub fn app_mut(&mut self) -> &mut App {
        self.sessions[self.selected]
            .app
            .as_mut()
            .unwrap_or(&mut self.offline)
    }
    pub fn namespace(&self) -> (u64, u64) {
        (
            self.selected as u64 + 1,
            self.sessions[self.selected].generation,
        )
    }

    pub fn current_owner(&self) -> crate::tree::Owner {
        crate::tree::Owner {
            slot: self.selected,
            generation: self.sessions[self.selected].generation,
        }
    }

    pub fn should_exit(&self) -> bool {
        !self.sessions.iter().any(|session| session.present)
    }

    fn publish(
        &mut self,
        attachment: usize,
        generation: u64,
        run: impl FnOnce() -> Result<Admission> + Send + 'static,
    ) -> Result<()> {
        self.connections
            .as_mut()
            .context("connection executor is shut down")?
            .submit(format!("{attachment}:{generation}"), run)
    }

    fn start(&mut self, index: usize, size: Size) -> Result<()> {
        let session = &mut self.sessions[index];
        session.generation = session
            .generation
            .checked_add(1)
            .context("session attempt generation exhausted")?;
        session.cancellation = CancellationToken::new();
        let cancellation = session.cancellation.clone();
        let generation = session.generation;
        session.next_attempt = None;
        session.phase = Phase::Connecting;
        let state = if session.ever_active {
            "reconnecting"
        } else {
            "connecting"
        };
        session.status = match &session.last_error {
            Some(error) => format!("{state}\nPrevious failure: {error}"),
            None => state.into(),
        };
        let connect: Box<dyn FnOnce() -> Result<Validated> + Send> = match &mut session.target {
            SessionTarget::Local {
                name,
                socket,
                spawn_defaults,
                prepare,
            } => {
                let name = name.clone();
                let socket = socket.clone();
                let defaults = spawn_defaults.clone();
                // Taking this before dispatch is intentional: even an ambiguous
                // bootstrap is never repeated by automatic or forced reconnect.
                let prepare = prepare.take();
                Box::new(move || {
                    anyhow::ensure!(!cancellation.is_cancelled(), "session connection cancelled");
                    if let Some(prepare) = prepare {
                        prepare(size, cancellation.clone())?;
                    }
                    anyhow::ensure!(!cancellation.is_cancelled(), "session connection cancelled");
                    let client = RegisteredClient::register_cancellable(&socket, &cancellation)?;
                    if let Some(name) = name {
                        anyhow::ensure!(
                            client.info().name == name,
                            "local target resolved to a different named session"
                        );
                    }
                    let owner = Attachment::Local(client);
                    anyhow::ensure!(!cancellation.is_cancelled(), "session connection cancelled");
                    Ok(Validated { owner, defaults })
                })
            }
            SessionTarget::Remote(target) => {
                let target = target.clone();
                Box::new(move || {
                    let (session, defaults) = crate::remote::RemoteSession::register_cancellable(
                        target,
                        cancellation.clone(),
                    )?;
                    let mut owner = Attachment::Remote(session);
                    if cancellation.is_cancelled() {
                        return Err(cancel_admission(
                            &mut owner,
                            anyhow::anyhow!("session connection cancelled"),
                        ));
                    }
                    Ok(Validated { owner, defaults })
                })
            }
        };
        let remote = matches!(&self.sessions[index].target, SessionTarget::Remote(_));
        self.publish(index, generation, move || {
            connect().map(Box::new).map(Admission::Validated)
        })?;
        self.admissions.insert((index, generation), remote);
        Ok(())
    }

    fn activate(&mut self, index: usize, mut validated: Validated) -> Result<()> {
        let info = validated.owner.info().clone();
        if let Err(error) = validate_identity(
            self.sessions[index].expected_session.as_deref(),
            &info,
            self.sessions
                .iter()
                .enumerate()
                .filter(|(other, session)| *other != index && session.present)
                .filter_map(|(_, session)| session.identity.as_ref()),
        ) {
            self.enqueue_retirement(
                index,
                self.sessions[index].generation,
                validated.owner,
                None,
                None,
                true,
            )?;
            return Err(error);
        }
        let session = &mut self.sessions[index];
        session
            .expected_session
            .get_or_insert_with(|| info.session_id.clone());
        session.identity = Some(info);
        session.phase = Phase::Activating;
        session.status = match &session.last_error {
            Some(error) => format!("activating\nPrevious failure: {error}"),
            None => "activating".into(),
        };
        let generation = session.generation;
        let cancellation = session.cancellation.clone();
        let detach_others = session.initial_activation
            && (self.options.detach_others
                || matches!(&session.target, SessionTarget::Remote(target) if target.detach_others));
        // This policy is consumed at dispatch, including an ambiguous response.
        session.initial_activation = false;
        let remote = matches!(&validated.owner, Attachment::Remote(_));
        self.publish(index, generation, move || {
            let initialized =
                (|| -> Result<(RegistrySnapshot, Pump<RegistrySnapshot>, InputPump)> {
                    validated.owner.activate(detach_others, &cancellation)?;
                    anyhow::ensure!(!cancellation.is_cancelled(), "session activation cancelled");
                    let endpoint = validated.owner.endpoint();
                    let registry: RegistryResult = serde_json::from_value(
                        endpoint.checked_cancellable(Operation::Inspect, &cancellation)?,
                    )?;
                    let input = InputPump::new(&endpoint)?;
                    let registry_pump = updates::registry(&endpoint, registry.registry.revision)?;
                    Ok((registry.registry, registry_pump, input))
                })();
            match initialized {
                Ok((registry, registry_pump, input)) => Ok(Admission::Active(Box::new(Active {
                    validated,
                    registry,
                    registry_pump,
                    input,
                }))),
                Err(error) => Err(cancel_admission(&mut validated.owner, error)),
            }
        })?;
        self.admissions.insert((index, generation), remote);
        Ok(())
    }

    /// Connection failure retains the named target. Only an actually admitted
    /// owner is eligible for automatic reconnect; initial errors require R.
    fn fail(&mut self, index: usize, error: anyhow::Error) -> Result<()> {
        self.retire_current(index, false)?;
        self.sessions[index].record_failure(error, Instant::now());
        Ok(())
    }

    /// Native semantic closure is different from an unavailable transport.
    /// Stable slots remain for already-admitted releases and cleanup receipts.
    fn remove(&mut self, index: usize, size: Size, graceful: bool) -> Result<()> {
        self.retire_current(index, graceful)?;
        let session = &mut self.sessions[index];
        session.present = false;
        session.identity = None;
        session.next_attempt = None;
        session.presentation = None;
        self.presentation_dirty = true;
        if index == self.selected
            && let Some(next) = self.sessions.iter().position(|session| session.present)
        {
            self.select(next, size)?;
        }
        Ok(())
    }

    fn retire_current(&mut self, index: usize, graceful: bool) -> Result<()> {
        self.remember_origins();
        let session = &mut self.sessions[index];
        let mut owner = session.owner.take();
        let retirement_token = if graceful && owner.is_some() {
            if let Some(owner) = &mut owner {
                owner.close_for_shutdown();
            }
            Some(std::mem::replace(
                &mut session.cancellation,
                CancellationToken::new(),
            ))
        } else {
            session.cancellation.cancel();
            if let Some(owner) = &mut owner {
                owner.cancel();
            }
            None
        };
        session.registry.take();
        let mut app = session.app.take();
        if let Some(app) = &mut app {
            app.backgrounded = true;
        }
        if let Some(app) = &mut app
            && index == self.selected
        {
            self.pointer_tombstone |= app.pointer_capture.is_some();
            transfer_terminal(app, &mut self.offline);
        }
        let generation = session.generation;
        if let Some(owner) = owner {
            self.enqueue_retirement(index, generation, owner, app, retirement_token, !graceful)?;
        }
        Ok(())
    }

    fn enqueue_retirement(
        &mut self,
        index: usize,
        generation: u64,
        mut owner: Attachment,
        app: Option<App>,
        token: Option<CancellationToken>,
        force: bool,
    ) -> Result<()> {
        if force {
            owner.cancel();
        }
        self.track_retirement(index, generation, move || {
            let _token = token;
            match app {
                Some(app) => finish_app(app, &mut owner),
                None => owner.detach(),
            }
        })
    }

    fn retire_admission(
        &mut self,
        index: usize,
        generation: u64,
        mut admission: Admission,
    ) -> Result<()> {
        match &mut admission {
            Admission::Validated(validated) => validated.owner.cancel(),
            Admission::Active(active) => active.validated.owner.cancel(),
        }
        // Keep the complete accepted resource bundle off the UI path: even an
        // otherwise unused InputPump may still have worker teardown to settle.
        self.track_retirement(index, generation, move || match admission {
            Admission::Validated(mut validated) => validated.owner.detach(),
            Admission::Active(active) => {
                let Active {
                    mut validated,
                    mut registry_pump,
                    input,
                    ..
                } = *active;
                registry_pump.cancel();
                drop(input);
                validated.owner.detach()
            }
        })
    }

    fn complete_retirement(&mut self, completion: jobs::Completion<Retirement>) -> Result<()> {
        let (index, generation) = completion
            .key
            .split_once(':')
            .context("invalid retirement key")?;
        let (index, generation) = (index.parse::<usize>()?, generation.parse::<u64>()?);
        anyhow::ensure!(
            self.retirements.remove(&(index, generation)),
            "unknown retirement completion"
        );
        self.sessions[index].retiring = self.sessions[index]
            .retiring
            .checked_sub(1)
            .context("retirement count underflow")?;
        let retired = completion.result.with_context(|| {
            format!(
                "{}: attachment generation {generation} retirement",
                self.sessions[index].target.label()
            )
        })?;
        if let Some(error) = retired.error {
            bail!(
                "{}: attachment generation {generation} cleanup failed: {error}",
                self.sessions[index].target.label()
            );
        }
        Ok(())
    }

    fn track_retirement(
        &mut self,
        index: usize,
        generation: u64,
        retire: impl FnOnce() -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.retirement_jobs
            .as_mut()
            .context("retirement executor is shut down")?
            .submit(format!("{index}:{generation}"), move || {
                Ok(Retirement {
                    error: retire().err().map(|error| format!("{error:#}")),
                })
            })?;
        self.retirements.insert((index, generation));
        self.sessions[index].retiring += 1;
        Ok(())
    }

    fn refresh_tree(&mut self) {
        let mut changed = std::mem::take(&mut self.presentation_dirty);
        for (index, session) in self.sessions.iter_mut().enumerate() {
            changed |= refresh_presentation(index, session, &self.empty);
        }
        let owner = self.current_owner();
        if changed || self.tree_owner != Some(owner) {
            let sessions = self
                .sessions
                .iter()
                .filter_map(|session| session.presentation.as_ref().cloned())
                .collect();
            self.tree
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .reconcile(sessions, owner);
            self.tree_owner = Some(owner);
        }
    }

    fn dispatch_tree(
        &mut self,
        target: crate::tree::Target,
        action: crate::keyguide::Action,
        size: Size,
    ) -> Result<()> {
        use crate::{keyguide::Action, tree::Node};
        let index = target.owner.slot;
        if self
            .sessions
            .get(index)
            .is_none_or(|session| !session.present || session.generation != target.owner.generation)
            || !self.app().session_navigation_allowed()
        {
            return Ok(());
        }
        if action == Action::ReconnectSession {
            if target.node == Node::Session {
                self.retire_current(index, false)?;
                self.sessions[index].queue_reconnect(Instant::now())?;
            }
            return Ok(());
        }
        if target.node == Node::Session && action == Action::ActivateSelected {
            self.select(index, size)?;
            if self.selected == index {
                self.app_mut().leave_sidebar();
            }
            return Ok(());
        }
        if self.sessions[index].app.is_none() {
            return Ok(());
        }
        self.select(index, size)?;
        if self.selected != index {
            return Ok(());
        }
        let app = self.app_mut();
        app.synchronize_tree_focus();
        let native = match &target.node {
            Node::Native(native) => Some(native),
            Node::Session => None,
        };
        if let Err(error) = app.dispatch_tree_action(native, action, size) {
            if transport::server_closing(&error) || app.socket.session_ended() {
                self.remove(index, size, true)?;
            } else {
                app.notify(format!("{error:#}"));
            }
        }
        Ok(())
    }

    fn select(&mut self, index: usize, size: Size) -> Result<()> {
        if index == self.selected || index >= self.sessions.len() || !self.sessions[index].present {
            return Ok(());
        }
        if self.sessions[self.selected].present && !self.app().session_navigation_allowed() {
            return Ok(());
        }
        let old = self.selected;
        self.remember_origins();
        if let Some(app) = self.sessions[old].app.as_mut() {
            app.backgrounded = true;
            let host_focused = app.host_focused;
            app.host_focused = false;
            app.retire_resize("session focus changed");
            app.selection_focus_lost();
            app.close_search();
            let retired = app.sync_views(size, &[]);
            app.scope_input_result(retired)?;
            transfer_terminal(app, &mut self.offline);
            self.offline.host_focused = host_focused;
        }
        self.selected = index;
        if let Some(app) = self.sessions[index].app.as_mut() {
            transfer_terminal(&mut self.offline, app);
            app.backgrounded = false;
        }
        Ok(())
    }

    fn remember_origins(&mut self) {
        let index = self.selected;
        let generation = self.sessions[index].generation;
        if let Some(app) = self.sessions[index].app.as_ref() {
            for key in app.held.keys() {
                self.release_owners.insert(key.clone(), (index, generation));
            }
            if let Some(offer) = &app.paste_offer
                && !self
                    .offer_origins
                    .iter()
                    .any(|(password, _, _)| *password == offer.password)
            {
                if self.offer_origins.len() == 128 {
                    self.offer_origins.pop_front();
                }
                self.offer_origins
                    .push_back((offer.password.clone(), index, generation));
            }
        }
    }

    pub fn tick(&mut self, size: Size) -> Result<()> {
        self.remember_origins();
        match self.app_mut().session_action.take() {
            Some(SessionAction::Tree {
                target,
                action,
                size,
            }) => self.dispatch_tree(target, action, size)?,
            Some(SessionAction::CiRefresh { owner, target }) => {
                let endpoint = self
                    .sessions
                    .get(owner.slot)
                    .filter(|session| session.present && session.generation == owner.generation)
                    .and_then(|session| session.app.as_ref())
                    .and_then(|app| app.ci_refresh_endpoint(&target));
                if let Some(endpoint) = endpoint {
                    if let Err(error) =
                        self.app_mut()
                            .queue_ci_refresh(owner, target.clone(), endpoint)
                    {
                        self.app_mut().popover.refresh_receipt(owner, &target, true);
                        self.app_mut().notify(format!("{error:#}"));
                    }
                } else {
                    self.app_mut().popover.refresh_receipt(owner, &target, true);
                }
            }
            None => {}
        }
        for completion in self.retirement_jobs.as_mut().unwrap().latest()? {
            if let Err(error) = self.complete_retirement(completion) {
                self.app_mut().notify(format!("{error:#}"));
            }
        }
        for completion in self.connections.as_mut().unwrap().latest()? {
            let completion = Completion::from_job(completion)?;
            let index = completion.attachment;
            self.admissions.remove(&(index, completion.generation));
            if !self.sessions[index].present
                || self.sessions[index].generation != completion.generation
            {
                if let Ok(admission) = completion.result {
                    self.retire_admission(index, completion.generation, admission)?;
                }
                continue;
            }
            // Cancellation of owned transport cleanup must never erase an
            // accepted current-generation failure. Successful cancelled owners
            // are retired, never activated.
            let result = if self.sessions[index].cancellation.is_cancelled() {
                match completion.result {
                    Ok(admission) => {
                        self.retire_admission(index, completion.generation, admission)?;
                        Err(anyhow::anyhow!(
                            "connection cancelled before admission completed"
                        ))
                    }
                    Err(error) => Err(error),
                }
            } else {
                completion.result
            };
            match result {
                Ok(Admission::Validated(validated)) => {
                    if let Err(error) = self.activate(index, *validated) {
                        if error.downcast_ref::<IdentityRejection>()
                            == Some(&IdentityRejection::Alias)
                        {
                            self.remove(index, size, false)?;
                        } else {
                            self.fail(index, error)?;
                        }
                    }
                }
                Ok(Admission::Active(active)) => {
                    let Active {
                        validated,
                        registry,
                        registry_pump,
                        input,
                    } = *active;
                    let Validated { owner, defaults } = validated;
                    let initialized = self
                        .jobs
                        .scope_for(matches!(&owner, Attachment::Remote(_)))
                        .and_then(|jobs| {
                            App::new(
                                owner.endpoint(),
                                self.options.clone(),
                                Some(defaults),
                                registry,
                                size,
                                AppRuntime {
                                    jobs,
                                    input: Some(input),
                                    tree: Arc::clone(&self.tree),
                                    toasts: Arc::clone(&self.toasts),
                                },
                            )
                        });
                    let mut app = match initialized {
                        Ok(app) => app,
                        Err(error) => {
                            self.enqueue_retirement(
                                index,
                                completion.generation,
                                owner,
                                None,
                                None,
                                true,
                            )?;
                            self.fail(index, error.context("client setup failed"))?;
                            continue;
                        }
                    };
                    app.namespace = (index as u64 + 1, completion.generation);
                    app.state.remote = self.sessions[index].target.provenance();
                    app.notification_source = Some(self.sessions[index].target.label());
                    if index == self.selected {
                        transfer_terminal(&mut self.offline, &mut app);
                    } else {
                        app.host_focused = false;
                    }
                    app.backgrounded = index != self.selected;
                    let session = &mut self.sessions[index];
                    session.owner = Some(owner);
                    session.registry = Some(registry_pump);
                    session.app = Some(app);
                    session.failures = 0;
                    session.ever_active = true;
                    session.last_error = None;
                    session.membership = Membership::default();
                    session.phase = Phase::Online;
                    session.status = "online".into();
                }
                Err(error)
                    if error.is::<transport::client_lifetime::NativeRetirement>()
                        || transport::server_closing(&error) =>
                {
                    self.remove(index, size, true)?;
                }
                Err(error) => self.fail(index, error)?,
            }
        }
        for index in 0..self.sessions.len() {
            let lifecycle = self.sessions[index]
                .owner
                .as_mut()
                .map(Attachment::lifecycle);
            match lifecycle {
                Some(Ok(Lifecycle::Detached { reason: _ })) => {
                    self.remove(index, size, true)?;
                    continue;
                }
                Some(Ok(Lifecycle::TransportClosed)) => {
                    self.fail(index, anyhow::anyhow!("transport closed"))?;
                    continue;
                }
                Some(Err(error)) => {
                    self.fail(index, error)?;
                    continue;
                }
                _ => {}
            }
            let session = &mut self.sessions[index];
            if let Some(app) = &mut session.app {
                let maintained = (|| -> Result<()> {
                    for failure in app.input.check()? {
                        app.record_input_failure(failure);
                    }
                    app.reap_retiring()?;
                    app.reap_responses();
                    let pointer = app.flush_pointer_releases();
                    app.scope_input_result(pointer)?;
                    let selection = app.flush_selection_release();
                    app.scope_input_result(selection)?;
                    app.complete_background()?;
                    app.expire_devices()?;
                    let keys = app.flush_key_releases();
                    app.scope_input_result(keys)?;
                    if let Some(registry) = session.registry.as_mut().unwrap().latest()?
                        && registry.revision >= app.registry.revision
                    {
                        app.observe_resize(&registry);
                        app.registry = registry;
                        app.state.reconcile(&app.registry);
                    }
                    if index != self.selected {
                        // Complete late attaches directly into retirement, never
                        // poll their frames or acquire hidden size/device leases.
                        let retired = app.sync_views(size, &[]);
                        app.scope_input_result(retired)?;
                    }
                    Ok(())
                })();
                if let Err(error) = maintained {
                    if transport::server_closing(&error) || app.socket.session_ended() {
                        self.remove(index, size, true)?;
                    } else {
                        self.fail(index, error)?;
                    }
                } else if session.membership.observe(&app.registry) {
                    self.remove(index, size, true)?;
                }
            }
        }
        if self.started {
            for remote in [false, true] {
                for index in 0..self.sessions.len() {
                    if !self.sessions[index].present
                        || matches!(&self.sessions[index].target, SessionTarget::Remote(_))
                            != remote
                        || self.sessions[index].retiring >= 2
                        || self.sessions[index]
                            .next_attempt
                            .is_none_or(|deadline| Instant::now() < deadline)
                    {
                        continue;
                    }
                    let remote_running = self.admissions.values().filter(|remote| **remote).count();
                    if !admission_available(self.admissions.len(), remote_running, remote) {
                        break;
                    }
                    if let Err(error) = self.start(index, size) {
                        self.fail(index, error)?;
                    }
                }
            }
        } else {
            self.started = true;
        }
        self.refresh_tree();
        let now = Instant::now();
        let busy_visible = {
            let tree = self.tree.lock().unwrap_or_else(|error| error.into_inner());
            tree.rows
                .iter()
                .skip(tree.scroll)
                .take(tree.viewport_rows())
                .any(|row| {
                    row.kind == crate::tree::Kind::Session
                        && tree
                            .session(row.owner)
                            .is_some_and(|session| session.group.status.busy())
                })
        };
        self.spinner.set_active(busy_visible, now);
        self.spinner.advance(now);
        let spinner = self.spinner.glyph();
        let namespace = self.namespace();
        self.offline.namespace = namespace;
        let app = self.app_mut();
        app.synchronize_tree_focus();
        app.state.session_spinner = spinner;
        Ok(())
    }

    pub fn append_poll_fds(&self, fds: &mut Vec<libc::pollfd>) {
        fds.push(libc::pollfd {
            fd: self.connections.as_ref().unwrap().wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.push(libc::pollfd {
            fd: self.retirement_jobs.as_ref().unwrap().wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        for (index, session) in self.sessions.iter().enumerate() {
            for fd in [
                session.owner.as_ref().map(Attachment::lifetime_fd),
                session.registry.as_ref().map(|pump| pump.wake.as_raw_fd()),
                session
                    .app
                    .as_ref()
                    .filter(|_| index != self.selected)
                    .map(|app| app.input.wake.as_raw_fd()),
            ]
            .into_iter()
            .flatten()
            {
                fds.push(libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
        }
    }
    pub fn timeout(&self, timeout: i32) -> i32 {
        let now = Instant::now();
        let active = self.admissions.len();
        let remote = self.admissions.values().filter(|remote| **remote).count();
        self.sessions
            .iter()
            .filter(|session| {
                session.present
                    && session.retiring < 2
                    && admission_available(
                        active,
                        remote,
                        matches!(&session.target, SessionTarget::Remote(_)),
                    )
            })
            .filter_map(|session| session.next_attempt)
            .chain(self.spinner.next_deadline())
            .fold(timeout, |timeout, deadline| {
                timeout.min(
                    deadline
                        .saturating_duration_since(now)
                        .as_millis()
                        .min(i32::MAX as u128) as i32,
                )
            })
    }

    pub fn route_device_reply(&mut self, response: &[u8]) -> Result<bool> {
        self.remember_origins();
        let Some(origin) = clipboard_reply_origin(response) else {
            return Ok(false);
        };
        let origin = match origin {
            ClipboardReplyOrigin::Request(id) => physical_origin(id),
            ClipboardReplyOrigin::Offer(password) => self
                .offer_origins
                .iter()
                .find(|(known, _, _)| known == password)
                .map(|(_, index, generation)| (*index, *generation))
                .or_else(|| Some((self.selected, self.sessions[self.selected].generation))),
        };
        if let Some((index, generation)) = origin
            && let Some(session) = self.sessions.get_mut(index)
            && session.generation == generation
            && let Some(app) = session.app.as_mut()
        {
            let result = app.device_reply(response);
            app.scope_input_result(result)?;
        }
        self.remember_origins();
        Ok(true)
    }

    /// Key releases are addressed to the exact original view, even after a
    /// sidebar switch. A retired generation consumes its release as a tombstone.
    pub fn route_release(&mut self, event: &ClientEvent, size: Size) -> Result<bool> {
        self.remember_origins();
        if matches!(event, ClientEvent::Reply(reply) if reply == b"focus;out") {
            // Unlike selecting another session, losing the physical terminal
            // can lose every outstanding hardware key-up, including the keys
            // owned by an inactive attachment's retiring view.
            for session in &mut self.sessions {
                if let Some(app) = session.app.as_mut() {
                    let cancelled = app.cancel_held_keys();
                    if let Err(error) = app.scope_input_result(cancelled) {
                        app.notify(format!("Held-key cleanup failed: {error:#}"));
                    }
                }
            }
        }
        if self.pointer_tombstone {
            match event {
                ClientEvent::Mouse { button, action, .. }
                    if !matches!(
                        button,
                        MouseButton::WheelUp
                            | MouseButton::WheelDown
                            | MouseButton::WheelLeft
                            | MouseButton::WheelRight
                    ) =>
                {
                    if *action == MouseAction::Release {
                        self.pointer_tombstone = false;
                    }
                    return Ok(true);
                }
                ClientEvent::Legacy(InputEvent::Mouse(mouse))
                    if !mouse
                        .mouse_buttons
                        .intersects(MouseButtons::VERT_WHEEL | MouseButtons::HORZ_WHEEL) =>
                {
                    if !mouse
                        .mouse_buttons
                        .intersects(MouseButtons::LEFT | MouseButtons::MIDDLE | MouseButtons::RIGHT)
                    {
                        self.pointer_tombstone = false;
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        let key = match event {
            ClientEvent::Reply(reply) => reply.strip_prefix(b"key;").map(kitty_key).transpose()?,
            ClientEvent::Legacy(InputEvent::Key(key)) => Some(key_event(key.clone())),
            _ => None,
        };
        if let Some(key) = key {
            match key.action {
                KeyAction::Press => {
                    if let Some((index, generation)) = self.release_owners.get(&key.key).copied() {
                        if let Some(session) = self.sessions.get_mut(index)
                            && session.present
                            && session.generation == generation
                            && let Some(app) = session.app.as_mut()
                        {
                            let cancelled = app.cancel_held_key(&key.key);
                            app.scope_input_result(cancelled)?;
                            if app.held.contains_key(&key.key) {
                                // A fresh press cannot replace an older owner
                                // whose cleanup has not yet entered its FIFO.
                                app.notify(
                                    "Previous key release is awaiting input admission".into(),
                                );
                                return Ok(true);
                            }
                        }
                        self.release_owners.remove(&key.key);
                    }
                }
                KeyAction::Repeat => {
                    if let Some((index, generation)) = self.release_owners.get(&key.key)
                        && (*index != self.selected
                            || self.sessions.get(*index).is_none_or(|session| {
                                !session.present
                                    || session.generation != *generation
                                    || session.app.as_ref().is_none_or(|app| {
                                        app.held.get(&key.key).is_none_or(|held| held.pending)
                                    })
                            }))
                    {
                        return Ok(true);
                    }
                }
                KeyAction::Release => {
                    if let Some((name, (index, generation))) =
                        self.release_owners.remove_entry(&key.key)
                        && (index != self.selected
                            || self.sessions.get(index).is_none_or(|session| {
                                !session.present
                                    || session.generation != generation
                                    || session.app.is_none()
                            }))
                    {
                        if let Some(session) = self.sessions.get_mut(index)
                            && session.present
                            && session.generation == generation
                            && let Some(app) = session.app.as_mut()
                        {
                            let result = app.key(key, &mut io::sink(), size, &[]);
                            if let Err(error) = result {
                                app.notify(format!("{error:#}"));
                            }
                            if app.held.contains_key(&name) {
                                self.release_owners.insert(name, (index, generation));
                            }
                        }
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    pub fn finish(mut self) -> Result<()> {
        for session in &mut self.sessions {
            if session.owner.is_none() {
                session.cancellation.cancel();
            }
        }
        let mut result = Ok(());
        for session in &mut self.sessions {
            if let Some(mut owner) = session.owner.take() {
                if let Some(app) = session.app.take() {
                    accumulate_cleanup(&mut result, finish_app(app, &mut owner));
                } else {
                    accumulate_cleanup(&mut result, owner.detach());
                }
            }
            session.cancellation.cancel();
        }
        // No input/render work remains. Cancelled admissions can only retire;
        // their owners are never activated or installed during shutdown.
        match self.connections.take().unwrap().finish() {
            Ok(completions) => {
                for completion in completions {
                    let completion = Completion::from_job(completion)?;
                    self.admissions
                        .remove(&(completion.attachment, completion.generation));
                    if let Ok(admission) = completion.result {
                        self.retire_admission(
                            completion.attachment,
                            completion.generation,
                            admission,
                        )?;
                    }
                }
            }
            Err(error) => accumulate_cleanup(&mut result, Err(error)),
        }
        match self.retirement_jobs.take().unwrap().finish() {
            Ok(completions) => {
                for completion in completions {
                    accumulate_cleanup(&mut result, self.complete_retirement(completion));
                }
            }
            Err(error) => accumulate_cleanup(&mut result, Err(error)),
        }
        result
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        for session in &mut self.sessions {
            session.cancellation.cancel();
            if let Some(owner) = &mut session.owner {
                owner.cancel();
            }
        }
    }
}

fn cancel_admission(owner: &mut Attachment, error: anyhow::Error) -> anyhow::Error {
    owner.cancel();
    match owner.detach() {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("owned connection cleanup also failed: {cleanup:#}")),
    }
}

fn admission_available(active: usize, remote: usize, target_remote: bool) -> bool {
    active < CONNECTION_WORKERS && (!target_remote || remote < REMOTE_CONNECTION_WORKERS)
}

fn retry_delay(failures: u32) -> Duration {
    Duration::from_millis(500u64.saturating_mul(1u64 << failures.saturating_sub(1).min(6)))
        .min(Duration::from_secs(30))
}

fn validate_identity<'a>(
    expected: Option<&str>,
    candidate: &SessionInfo,
    mut owners: impl Iterator<Item = &'a SessionInfo>,
) -> Result<()> {
    if expected.is_some_and(|expected| expected != candidate.session_id) {
        return Err(IdentityRejection::Replacement.into());
    }
    // Names and persistent UUIDs alone do not establish an owner. A fresh
    // daemon incarnation can reattach only with entirely new native authority.
    if owners.any(|owner| {
        owner.daemon_id == candidate.daemon_id && owner.session_id == candidate.session_id
    }) {
        return Err(IdentityRejection::Alias.into());
    }
    Ok(())
}

fn physical_origin(id: &str) -> Option<(usize, u64)> {
    let (attachment, rest) = id.strip_prefix('a')?.split_once('g')?;
    let (generation, _) = rest.split_once('_')?;
    Some((
        attachment.parse::<usize>().ok()?.checked_sub(1)?,
        generation.parse().ok()?,
    ))
}

fn transfer_terminal(from: &mut App, to: &mut App) {
    to.colors = std::mem::take(&mut from.colors);
    to.color_query_pending = std::mem::take(&mut from.color_query_pending);
    to.enhanced_supported = from.enhanced_supported;
    to.theme = from.theme;
    to.chrome_graphics_enabled = from.chrome_graphics_enabled;
    to.host_focused = from.host_focused;
    to.last_button = from.last_button;
    to.state.sidebar.preferred_columns = from.state.sidebar.preferred_columns;
    if from.theme_picker.is_some() {
        to.begin_modal();
        to.theme_picker = from.theme_picker.take();
    }
}

impl App {
    pub(super) fn device_reply(&mut self, response: &[u8]) -> Result<()> {
        if self.consent.is_none()
            && self.state.form.is_none()
            && (!self.search_open() || self.paste_offer.is_some())
            && (!self.resize_capturing() || self.paste_offer.is_some())
            && !self.options.read_only
            && self.options.clipboard_policy != "deny"
        {
            self.paste_reply(response)?;
        }
        if let Some(consent) = self.consent.as_mut().filter(|consent| consent.state.sent) {
            let native_id = std::mem::take(&mut consent.state.id);
            consent.state.id = format!("a{}g{}_{}", self.namespace.0, self.namespace.1, native_id);
            let answer = clipboard_packet(&mut consent.state, response);
            consent.state.id = native_id;
            match answer {
                Ok(Some(answer)) => self.finish_consent(answer)?,
                Ok(None) => {}
                Err(error) => {
                    if consent.state.paste {
                        let consent = self.consent.take().unwrap();
                        return Err(self.fail_paste(&consent.pane, &consent.view, error, 0));
                    }
                    self.finish_consent(ClipboardResponse::denied())?;
                }
            }
        }
        Ok(())
    }

    fn expire_devices(&mut self) -> Result<()> {
        if self
            .paste_offer
            .as_ref()
            .is_some_and(|offer| offer.started.elapsed() > Duration::from_secs(4))
        {
            let result = self.abort_listing("enhanced paste listing timed out");
            self.scope_input_result(result)?;
        }
        if self
            .consent
            .as_ref()
            .is_some_and(|consent| consent.state.started.elapsed() > Duration::from_secs(4))
        {
            let result = self.finish_consent(ClipboardResponse::denied());
            self.scope_input_result(result)?;
        }
        Ok(())
    }

    pub(super) fn session_navigation_allowed(&self) -> bool {
        self.state.form.is_none()
            && !self.picker_open()
            && self
                .consent
                .as_ref()
                .is_none_or(|consent| consent.state.sent)
            && self.pointer_capture.is_none()
            && !self.resize_capturing()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{attach_fixture, fixture, key, size};
    use super::*;

    fn coordinator_fixture() -> (Coordinator, [updates::Capture; 2]) {
        let (mut first, first_input) = fixture();
        let (mut second, second_input) = fixture();
        attach_fixture(&mut first);
        attach_fixture(&mut second);
        second.backgrounded = true;
        second.host_focused = false;
        let targets = (0..2)
            .map(|index| {
                let mut session = pending_session(Instant::now());
                let SessionTarget::Remote(target) = &mut session.target else {
                    unreachable!();
                };
                target.session_name = format!("session-{index}");
                session.target
            })
            .collect();
        let mut coordinator = Coordinator::new(targets, first.options.clone(), size()).unwrap();
        for (index, mut app) in [first, second].into_iter().enumerate() {
            app.namespace = (index as u64 + 1, 1);
            let session = &mut coordinator.sessions[index];
            session.generation = 1;
            session.phase = Phase::Online;
            session.next_attempt = None;
            let mut registry = Pump::capture();
            registry.cancel();
            session.registry = Some(registry);
            session.app = Some(app);
        }
        (coordinator, [first_input, second_input])
    }

    fn take_operations(capture: &updates::Capture) -> Vec<Operation> {
        std::iter::from_fn(|| capture.try_recv().ok()).collect()
    }

    fn physical_key(action: KeyAction) -> ClientEvent {
        let action = match action {
            KeyAction::Press => 1,
            KeyAction::Repeat => 2,
            KeyAction::Release => 3,
        };
        ClientEvent::Reply(format!("key;97;1:{action}").into_bytes())
    }

    #[test]
    fn in_host_session_switch_keeps_held_key_until_original_hardware_release() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        assert!(matches!(first_input.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Key { event, .. }, ..
        } if event.action == KeyAction::Press));

        coordinator.select(1, size()).unwrap();
        assert!(
            coordinator.sessions[0]
                .app
                .as_ref()
                .unwrap()
                .held
                .contains_key("KeyA")
        );
        assert!(take_operations(&first_input).iter().all(|operation| {
            !matches!(operation, Operation::Pane {
                request: PaneRequest::Key { event, .. }, ..
            } if event.action == KeyAction::Release)
        }));
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Repeat), size())
                .unwrap()
        );
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Release), size())
                .unwrap()
        );
        assert!(matches!(first_input.try_recv().unwrap(), Operation::Pane {
            pane_id, request: PaneRequest::Key { view_id, event },
        } if pane_id == "focused-pane" && view_id == "focused-view"
            && event.key == "KeyA" && event.action == KeyAction::Release));
        assert!(second_input.try_recv().is_err());
    }

    #[test]
    fn physical_focus_loss_releases_inactive_view_and_allows_its_retirement() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        take_operations(&first_input);
        coordinator.select(1, size()).unwrap();
        take_operations(&first_input);
        coordinator
            .app_mut()
            .key(key("KeyB"), &mut io::sink(), size(), &[])
            .unwrap();
        take_operations(&second_input);
        assert!(
            coordinator.sessions[0].app.as_ref().unwrap().views["focused-pane"]
                .retirement_requested
        );

        assert!(
            !coordinator
                .route_release(&ClientEvent::Reply(b"focus;out".to_vec()), size())
                .unwrap()
        );
        for (capture, expected) in [(&first_input, "KeyA"), (&second_input, "KeyB")] {
            assert!(matches!(capture.try_recv().unwrap(), Operation::Pane {
                pane_id, request: PaneRequest::Key { view_id, event },
            } if pane_id == "focused-pane" && view_id == "focused-view"
                && event.key == expected && event.action == KeyAction::Release));
            assert!(capture.try_recv().is_err());
        }
        assert!(
            coordinator
                .sessions
                .iter()
                .all(|session| { session.app.as_ref().unwrap().held.is_empty() })
        );
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Repeat), size())
                .unwrap()
        );
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Release), size())
                .unwrap()
        );
        assert!(second_input.try_recv().is_err());

        coordinator.tick(size()).unwrap();
        assert!(take_operations(&first_input).iter().any(|operation| {
            matches!(operation, Operation::Pane {
                pane_id, request: PaneRequest::Detach { view_id },
            } if pane_id == "focused-pane" && view_id == "focused-view")
        }));
        coordinator.tick(size()).unwrap();
        let original = coordinator.sessions[0].app.as_ref().unwrap();
        assert!(original.views.is_empty());
        assert!(original.retiring.is_empty());
    }

    #[test]
    fn failed_inactive_cleanup_does_not_prevent_another_sessions_key_release() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        take_operations(&first_input);
        coordinator.select(1, size()).unwrap();
        take_operations(&first_input);
        coordinator
            .app_mut()
            .key(key("KeyB"), &mut io::sink(), size(), &[])
            .unwrap();
        take_operations(&second_input);
        drop(first_input);
        assert!(
            !coordinator
                .route_release(&ClientEvent::Reply(b"focus;out".to_vec()), size(),)
                .unwrap()
        );
        assert!(coordinator.sessions[0].app.as_ref().unwrap().held["KeyA"].pending);
        assert!(
            coordinator.sessions[1]
                .app
                .as_ref()
                .unwrap()
                .held
                .is_empty()
        );
        assert!(matches!(second_input.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Key { event, .. }, ..
        } if event.key == "KeyB" && event.action == KeyAction::Release));
    }

    #[test]
    fn inactive_pending_cleanup_retries_without_admitting_a_foreign_repress() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        take_operations(&first_input);
        coordinator.select(1, size()).unwrap();
        take_operations(&first_input);
        let original = coordinator.sessions[0].app.as_ref().unwrap();
        let reservations = std::iter::from_fn(|| {
            original
                .input
                .reserve(Operation::Pane {
                    pane_id: "busy".into(),
                    request: PaneRequest::Key {
                        view_id: "busy-view".into(),
                        event: key("KeyB"),
                    },
                })
                .ok()
        })
        .collect::<Vec<_>>();

        coordinator
            .route_release(&ClientEvent::Reply(b"focus;out".to_vec()), size())
            .unwrap();
        assert!(coordinator.sessions[0].app.as_ref().unwrap().held["KeyA"].pending);
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Release), size())
                .unwrap()
        );
        assert!(coordinator.sessions[0].app.as_ref().unwrap().held["KeyA"].pending);
        assert_eq!(coordinator.release_owners.get("KeyA"), Some(&(0, 1)));
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Press), size())
                .unwrap()
        );
        assert_eq!(coordinator.release_owners.get("KeyA"), Some(&(0, 1)));
        assert!(first_input.try_recv().is_err());
        assert!(second_input.try_recv().is_err());

        drop(reservations);
        coordinator.tick(size()).unwrap();
        let operations = take_operations(&first_input);
        assert!(matches!(&operations[0], Operation::Pane {
            request: PaneRequest::Key { view_id, event }, ..
        } if view_id == "focused-view" && event.key == "KeyA"
            && event.action == KeyAction::Release));
        assert!(
            coordinator.sessions[0]
                .app
                .as_ref()
                .unwrap()
                .held
                .is_empty()
        );
        assert!(
            !coordinator
                .route_release(&physical_key(KeyAction::Press), size())
                .unwrap()
        );
        assert!(!coordinator.release_owners.contains_key("KeyA"));
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        assert!(matches!(second_input.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Key { event, .. }, ..
        } if event.key == "KeyA" && event.action == KeyAction::Press));
    }

    #[test]
    fn focus_loss_preserves_inactive_paste_barrier_and_accepted_key_suffix() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        let app = coordinator.app_mut();
        app.key(key("KeyA"), &mut io::sink(), size(), &[]).unwrap();
        take_operations(&first_input);
        app.paste_reply(b"5522;type=read:status=OK:pw=cHc=;")
            .unwrap();
        app.key(key("KeyB"), &mut io::sink(), size(), &[]).unwrap();
        assert!(first_input.try_recv().is_err());
        coordinator.select(1, size()).unwrap();
        take_operations(&first_input);
        coordinator
            .route_release(&ClientEvent::Reply(b"focus;out".to_vec()), size())
            .unwrap();
        coordinator.tick(size()).unwrap();
        assert!(first_input.try_recv().is_err());
        assert!(
            coordinator.sessions[0]
                .app
                .as_ref()
                .unwrap()
                .held
                .values()
                .all(|held| held.pending)
        );

        assert!(
            coordinator
                .route_device_reply(b"5522;type=read:status=DONE:pw=cHc=;")
                .unwrap()
        );
        assert!(matches!(first_input.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Key { event, .. }, ..
        } if event.key == "KeyB" && event.action == KeyAction::Press));
        assert!(first_input.try_recv().is_err());
        coordinator.tick(size()).unwrap();
        let released = take_operations(&first_input)
            .into_iter()
            .filter_map(|operation| match operation {
                Operation::Pane {
                    pane_id,
                    request: PaneRequest::Key { view_id, event },
                } => {
                    assert_eq!(pane_id, "focused-pane");
                    assert_eq!(view_id, "focused-view");
                    assert!(event.action == KeyAction::Release);
                    Some(event.key)
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(released, HashSet::from(["KeyA".into(), "KeyB".into()]));
        assert!(second_input.try_recv().is_err());
    }

    #[test]
    fn removed_and_replaced_generations_consume_old_repeat_and_release_tombstones() {
        let (mut coordinator, [first_input, second_input]) = coordinator_fixture();
        for name in ["KeyA", "KeyB"] {
            coordinator
                .app_mut()
                .key(key(name), &mut io::sink(), size(), &[])
                .unwrap();
        }
        take_operations(&first_input);
        coordinator.remove(0, size(), false).unwrap();
        assert_eq!(coordinator.selected, 1);
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Repeat), size())
                .unwrap()
        );
        assert!(
            coordinator
                .route_release(&physical_key(KeyAction::Release), size())
                .unwrap()
        );
        assert!(!coordinator.release_owners.contains_key("KeyA"));
        assert!(second_input.try_recv().is_err());

        let (mut replacement, replacement_input) = fixture();
        attach_fixture(&mut replacement);
        let session = &mut coordinator.sessions[0];
        session.present = true;
        session.generation += 1;
        session.app = Some(replacement);
        session.registry = Some(Pump::capture());
        coordinator.select(0, size()).unwrap();
        assert!(
            coordinator
                .route_release(&ClientEvent::Reply(b"key;98;1:2".to_vec()), size())
                .unwrap()
        );
        assert!(
            coordinator
                .route_release(&ClientEvent::Reply(b"key;98;1:3".to_vec()), size())
                .unwrap()
        );
        assert!(!coordinator.release_owners.contains_key("KeyB"));
        assert!(replacement_input.try_recv().is_err());
        assert!(
            !coordinator
                .route_release(&physical_key(KeyAction::Press), size())
                .unwrap()
        );
        coordinator
            .app_mut()
            .key(key("KeyA"), &mut io::sink(), size(), &[])
            .unwrap();
        assert!(
            matches!(replacement_input.try_recv().unwrap(), Operation::Pane {
            request: PaneRequest::Key { event, .. }, ..
        } if event.key == "KeyA" && event.action == KeyAction::Press)
        );
    }

    #[test]
    fn queued_remote_attempts_cannot_consume_the_local_admission_reserve() {
        assert!(!admission_available(6, 6, true));
        assert!(admission_available(6, 6, false));
        assert!(!admission_available(8, 6, false));
        assert!(admission_available(5, 5, true));
    }

    #[test]
    fn retries_are_bounded_and_never_busy() {
        assert_eq!(retry_delay(1), Duration::from_millis(500));
        assert_eq!(retry_delay(2), Duration::from_secs(1));
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(30));
        assert!((1..=100).all(|n| retry_delay(n) >= Duration::from_millis(500)));
    }

    #[test]
    fn logical_session_binding_rejects_replacement_but_accepts_new_incarnation() {
        let first = SessionInfo {
            name: "work".into(),
            session_id: "logical".into(),
            daemon_id: "old".into(),
        };
        let mut candidate = first.clone();
        candidate.daemon_id = "new".into();
        assert!(validate_identity(Some(&first.session_id), &candidate, std::iter::empty()).is_ok());
        candidate.session_id = "replacement".into();
        assert!(
            validate_identity(Some(&first.session_id), &candidate, std::iter::empty()).is_err()
        );
    }

    #[test]
    fn aliases_require_the_same_daemon_owner_not_a_name_or_logical_uuid() {
        let first = SessionInfo {
            name: "work".into(),
            session_id: "logical".into(),
            daemon_id: "one".into(),
        };
        let mut candidate = first.clone();
        candidate.daemon_id = "two".into();
        assert!(validate_identity(None, &candidate, std::iter::once(&first)).is_ok());
        candidate.daemon_id.clone_from(&first.daemon_id);
        candidate.name = "alias".into();
        assert!(validate_identity(None, &candidate, std::iter::once(&first)).is_err());
    }

    fn pending_session(now: Instant) -> Session {
        Session::new(
            SessionTarget::Remote(crate::remote::RemoteTarget {
                destination: "example.invalid".into(),
                session_name: "work".into(),
                helper: None,
                ssh_config: None,
                forward_agent: false,
                detach_others: true,
            }),
            now,
        )
    }

    #[test]
    fn projection_keeps_literal_remote_fields_separate_and_uses_admitted_name() {
        let mut session = pending_session(Instant::now());
        let SessionTarget::Remote(target) = &mut session.target else {
            unreachable!();
        };
        target.destination = "user@[2001:db8::1]".into();
        target.session_name = "literal/session:name".into();
        let empty = EmptyMetadata::new();
        assert!(refresh_presentation(0, &mut session, &empty));
        let group = &session.presentation.as_ref().unwrap().group;
        assert_eq!(group.name, "literal/session:name");
        assert_eq!(group.host.as_deref(), Some("user@[2001:db8::1]"));

        session.identity = Some(SessionInfo {
            name: "authoritative session".into(),
            session_id: "logical".into(),
            daemon_id: "daemon".into(),
        });
        assert!(refresh_presentation(0, &mut session, &empty));
        assert_eq!(
            session.presentation.as_ref().unwrap().group.name,
            "authoritative session"
        );
        session.generation += 1;
        session.phase = Phase::Offline;
        assert!(refresh_presentation(0, &mut session, &empty));
        let group = &session.presentation.as_ref().unwrap().group;
        assert_eq!(group.name, "authoritative session");
        assert_eq!(group.host.as_deref(), Some("user@[2001:db8::1]"));
    }

    #[test]
    fn failed_initial_activation_is_manual_even_after_one_shot_admission_was_consumed() {
        let now = Instant::now();
        let mut session = pending_session(now);
        session.initial_activation = false;
        session.phase = Phase::Activating;
        session.cancellation.cancel();
        session.record_failure(anyhow::anyhow!("helper is missing"), now);
        assert!(session.phase == Phase::Offline);
        assert!(session.next_attempt.is_none());
        assert_eq!(session.last_error.as_deref(), Some("helper is missing"));
        assert!(session.status.contains("Press R"));
        assert!(session.present);
    }

    #[test]
    fn established_loss_retries_but_replacement_rejection_stays_pinned_and_manual() {
        let now = Instant::now();
        let mut session = pending_session(now);
        session.expected_session = Some("pinned".into());
        session.ever_active = true;
        session.record_failure(anyhow::anyhow!("transport lost"), now);
        assert_eq!(session.next_attempt, Some(now + Duration::from_millis(500)));
        session.record_failure(IdentityRejection::Replacement.into(), now);
        assert!(session.phase == Phase::Rejected);
        assert!(session.next_attempt.is_none());
        assert_eq!(session.expected_session.as_deref(), Some("pinned"));
        assert!(session.present);
    }

    #[test]
    fn force_reconnect_invalidates_old_generation_before_admission_without_resetting_policy() {
        let now = Instant::now();
        let mut session = pending_session(now);
        session.generation = 8;
        session.initial_activation = false;
        session.expected_session = Some("pinned".into());
        session.record_failure(anyhow::anyhow!("old failure"), now);
        let old = session.cancellation.clone();
        session.queue_reconnect(now).unwrap();
        assert!(old.is_cancelled());
        assert_eq!(session.generation, 9);
        assert!(!session.cancellation.is_cancelled());
        assert!(session.phase == Phase::Queued);
        assert_eq!(session.next_attempt, Some(now));
        assert!(session.status.contains("Previous failure: old failure"));
        assert!(!session.initial_activation);
        assert!(!session.ever_active);
        assert_eq!(session.expected_session.as_deref(), Some("pinned"));
    }

    #[test]
    fn initial_empty_headless_is_not_a_last_close_transition() {
        let mut membership = Membership::default();
        let mut registry = empty_registry();
        assert!(!membership.observe(&registry));
        registry.revision += 1;
        assert!(!membership.observe(&registry));
        registry.workspaces.push(Workspace {
            id: "workspace".into(),
            name: "workspace".into(),
            revision: 1,
            windows: vec![Window {
                id: "window".into(),
                name: "window".into(),
                revision: 1,
                layout: None,
            }],
        });
        assert!(!membership.observe(&registry));
        registry.workspaces[0].windows.clear();
        assert!(membership.observe(&registry));
        assert!(!membership.observe(&registry));
        registry.workspaces.clear();
        assert!(membership.observe(&registry));
    }

    #[test]
    fn retained_execution_metadata_alone_never_declares_semantic_session_end() {
        let mut membership = Membership::default();
        let mut registry = empty_registry();
        registry.panes.push(PaneInfo {
            id: "pane".into(),
            workspace_id: None,
            window_id: None,
            title: String::new(),
            status: PaneStatus::Exited,
            exit_code: Some(0),
            cwd: PathBuf::new(),
            foreground_cwd: None,
            harness: None,
            harness_status: None,
            project: None,
            home: PathBuf::new(),
            isolated_environment: false,
        });
        assert!(!membership.observe(&registry));
        registry.panes[0].status = PaneStatus::Stopped;
        assert!(!membership.observe(&registry));
        registry.panes[0].status = PaneStatus::Running;
        assert!(!membership.observe(&registry));
        registry.panes[0].status = PaneStatus::Exited;
        assert!(!membership.observe(&registry));
        registry.panes.clear();
        assert!(membership.observe(&registry));
    }

    #[test]
    fn clipboard_reply_ids_bind_attachment_and_attempt_without_parsing_native_ids() {
        assert_eq!(physical_origin("a2g17_native_g9_other"), Some((1, 17)));
        assert_eq!(physical_origin("a0g17_native"), None);
        assert_eq!(physical_origin("native-backend-id"), None);
        assert_eq!(physical_origin("a2gwrong_native"), None);
    }
}
